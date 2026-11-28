//! Explicit, foreground-only handoff to this executable via Windows UAC.
//! The child acknowledges the exact live parent and waits for its orderly exit
//! before acquiring Tauri's single-instance guard. No background registration.
use super::win::{self, Apartment, Outcome};
use std::{
    ffi::{OsStr, OsString},
    os::windows::io::{FromRawHandle, OwnedHandle},
    sync::atomic::{AtomicBool, Ordering},
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{
            GetLastError, SetLastError, ERROR_ALREADY_EXISTS, ERROR_CANCELLED, ERROR_SUCCESS, HWND,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Storage::FileSystem::SYNCHRONIZE,
        System::Threading::{
            CreateEventW, GetCurrentProcess, OpenEventW, OpenProcess, SetEvent,
            WaitForMultipleObjects, WaitForSingleObject, EVENT_MODIFY_STATE, PROCESS_ACCESS_RIGHTS,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::{
            Shell::{
                ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
                SHELLEXECUTEINFOW,
            },
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
};

const FLAG: &str = "--openrevo-hardware-handoff";
const READY_TIMEOUT_MS: u32 = 10_000;
const EXIT_TIMEOUT_MS: u32 = 20_000;
static PENDING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Handoff {
    pid: u32,
    created: u64,
}

impl Handoff {
    fn current() -> Outcome<Self> {
        Ok(Self {
            pid: std::process::id(),
            created: win::creation(unsafe { GetCurrentProcess() })?,
        })
    }

    fn event_name(self) -> Vec<u16> {
        win::wide(format!(
            r"Local\OpenRevo.hardware.ready.{}.{}",
            self.pid, self.created
        ))
    }

    fn arguments(self) -> Vec<u16> {
        win::wide(format!("{FLAG} {} {}", self.pid, self.created))
    }
}

fn parse(args: &[OsString]) -> Outcome<Option<Handoff>> {
    if !args.iter().any(|s| s.eq_ignore_ascii_case(FLAG)) {
        return Ok(None);
    }
    let [flag, pid, created] = args else {
        return Err("硬件面板切换参数不完整。".into());
    };
    if flag != OsStr::new(FLAG) {
        return Err("硬件面板切换参数无效。".into());
    }
    let pid = pid
        .to_str()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v != 0);
    let created = created
        .to_str()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v != 0);
    match (pid, created) {
        (Some(pid), Some(created)) => Ok(Some(Handoff { pid, created })),
        _ => Err("硬件面板切换参数无效。".into()),
    }
}

fn start(owner: usize) -> Outcome<()> {
    win::verify_account()?;
    let _apartment = Apartment::new()?;
    let tag = Handoff::current()?;
    let event_name = tag.event_name();
    let exe = std::env::current_exe().map_err(win::error)?;
    let directory = exe.parent().ok_or("找不到 OpenRevo 的运行目录。")?;
    let file = win::wide(exe.as_os_str());
    let working = win::wide(directory.as_os_str());
    let args = tag.arguments();
    unsafe {
        SetLastError(ERROR_SUCCESS);
        let ready =
            CreateEventW(None, true, false, PCWSTR(event_name.as_ptr())).map_err(win::error)?;
        let existed = GetLastError() == ERROR_ALREADY_EXISTS;
        let _event = OwnedHandle::from_raw_handle(ready.0);
        if existed {
            return Err("另一次会话切换正在进行。".into());
        }
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            hwnd: HWND(owner as _),
            lpVerb: w!("runas"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(args.as_ptr()),
            lpDirectory: PCWSTR(working.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        if let Err(error) = ShellExecuteExW(&mut info) {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                return Err("已取消 Windows 管理员确认，管理中心保持打开。".into());
            }
            return Err(format!("Windows 未启动硬件会话：{error}"));
        }
        if info.hProcess.is_invalid() {
            return Err("Windows 没有返回硬件会话进程，管理中心保持打开。".into());
        }
        let _child = OwnedHandle::from_raw_handle(info.hProcess.0);
        if WaitForMultipleObjects(&[ready, info.hProcess], false, READY_TIMEOUT_MS) != WAIT_OBJECT_0
            || WaitForSingleObject(info.hProcess, 0) != WAIT_TIMEOUT
        {
            return Err(
                "硬件会话未确认就绪；保持管理中心，且未切换。管理员账户须与当前桌面账户一致。"
                    .into(),
            );
        }
    }
    Ok(())
}

pub(super) fn launch(owner: usize) -> Outcome<()> {
    if PENDING.swap(true, Ordering::AcqRel) {
        return Err("硬件面板正在打开，请等待 Windows 确认。".into());
    }
    let result = start(owner);
    if result.is_err() {
        PENDING.store(false, Ordering::Release);
    }
    result
}

pub(crate) fn wait_for_parent(args: &[OsString]) -> Outcome<()> {
    let Some(tag) = parse(args)? else {
        return Ok(());
    };
    if tag.pid == std::process::id() {
        return Err("硬件面板切换来源无效。".into());
    }
    unsafe {
        let parent = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_ACCESS_RIGHTS(SYNCHRONIZE.0),
            false,
            tag.pid,
        )
        .map_err(win::error)?;
        let _parent = OwnedHandle::from_raw_handle(parent.0);
        let image = win::process_image(parent).ok_or("读不到切换来源的程序身份。")?;
        let current = win::process_image(GetCurrentProcess()).ok_or("读不到当前程序身份。")?;
        if !image.as_os_str().eq_ignore_ascii_case(current.as_os_str())
            || win::creation(parent)? != tag.created
        {
            return Err("硬件面板切换来源已变化。".into());
        }
        let name = tag.event_name();
        let event =
            OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())).map_err(win::error)?;
        let _event = OwnedHandle::from_raw_handle(event.0);
        SetEvent(event).map_err(win::error)?;
        if WaitForSingleObject(parent, EXIT_TIMEOUT_MS) != WAIT_OBJECT_0 {
            return Err("原管理中心仍在运行，已停止切换。".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn handoff_parser_never_accepts_extra_flags_or_missing_identity() {
        assert_eq!(parse(&args(&[])).unwrap(), None);
        assert_eq!(parse(&args(&["--visible"])).unwrap(), None);
        assert_eq!(
            parse(&args(&[FLAG, "15", "234"])).unwrap(),
            Some(Handoff {
                pid: 15,
                created: 234
            })
        );
        for invalid in [
            vec![FLAG, "0", "99"],
            vec![FLAG, "84", "0"],
            vec![FLAG, "84"],
            vec![FLAG, "84", "50", "--minimized"],
            vec![FLAG, "cmd.exe", "50"],
        ] {
            assert!(parse(&args(&invalid)).is_err());
        }
    }

    #[test]
    fn handoff_waits_for_same_image_parent_to_exit() {
        const STAGE: &str = "REVO_HANDOFF_UNIT_STAGE";
        const CASE: &str =
            "system::manager::handoff::tests::handoff_waits_for_same_image_parent_to_exit";
        let worker = || {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", CASE, "--nocapture"]);
            command
        };
        match std::env::var(STAGE).as_deref() {
            Ok("child") => {
                wait_for_parent(&[
                    FLAG.into(),
                    std::env::var_os("REVO_HANDOFF_UNIT_PID").unwrap(),
                    std::env::var_os("REVO_HANDOFF_UNIT_CREATED").unwrap(),
                ])
                .unwrap();
                println!("CHILD_AFTER_PARENT_EXIT");
            }
            Ok("parent") => unsafe {
                let tag = Handoff::current().unwrap();
                let name = tag.event_name();
                let ready = CreateEventW(None, true, false, PCWSTR(name.as_ptr())).unwrap();
                let _event = OwnedHandle::from_raw_handle(ready.0);
                let mut child = worker()
                    .env(STAGE, "child")
                    .env("REVO_HANDOFF_UNIT_PID", tag.pid.to_string())
                    .env("REVO_HANDOFF_UNIT_CREATED", tag.created.to_string())
                    .spawn()
                    .unwrap();
                assert_eq!(WaitForSingleObject(ready, READY_TIMEOUT_MS), WAIT_OBJECT_0);
                assert!(child.try_wait().unwrap().is_none());
                println!("PARENT_ACKNOWLEDGED_BEFORE_EXIT");
            },
            _ => {
                let result = worker().env(STAGE, "parent").output().unwrap();
                let out = String::from_utf8_lossy(&result.stdout);
                let start = out.find("PARENT_ACKNOWLEDGED_BEFORE_EXIT");
                let end = out.find("CHILD_AFTER_PARENT_EXIT");
                assert!(
                    result.status.success() && start.zip(end).is_some_and(|(a, b)| a < b),
                    "{out}\n{}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
        }
    }
}
