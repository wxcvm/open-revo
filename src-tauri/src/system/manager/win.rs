use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata, OpenOptions},
    io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Component, Path, PathBuf, Prefix},
    sync::OnceLock,
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{CloseHandle, ERROR_CANCELLED, FILETIME, HANDLE, HWND, LPARAM, WPARAM},
        Security::{
            DuplicateTokenEx, EqualSid, GetTokenInformation, ImpersonateLoggedOnUser, RevertToSelf,
            SecurityImpersonation, TokenElevation, TokenImpersonation, TokenUser, TOKEN_DUPLICATE,
            TOKEN_ELEVATION, TOKEN_IMPERSONATE, TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::{
            FileCompressionInfo, FileIdInfo, FileStandardInfo, GetDiskFreeSpaceExW, GetDriveTypeW,
            GetFileInformationByHandleEx, GetLogicalDrives, FILE_COMPRESSION_INFO,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
            FILE_STANDARD_INFO,
        },
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
            },
            SystemInformation::GetSystemDirectoryW,
            Threading::{
                GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
                QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
        UI::{
            Shell::{
                BHID_Transfer, Common::ITEMIDLIST, FOLDERID_LocalAppData,
                FOLDERID_RecycleBinFolder, FOLDERID_Startup, FileOpenDialog, IFileOpenDialog,
                IShellItem, ITransferSource, SHCreateItemFromParsingName, SHGetKnownFolderItem,
                SHGetKnownFolderPath, SHOpenFolderAndSelectItems, SHParseDisplayName,
                ShellExecuteW, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST,
                FOS_PICKFOLDERS, KF_FLAG_DEFAULT, SIGDN_FILESYSPATH, TSF_NORMAL,
            },
            WindowsAndMessaging::{
                EnumWindows, GetShellWindow, GetWindowThreadProcessId, IsWindowVisible,
                MessageBoxW, PostMessageW, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO,
                SW_SHOWNORMAL, WM_CLOSE,
            },
        },
    },
};

pub(super) type Outcome<T> = Result<T, String>;
pub(super) fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub(super) fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
pub(super) fn filetime(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
pub(super) struct Apartment;
impl Apartment {
    pub(super) fn new() -> Outcome<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(error)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

fn token_for_process(process: HANDLE, duplicate: bool) -> Outcome<Handle> {
    unsafe {
        let mut result = HANDLE::default();
        let access = if duplicate {
            TOKEN_QUERY | TOKEN_DUPLICATE
        } else {
            TOKEN_QUERY
        };
        OpenProcessToken(process, access, &mut result).map_err(error)?;
        Ok(Handle(result))
    }
}
fn sid(token: HANDLE) -> Outcome<Vec<usize>> {
    unsafe {
        let mut len = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        if !(std::mem::size_of::<TOKEN_USER>() as u32..=65536).contains(&len) {
            return Err("无法读取 Windows 用户标识".into());
        }
        let mut buffer = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            len,
            &mut len,
        )
        .map_err(error)?;
        Ok(buffer)
    }
}
fn same_account(a: HANDLE, b: HANDLE) -> Outcome<()> {
    let a = sid(a)?;
    let b = sid(b)?;
    unsafe {
        let a = &*a.as_ptr().cast::<TOKEN_USER>();
        let b = &*b.as_ptr().cast::<TOKEN_USER>();
        EqualSid(a.User.Sid, b.User.Sid).map_err(|_| "OpenRevo 与当前桌面不是同一登录用户".into())
    }
}

fn desktop_token() -> Outcome<Handle> {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(GetShellWindow(), Some(&mut pid));
        if pid == 0 {
            return Err("找不到当前 Windows 桌面会话".into());
        }
        let process =
            Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(error)?);
        let desktop = token_for_process(process.0, true)?;
        same_account(desktop.0, token_for_process(GetCurrentProcess(), false)?.0)?;
        // A task executing as an alternate administrator must not inspect or alter the actual desktop user's state.
        let mut copy = HANDLE::default();
        DuplicateTokenEx(
            desktop.0,
            TOKEN_QUERY | TOKEN_IMPERSONATE,
            None,
            SecurityImpersonation,
            TokenImpersonation,
            &mut copy,
        )
        .map_err(error)?;
        Ok(Handle(copy))
    }
}

pub(super) fn verify_account() -> Outcome<()> {
    desktop_token().map(|_| ())
}

pub(super) fn as_user<T>(f: impl FnOnce() -> Outcome<T>) -> Outcome<T> {
    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            if unsafe { RevertToSelf() }.is_err() {
                // A pooled worker may not be returned with somebody else's access token.
                std::process::abort();
            }
        }
    }
    let token = desktop_token()?;
    unsafe {
        let process = token_for_process(GetCurrentProcess(), false)?;
        let mut elev = TOKEN_ELEVATION::default();
        let mut needed = 0;
        GetTokenInformation(
            process.0,
            TokenElevation,
            Some((&mut elev as *mut TOKEN_ELEVATION).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut needed,
        )
        .map_err(error)?;
        if elev.TokenIsElevated == 0 {
            return f();
        }
        ImpersonateLoggedOnUser(token.0).map_err(|e| format!("切换到标准用户文件权限：{e}"))?;
    }
    let _guard = Revert;
    f()
}

pub(super) fn known_folder(id: &windows::core::GUID) -> Outcome<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, HANDLE::default()).map_err(error)?;
        let out = PathBuf::from(OsString::from_wide(p.as_wide()));
        CoTaskMemFree(Some(p.0.cast()));
        Ok(out)
    }
}
pub(super) fn appdata() -> Outcome<PathBuf> {
    known_folder(&FOLDERID_LocalAppData)
}
pub(super) fn startup_dir() -> Outcome<PathBuf> {
    known_folder(&FOLDERID_Startup)
}

pub(super) fn system_dir() -> Outcome<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let len = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    if len == 0 || len >= buf.len() {
        return Err("找不到 Windows 系统目录".into());
    }
    Ok(PathBuf::from(OsString::from_wide(&buf[..len])))
}

pub(super) fn drive_root(path: &Path) -> Outcome<PathBuf> {
    let mut c = path.components();
    let letter = match c.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(l) | Prefix::VerbatimDisk(l) => l.to_ascii_uppercase(),
            _ => return Err("仅支持本地固定磁盘".into()),
        },
        _ => return Err("需要本地绝对路径".into()),
    };
    if !letter.is_ascii_uppercase() || c.next() != Some(Component::RootDir) ||
        c.any(|part| !matches!(part, Component::Normal(s) if !s.encode_wide().any(|ch| ch == 0 || ch == 58))) {
        return Err("路径不是本地文件系统中的普通路径".into());
    }
    let root = PathBuf::from(format!("{}:\\", letter as char));
    if unsafe { GetDriveTypeW(PCWSTR(wide(root.as_os_str()).as_ptr())) } != 3 {
        return Err("仅支持本地固定磁盘".into());
    }
    Ok(root)
}

#[derive(Clone, serde::Serialize)]
pub(super) struct Volume {
    pub root: String,
    pub total: Option<u64>,
    pub free: Option<u64>,
}
pub(super) fn volume(path: &Path) -> Outcome<Volume> {
    let root = drive_root(path)?;
    let (mut total, mut free) = (0u64, 0u64);
    let info = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide(root.as_os_str()).as_ptr()),
            None,
            Some(&mut total),
            Some(&mut free),
        )
    }
    .is_ok();
    // Subst aliases must not be a write scope; canonicalize the actual path before opening files.
    Ok(Volume {
        root: root.to_string_lossy().into_owned(),
        total: info.then_some(total),
        free: info.then_some(free),
    })
}
pub(super) fn volumes() -> Vec<Volume> {
    let bits = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|i| bits & (1 << i) != 0)
        .filter_map(|i| volume(Path::new(&format!("{}:\\", (b'A' + i) as char))).ok())
        .collect()
}

pub(super) fn regular(m: &Metadata) -> bool {
    // Reparse, offline, recall-on-data-access and recall-on-open are never followed/hydrated.
    m.file_attributes() & (0x400 | 0x1000 | 0x0040_0000 | 0x0004_0000) == 0
}
fn without_device_prefix(path: PathBuf) -> PathBuf {
    // Use wide characters rather than to_string_lossy; preserve the actual Windows file name.
    let s: Vec<_> = path.as_os_str().encode_wide().collect();
    if s.starts_with(&[92, 92, 63, 92]) && s.get(5) == Some(&58) {
        PathBuf::from(OsString::from_wide(&s[4..]))
    } else {
        path
    }
}
pub(super) fn checked_path(path: &Path) -> Outcome<PathBuf> {
    drive_root(path)?;
    for parent in path.ancestors() {
        if !regular(&fs::symlink_metadata(parent).map_err(error)?) {
            return Err("路径包含重解析点、离线或云占位项目".into());
        }
    }
    let resolved = without_device_prefix(fs::canonicalize(path).map_err(error)?);
    drive_root(&resolved)?;
    let original: Vec<_> = path.components().collect();
    let actual: Vec<_> = resolved.components().collect();
    if actual.len() != original.len()
        || !actual
            .iter()
            .zip(&original)
            .all(|(a, b)| a.as_os_str().eq_ignore_ascii_case(b.as_os_str()))
    {
        return Err("路径解析结果与原位置不一致".into());
    }
    Ok(resolved)
}
pub(super) fn child_of(path: &Path, root: &Path) -> bool {
    let mut components = path.components();
    root.components().all(|c| {
        components
            .next()
            .is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(c.as_os_str()))
    }) && components.next().is_some()
}

pub(super) fn pin_parents(path: &Path) -> Outcome<Vec<File>> {
    drive_root(path)?;
    let mut handles = Vec::new();
    for dir in path.ancestors().skip(1) {
        let h = OpenOptions::new()
            .access_mode(0x80)
            .share_mode(1 | 2)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(dir)
            .map_err(error)?;
        let m = h.metadata().map_err(error)?;
        if !m.is_dir() || !regular(&m) {
            return Err("上级目录在处理期间已改变".into());
        }
        handles.push(h);
    }
    Ok(handles)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) struct Stamp {
    pub volume: u64,
    pub id: [u8; 16],
    pub len: u64,
    pub written: u64,
}
pub(super) fn open_file(path: &Path, data: bool, share: u32) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(if data { 0x8000_0000 } else { 0x80 })
        .share_mode(share)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
}

pub(super) fn file_info(file: &File) -> Outcome<(Stamp, Option<u64>, u32)> {
    let m = file.metadata().map_err(error)?;
    if !m.is_file() || !regular(&m) {
        return Err("不是可处理的普通文件".into());
    }
    let h = HANDLE(file.as_raw_handle());
    let mut id = FILE_ID_INFO::default();
    let mut standard = FILE_STANDARD_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            h,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            std::mem::size_of_val(&id) as u32,
        )
        .map_err(error)?;
        GetFileInformationByHandleEx(
            h,
            FileStandardInfo,
            (&mut standard as *mut FILE_STANDARD_INFO).cast(),
            std::mem::size_of_val(&standard) as u32,
        )
        .map_err(error)?;
    }
    if standard.DeletePending.as_bool() {
        return Err("文件正在移除".into());
    }
    let allocated = if m.file_attributes() & (0x200 | 0x800) != 0 {
        // Sparse/compressed files have filesystem-reported storage independent of their logical extent.
        let mut compressed = FILE_COMPRESSION_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                h,
                FileCompressionInfo,
                (&mut compressed as *mut FILE_COMPRESSION_INFO).cast(),
                std::mem::size_of_val(&compressed) as u32,
            )
            .ok()
            .and_then(|_| u64::try_from(compressed.CompressedFileSize).ok())
        }
    } else {
        u64::try_from(standard.AllocationSize).ok()
    };
    Ok((
        Stamp {
            volume: id.VolumeSerialNumber,
            id: id.FileId.Identifier,
            len: m.len(),
            written: m.last_write_time(),
        },
        allocated,
        standard.NumberOfLinks,
    ))
}

pub(super) fn pick_folder(owner: usize) -> Outcome<Option<PathBuf>> {
    let _apartment = Apartment::new()?;
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(error)?;
        dialog
            .SetOptions(
                dialog.GetOptions().map_err(error)?
                    | FOS_PICKFOLDERS
                    | FOS_FORCEFILESYSTEM
                    | FOS_NOCHANGEDIR
                    | FOS_PATHMUSTEXIST,
            )
            .map_err(error)?;
        if let Err(e) = dialog.Show(HWND(owner as _)) {
            return if e.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) {
                Ok(None)
            } else {
                Err(error(e))
            };
        }
        let p = dialog
            .GetResult()
            .map_err(error)?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(error)?;
        let result = PathBuf::from(OsString::from_wide(p.as_wide()));
        CoTaskMemFree(Some(p.0.cast()));
        Ok(Some(result))
    }
}
pub(super) fn reveal(path: &Path) -> Outcome<()> {
    let _apartment = Apartment::new()?;
    let path = checked_path(path)?;
    unsafe {
        let mut item: *mut ITEMIDLIST = std::ptr::null_mut();
        SHParseDisplayName(
            PCWSTR(wide(path.as_os_str()).as_ptr()),
            None,
            &mut item,
            0,
            None,
        )
        .map_err(error)?;
        let result = SHOpenFolderAndSelectItems(item, None, 0).map_err(error);
        CoTaskMemFree(Some(item.cast()));
        result
    }
}

pub(super) fn recycle(root: &Path, path: &Path, stamp: &Stamp) -> Outcome<()> {
    let _apartment = Apartment::new()?;
    let _parents = pin_parents(path)?;
    let folder = checked_path(root)?;
    let path = checked_path(path)?;
    if !child_of(&path, &folder) {
        return Err("文件不在允许清理的目录内".into());
    }
    let handle = open_file(&path, false, 1 | 4).map_err(error)?; // Deny writers; allow Shell to recycle.
    let (identity, _, links) = file_info(&handle)?;
    if &identity != stamp || links != 1 {
        return Err("文件标识已变化或有多个硬链接".into());
    }
    // RecycleItem has no permanent-delete branch. The returned shell item is the receipt,
    // rather than inferring success from a global Recycle Bin count that may change independently.
    let receipt = unsafe {
        let item: IShellItem =
            SHCreateItemFromParsingName(PCWSTR(wide(path.as_os_str()).as_ptr()), None)
                .map_err(error)?;
        let parent = item.GetParent().map_err(error)?;
        let transfer: ITransferSource =
            parent.BindToHandler(None, &BHID_Transfer).map_err(error)?;
        let bin: IShellItem = SHGetKnownFolderItem(
            &FOLDERID_RecycleBinFolder,
            KF_FLAG_DEFAULT,
            HANDLE::default(),
        )
        .map_err(error)?;
        transfer
            .RecycleItem(&item, &bin, TSF_NORMAL.0 as u32)
            .map_err(error)?
    };
    drop(receipt);
    if !matches!(fs::symlink_metadata(&path), Err(e) if e.kind() == io::ErrorKind::NotFound) {
        return Err("Windows 回收调用返回，但源位置未核实".into());
    }
    Ok(())
}

pub(super) fn shell_page(owner: usize, key: &str) -> Outcome<()> {
    let uri = match key {
        "apps" => "ms-settings:appsfeatures",
        "startup" => "ms-settings:startupapps",
        "storage" => "ms-settings:storagesense",
        "update" => "ms-settings:windowsupdate",
        "security" => "windowsdefender:",
        "recycle" => "shell:RecycleBinFolder",
        _ => return Err("未知的 Windows 入口".into()),
    };
    let _com = Apartment::new()?;
    let rc = unsafe {
        ShellExecuteW(
            HWND(owner as _),
            PCWSTR(wide("open").as_ptr()),
            PCWSTR(wide(uri).as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if rc.0 as usize <= 32 {
        Err(format!("Windows 未打开该入口（{}）", rc.0 as usize))
    } else {
        Ok(())
    }
}
pub(super) fn confirm(owner: usize, title: &str, message: &str) -> bool {
    unsafe {
        MessageBoxW(
            HWND(owner as _),
            PCWSTR(wide(message).as_ptr()),
            PCWSTR(wide(title).as_ptr()),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES
    }
}

pub(super) fn creation(h: HANDLE) -> Outcome<u64> {
    let (mut stamp, mut exit, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        GetProcessTimes(h, &mut stamp, &mut exit, &mut k, &mut u).map_err(error)?;
    }
    Ok(filetime(stamp))
}
pub(super) fn process_image(h: HANDLE) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let mut n = buf.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).ok()?;
    }
    Some(PathBuf::from(OsString::from_wide(&buf[..n as usize])))
}
fn user_windows(pid: u32) -> Vec<HWND> {
    struct Query {
        pid: u32,
        found: Vec<HWND>,
    }
    unsafe extern "system" fn collect(win: HWND, data: LPARAM) -> windows::Win32::Foundation::BOOL {
        let q = &mut *(data.0 as *mut Query);
        let mut pid = 0;
        GetWindowThreadProcessId(win, Some(&mut pid));
        if pid == q.pid && IsWindowVisible(win).as_bool() {
            q.found.push(win);
        }
        windows::Win32::Foundation::BOOL(1)
    }
    let mut q = Query {
        pid,
        found: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM((&mut q as *mut Query) as isize));
    }
    q.found
}
fn ordinary(pid: u32, stamp: u64) -> Outcome<(Handle, PathBuf)> {
    if pid <= 4 || pid == std::process::id() {
        return Err("不能关闭系统进程或 OpenRevo 本身".into());
    }
    unsafe {
        let h = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(error)?);
        if creation(h.0)? != stamp {
            return Err("进程已退出或 PID 已复用".into());
        }
        same_account(token_for_process(h.0, false)?.0, desktop_token()?.0)?;
        let image = process_image(h.0).ok_or("未读到进程所属程序")?;
        let sys = system_dir()?;
        if sys.parent().is_some_and(|win| child_of(&image, win)) {
            return Err("Windows 系统应用不在操作范围内".into());
        }
        if user_windows(pid).is_empty() {
            return Err("此进程没有当前桌面可见的应用窗口".into());
        }
        Ok((h, image))
    }
}
pub(super) fn close_preview(pid: u32, stamp: u64) -> Outcome<String> {
    let (_, path) = ordinary(pid, stamp)?;
    Ok(path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned())
}
pub(super) fn close_app(pid: u32, stamp: u64) -> Outcome<bool> {
    let (h, _) = ordinary(pid, stamp)?;
    let mut asked = 0;
    for win in user_windows(pid) {
        unsafe {
            let mut current = 0;
            GetWindowThreadProcessId(win, Some(&mut current));
            if current == pid
                && creation(h.0)? == stamp
                && PostMessageW(win, WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok()
            {
                asked += 1;
            }
        }
    }
    if asked == 0 {
        return Err("没有窗口接受关闭请求".into());
    }
    for _ in 0..16 {
        let mut code = 0;
        if unsafe { windows::Win32::System::Threading::GetExitCodeProcess(h.0, &mut code) }.is_ok()
            && code != 259
        {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(125));
    }
    Ok(false)
}

pub(super) fn close_stamp(key: &str) -> Option<(u32, u64)> {
    let mut fields = key.split(':');
    if fields.next()? != "proc" {
        return None;
    }
    let pair = (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?);
    (fields.next().is_none()).then_some(pair)
}

pub(super) fn state_dir() -> Outcome<PathBuf> {
    let dir = appdata()?;
    checked_path(&dir)?;
    let dir = dir.join("OpenRevoManager");
    static LOCK: OnceLock<parking_lot::Mutex<()>> = OnceLock::new();
    let _lock = LOCK.get_or_init(|| parking_lot::Mutex::new(())).lock();
    match fs::create_dir(&dir) {
        Ok(()) => (),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(error(e)),
    }
    checked_path(&dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_window_identity_key_is_required() {
        assert_eq!(
            close_stamp("proc:407:15788259738999999"),
            Some((407, 15788259738999999))
        );
        for bad in [
            "p:407:1",
            "proc:0",
            "proc:12:1:2",
            "proc:NaN:1",
            "proc:7:not_a_stamp",
        ] {
            assert_eq!(close_stamp(bad), None);
        }
        assert!(child_of(
            Path::new(r"C:\Temp\abc\test"),
            Path::new(r"c:\Temp")
        ));
        assert!(!child_of(
            Path::new(r"C:\Temporary\test"),
            Path::new(r"C:\Temp")
        ));
    }

    #[test]
    #[ignore = "interactive Windows scratch file; verifies Recycle Bin and leaves no live system setting changed"]
    fn manager_live_recycle_transfer() {
        as_user(|| {
            let root =
                checked_path(&std::env::temp_dir()).map_err(|e| format!("fixture folder: {e}"))?;
            let marker = root.join(format!(
                "OpenRevo-Recycle-Probe-{}-{}.tmp",
                std::process::id(),
                super::super::now_ms()
            ));
            let data = b"disposable OpenRevo recycle test";
            use std::io::Write;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
                .map_err(error)?
                .write_all(data)
                .map_err(error)?;
            let (stamp, _, _) = file_info(&open_file(&marker, false, 1 | 2 | 4).map_err(error)?)?;
            let mut wrong = stamp.clone();
            wrong.id[0] ^= 1;
            assert!(
                recycle(&root, &marker, &wrong).is_err(),
                "stale identity was not rejected"
            );
            assert_eq!(fs::read(&marker).map_err(error)?, data);
            recycle(&root, &marker, &stamp).map_err(|e| format!("recycle transfer: {e}"))
        })
        .unwrap();
    }
}
