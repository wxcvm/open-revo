use super::win;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use windows::{
    core::PCWSTR,
    Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS},
};
use winreg::{enums::*, transaction::Transaction, RegKey, RegValue};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const MAX: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Payload {
    Run { bytes: Vec<u8>, expand: bool },
    Shortcut { stamp: win::Stamp },
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct Entry {
    pub name: String,
    payload: Payload,
}
impl Entry {
    pub fn source(&self) -> &'static str {
        match self.payload {
            Payload::Run { .. } => "user-run",
            Payload::Shortcut { .. } => "user-folder",
        }
    }
    fn slot(&self) -> String {
        id_for(self.source(), &self.name)
    }
}

#[derive(Serialize)]
pub(super) struct Row {
    id: Option<String>,
    name: String,
    source: &'static str,
    detail: String,
    state: &'static str,
    action: Option<&'static str>,
}
#[derive(Serialize)]
pub(crate) struct List {
    rows: Vec<Row>,
    warnings: Vec<String>,
}

fn id_for(scope: &str, name: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{scope}/{}", name.to_uppercase()).as_bytes())
    )
}
fn normalized_name(name: &str) -> bool {
    !name.is_empty() && name.encode_utf16().count() <= 260 && !name.chars().any(|c| c.is_control())
}
fn shortcut_name(name: &str) -> bool {
    normalized_name(name)
        && !name.contains(['\\', '/', ':'])
        && !name.ends_with([' ', '.'])
        && Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
}
fn text(bytes: &[u8]) -> String {
    String::from_utf16_lossy(
        &bytes
            .chunks_exact(2)
            .map(|a| u16::from_le_bytes([a[0], a[1]]))
            .take_while(|p| *p != 0)
            .collect::<Vec<_>>(),
    )
}
fn protected(s: &str) -> bool {
    let t = s.to_lowercase();
    [
        "openrevo",
        "open-revo",
        "artex",
        "windows defender",
        "securityhealth",
        "wsl.exe",
    ]
    .iter()
    .any(|p| t.contains(p))
}
fn as_payload(v: RegValue) -> Option<Payload> {
    (matches!(v.vtype, REG_SZ | REG_EXPAND_SZ) && v.bytes.len() <= 16_384 && v.bytes.len() % 2 == 0)
        .then(|| Payload::Run {
            bytes: v.bytes,
            expand: v.vtype == REG_EXPAND_SZ,
        })
}
fn entry_valid(e: &Entry) -> bool {
    normalized_name(&e.name)
        && !protected(&e.name)
        && match &e.payload {
            Payload::Run { bytes, .. } => {
                bytes.len() <= 16_384 && bytes.len() % 2 == 0 && !protected(&text(bytes))
            }
            Payload::Shortcut { .. } => shortcut_name(&e.name),
        }
}
fn backup_path(entry: &Entry, ext: &str) -> win::Outcome<PathBuf> {
    Ok(win::state_dir()?.join(format!("startup-{}.{ext}", entry.slot())))
}
fn read_backup(path: &Path) -> win::Outcome<Entry> {
    win::checked_path(path)?;
    if fs::metadata(path).map_err(win::error)?.len() > 100_000 {
        return Err("自启备份超过大小限制".into());
    }
    let e: Entry =
        serde_json::from_slice(&fs::read(path).map_err(win::error)?).map_err(win::error)?;
    if !entry_valid(&e)
        || path.file_name().and_then(|s| s.to_str()) != Some(&format!("startup-{}.json", e.slot()))
    {
        return Err("自启备份记录不匹配".into());
    }
    Ok(e)
}
fn backup(e: &Entry) -> win::Outcome<()> {
    let path = backup_path(e, "json")?;
    // No overwrite of an existing, possibly still needed recovery point.
    match fs::symlink_metadata(&path) {
        Ok(_) if read_backup(&path).ok().as_ref() == Some(e) => return Ok(()),
        Ok(_) => return Err("已有同名自启备份；保留旧记录".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(win::error(e)),
    }
    let _pins = win::pin_parents(&path)?;
    let bytes = serde_json::to_vec(e).map_err(win::error)?;
    let temp = path.with_extension(format!("stage-{}", super::serial()));
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(win::error)?;
    if let Err(e) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(win::error(e));
    }
    drop(file);
    if let Err(e) = move_exclusive(&temp, &path) {
        let _ = fs::remove_file(temp);
        return Err(e);
    }
    Ok(())
}
fn move_exclusive(from: &Path, to: &Path) -> win::Outcome<()> {
    // No REPLACE_EXISTING, COPY_ALLOWED or delayed/reboot behavior.
    unsafe {
        MoveFileExW(
            PCWSTR(win::wide(from.as_os_str()).as_ptr()),
            PCWSTR(win::wide(to.as_os_str()).as_ptr()),
            MOVE_FILE_FLAGS(0),
        )
        .map_err(win::error)
    }
}

fn active() -> (Vec<Entry>, Vec<Row>, Vec<String>) {
    let mut entries = Vec::new();
    let mut other = Vec::new();
    let mut warnings = Vec::new();
    match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN, KEY_READ) {
        Ok(run) => {
            for row in run.enum_values().take(MAX) {
                match row {
                    Ok((name, value)) => {
                        let detail = text(&value.bytes);
                        if let Some(payload) = as_payload(value) {
                            entries.push(Entry { name, payload });
                        } else {
                            other.push(Row {
                                id: None,
                                name,
                                source: "user-run",
                                detail,
                                state: "registered",
                                action: None,
                            });
                        }
                    }
                    Err(e) => warnings.push(win::error(e)),
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => warnings.push(win::error(e)),
    }
    match win::startup_dir()
        .and_then(|p| win::checked_path(&p))
        .and_then(|p| fs::read_dir(p).map_err(win::error))
    {
        Ok(r) => {
            for child in r.flatten().take(MAX) {
                let name = child.file_name().to_string_lossy().into_owned();
                let path = child.path();
                if name.eq_ignore_ascii_case("desktop.ini") {
                    continue;
                }
                let item =
                    if shortcut_name(&name) && child.file_name() == std::ffi::OsStr::new(&name) {
                        win::open_file(&path, false, 1 | 2 | 4)
                            .ok()
                            .and_then(|h| win::file_info(&h).ok())
                    } else {
                        None
                    };
                if let Some((stamp, _, 1)) = item {
                    entries.push(Entry {
                        name,
                        payload: Payload::Shortcut { stamp },
                    });
                } else {
                    other.push(Row {
                        id: None,
                        name,
                        source: "user-folder",
                        detail: path.to_string_lossy().into_owned(),
                        state: "registered",
                        action: None,
                    });
                }
            }
        }
        Err(e) => warnings.push(format!("启动文件夹：{e}")),
    }
    (entries, other, warnings)
}

fn backups() -> (Vec<Entry>, Vec<String>) {
    let mut rows = Vec::new();
    let mut notes = Vec::new();
    let dir = win::appdata().map(|p| p.join("OpenRevoManager"));
    let result = dir.and_then(|p| match fs::symlink_metadata(&p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(win::error(e)),
        Ok(_) => fs::read_dir(win::checked_path(&p)?)
            .map(Some)
            .map_err(win::error),
    });
    match result {
        Ok(Some(list)) => {
            for p in list
                .flatten()
                .filter(|r| {
                    r.file_name()
                        .to_str()
                        .is_some_and(|s| s.starts_with("startup-") && s.ends_with(".json"))
                })
                .take(MAX)
            {
                match read_backup(&p.path()) {
                    Ok(row) => rows.push(row),
                    Err(e) => notes.push(e),
                }
            }
        }
        Ok(None) => (),
        Err(e) => notes.push(e),
    }
    (rows, notes)
}
fn description(entry: &Entry) -> String {
    match &entry.payload {
        Payload::Run { bytes, .. } => text(bytes),
        Payload::Shortcut { .. } => win::startup_dir()
            .map(|p| p.join(&entry.name).to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}
pub(super) fn list() -> win::Outcome<List> {
    let (entries, mut rows, mut warnings) = active();
    let (copies, extra) = backups();
    warnings.extend(extra);
    for e in &entries {
        let source = e.source();
        let detail = description(e);
        let allowed = entry_valid(e);
        rows.push(Row {
            id: allowed.then(|| e.slot()),
            name: e.name.clone(),
            source,
            detail,
            state: "registered",
            action: allowed.then_some("disable"),
        });
    }
    for e in copies {
        let exists = entries.iter().any(|row| row.slot() == e.slot());
        let source = e.source();
        if !exists || !entries.contains(&e) {
            rows.push(Row {
                id: (!exists).then(|| e.slot()),
                name: e.name.clone(),
                source,
                detail: description(&e),
                state: if exists { "conflict" } else { "backup" },
                action: (!exists).then_some("restore"),
            });
        }
    }
    rows.sort_by(|a, b| (a.source, &a.name).cmp(&(b.source, &b.name)));
    Ok(List { rows, warnings })
}

pub(super) fn lookup(id: &str, restore: bool) -> win::Outcome<Entry> {
    let set = if restore { backups().0 } else { active().0 };
    set.into_iter()
        .find(|e| e.slot() == id && entry_valid(e))
        .ok_or_else(|| "自启项目已变化，或不在当前用户可操作范围".into())
}

pub(super) fn change(e: &Entry, restore: bool) -> win::Outcome<&'static str> {
    if !entry_valid(e) {
        return Err("此启动项只读".into());
    }
    let current = lookup(&e.slot(), restore)?;
    if current != *e {
        return Err("自启项目自预览后已变化".into());
    }
    if !restore {
        backup(e)?;
    }
    match &e.payload {
        Payload::Run { bytes, expand } => {
            let root = RegKey::predef(HKEY_CURRENT_USER);
            let tx = Transaction::new().map_err(win::error)?;
            let key = if restore {
                root.create_subkey_transacted_with_flags(RUN, &tx, KEY_READ | KEY_WRITE)
                    .map_err(win::error)?
                    .0
            } else {
                root.open_subkey_transacted_with_flags(RUN, &tx, KEY_READ | KEY_WRITE)
                    .map_err(win::error)?
            };
            let expected = RegValue {
                bytes: bytes.clone(),
                vtype: if *expand { REG_EXPAND_SZ } else { REG_SZ },
            };
            let observed = key.get_raw_value(&e.name);
            if restore {
                if !matches!(observed, Err(ref error) if error.kind() == io::ErrorKind::NotFound) {
                    return Err("已存在同名启动值，保留备份且不覆盖".into());
                }
                key.set_raw_value(&e.name, &expected).map_err(win::error)?;
            } else {
                if observed.ok().as_ref() != Some(&expected) {
                    return Err("启动值已变化".into());
                }
                key.delete_value(&e.name).map_err(win::error)?;
            }
            drop(key);
            tx.commit().map_err(win::error)?;
            let observed = root
                .open_subkey_with_flags(RUN, KEY_READ)
                .and_then(|key| key.get_raw_value(&e.name));
            let matches = if restore {
                observed.ok().as_ref() == Some(&expected)
            } else {
                matches!(observed, Err(error) if error.kind() == io::ErrorKind::NotFound)
            };
            if !matches {
                return Err("Windows 注册表回读与目标不一致，保留备份".into());
            }
        }
        Payload::Shortcut { stamp } => {
            let source = win::checked_path(&win::startup_dir()?)?.join(&e.name);
            let stored = backup_path(e, "lnk")?;
            let (from, to) = if restore {
                (&stored, &source)
            } else {
                (&source, &stored)
            };
            let _source_pins = win::pin_parents(from)?;
            let _destination_pins = win::pin_parents(to)?;
            win::checked_path(from)?;
            let f = win::open_file(from, false, 1 | 4).map_err(win::error)?;
            let (current, _, links) = win::file_info(&f)?;
            if current != *stamp || links != 1 {
                return Err("快捷方式身份已变化".into());
            }
            move_exclusive(from, to)?;
            let seen =
                win::file_info(&win::open_file(to, false, 1 | 2 | 4).map_err(win::error)?)?.0;
            if seen != *stamp
                || !matches!(fs::symlink_metadata(from), Err(error) if error.kind() == io::ErrorKind::NotFound)
            {
                return Err("快捷方式移动后身份不一致，未标记成功".into());
            }
        }
    }
    if restore {
        let path = backup_path(e, "json")?;
        // Delete only our own recovery manifest after verifying the exact restored item.
        if read_backup(&path).ok().as_ref() == Some(e) {
            if let Err(error) = fs::remove_file(path) {
                crate::log_warn!(
                    "MANAGER",
                    "Restored startup entry; recovery manifest retained: {error}"
                );
            }
        }
    }
    Ok(if restore {
        "已恢复 Windows 注册记录；Windows 自身禁止状态未改动"
    } else {
        "已取消启动注册并核对，保留恢复记录"
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protected_and_invalid_startup_names_are_not_mutable() {
        assert!(!shortcut_name("../escape.lnk"));
        assert!(!shortcut_name("nested\\escape.lnk"));
        assert!(shortcut_name("ok.LNK"));
        assert!(protected("artex-startup"));
        assert_eq!(id_for("user-run", "ApP"), id_for("user-run", "app"));
        assert_ne!(id_for("user-run", "App"), id_for("user-folder", "App"));
    }

    #[test]
    #[ignore = "interactive Windows; creates, disables, and restores one inert temporary Run value"]
    fn manager_live_startup_roundtrip() {
        win::as_user(|| {
            let name = format!(
                "RevoManagerFixture{}_{}",
                std::process::id(),
                super::super::now_ms()
            );
            let key = RegKey::predef(HKEY_CURRENT_USER)
                .open_subkey_with_flags(RUN, KEY_READ | KEY_WRITE)
                .map_err(win::error)?;
            let original = r"Z:\OpenRevo_Validation_No_Executable\fixture-unused.exe";
            // Unique fixture only. Its command is an inert nonexistent path and registration is removed in Drop.
            struct Fixture<'a> {
                key: &'a RegKey,
                name: &'a str,
            }
            impl Drop for Fixture<'_> {
                fn drop(&mut self) {
                    let _ = self.key.delete_value(self.name);
                }
            }
            if key.get_raw_value(&name).is_ok() {
                return Err("test Run fixture already exists".into());
            }
            let original = original.replace("OpenRevo_Validation", "RevoProbe");
            key.set_value(&name, &original).map_err(win::error)?;
            let _cleanup = Fixture {
                key: &key,
                name: &name,
            };
            let id = id_for("user-run", &name);
            let entry = lookup(&id, false)?;
            change(&entry, false)?;
            assert_eq!(
                key.get_raw_value(&name).unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
            // Deliberate collision verifies restore does not overwrite an existing registration.
            let other = r"Z:\RevoProbe\new-fixture-unused.exe";
            key.set_value(&name, &other).map_err(win::error)?;
            assert!(change(&entry, true).is_err());
            assert_eq!(
                key.get_value::<String, _>(&name).map_err(win::error)?,
                other
            );
            key.delete_value(&name).map_err(win::error)?;
            change(&lookup(&id, true)?, true)?;
            assert_eq!(
                key.get_value::<String, _>(&name).map_err(win::error)?,
                original
            );
            Ok(())
        })
        .unwrap();
    }
}
