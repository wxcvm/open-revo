//! On-demand Windows maintenance. The manager has its own WebView, never loads extension HTML,
//! and accepts only server-owned scan/preview item identifiers for writes.
pub(crate) mod handoff;
mod inventory;
mod quick_controls;
mod startup;
mod storage;
mod volume_overview;
mod win;

pub(crate) fn verify_interactive_account() -> Result<(), String> {
    win::verify_account()
}

pub(crate) fn process_group(h: windows::Win32::Foundation::HANDLE) -> Option<String> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hash = DefaultHasher::new();
    win::process_image(h)?
        .to_string_lossy()
        .to_lowercase()
        .hash(&mut hash);
    Some(format!("{:016x}", hash.finish()))
}

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

const LABEL: &str = "pc-manager";
const TTL: Duration = Duration::from_secs(600);

fn serial() -> u64 {
    static COUNT: AtomicU64 = AtomicU64::new(1);
    COUNT.fetch_add(1, Ordering::Relaxed)
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn is_manager_page(url: &tauri::Url) -> bool {
    let local = if cfg!(feature = "custom-protocol") {
        matches!(
            (url.scheme(), url.host_str(), url.port()),
            ("http" | "https", Some("tauri.localhost"), None) | ("tauri", Some("localhost"), None)
        )
    } else {
        matches!(
            (url.scheme(), url.host_str(), url.port()),
            ("http", Some("localhost"), Some(1420))
        )
    };
    local && url.path() == "/manager.html" && url.query().is_none()
}
pub(crate) fn authorize(w: &WebviewWindow, visible: bool) -> Result<usize, String> {
    if w.label() != LABEL || !w.url().ok().is_some_and(|url| is_manager_page(&url)) {
        return Err("此功能仅在独立的 OpenRevo 管理中心开放".into());
    }
    if visible && (!w.is_visible().unwrap_or(false) || w.is_minimized().unwrap_or(true)) {
        return Err("先打开管理中心窗口".into());
    }
    Ok(w.hwnd().map_err(win::error)?.0 as usize)
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> win::Outcome<T> + Send + 'static,
) -> win::Outcome<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(win::error)?
}

#[tauri::command]
pub(crate) async fn open_pc_manager(
    window: WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("仅从主窗口打开管理中心".into());
    }
    show(&app).await
}

#[tauri::command]
pub(crate) fn manager_session(
    window: WebviewWindow,
    session: tauri::State<'_, super::desktop_session::Session>,
) -> Result<super::desktop_session::Session, String> {
    authorize(&window, true)?;
    Ok(super::desktop_session::Session {
        hardware: session.hardware,
    })
}

#[tauri::command]
pub(crate) async fn manager_hardware_panel(
    window: WebviewWindow,
    app: tauri::AppHandle,
    session: tauri::State<'_, super::desktop_session::Session>,
) -> Result<(), String> {
    let owner = authorize(&window, true)?;
    if session.hardware {
        super::window::request_main_window(&app, super::window::MainWindowMode::Preferred);
        return Ok(());
    }
    blocking(move || handoff::launch(owner)).await?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub(crate) async fn manager_quick_controls(
    window: WebviewWindow,
    session: tauri::State<'_, super::desktop_session::Session>,
) -> Result<quick_controls::Snapshot, String> {
    authorize(&window, true)?;
    let hardware = session.hardware;
    blocking(move || quick_controls::read(hardware)).await
}

#[tauri::command]
pub(crate) async fn manager_set_quick_control(
    window: WebviewWindow,
    session: tauri::State<'_, super::desktop_session::Session>,
    action: quick_controls::Action,
) -> Result<quick_controls::Snapshot, String> {
    authorize(&window, true)?;
    let hardware = session.hardware;
    blocking(move || quick_controls::apply(action, hardware)).await
}

pub(crate) async fn show(app: &tauri::AppHandle) -> Result<(), String> {
    static OPEN_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let _guard = OPEN_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    if let Some(w) = app.get_webview_window(LABEL) {
        w.unminimize().map_err(win::error)?;
        w.show().map_err(win::error)?;
        return w.set_focus().map_err(win::error);
    }
    // No opener, no extension runtime, and an isolated WebView2 profile. The extension IPC
    // bridge can open this window, but it cannot read it or issue its management commands.
    let w = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("manager.html".into()))
        .title("OpenRevo 管理中心")
        .inner_size(1140.0, 802.0)
        .min_inner_size(728.0, 550.0)
        .center()
        .incognito(true)
        .devtools(false)
        .on_navigation(is_manager_page)
        .build()
        .map_err(win::error)?;
    w.on_window_event(|event| {
        if matches!(event, WindowEvent::Destroyed) {
            let _ = storage::cancel(None);
            previews().lock().clear();
            let _ = crate::system::resources::release_view(true);
        }
    });
    Ok(())
}

#[tauri::command]
pub(crate) async fn manager_health(window: WebviewWindow) -> Result<inventory::Health, String> {
    authorize(&window, true)?;
    blocking(inventory::health).await
}
#[tauri::command]
pub(crate) async fn manager_inventory(
    window: WebviewWindow,
    kind: String,
    hours: Option<u16>,
) -> Result<serde_json::Value, String> {
    authorize(&window, true)?;
    blocking(move || inventory::collect(&kind, hours.unwrap_or(24))).await
}
#[tauri::command]
pub(crate) async fn manager_startup(window: WebviewWindow) -> Result<startup::List, String> {
    authorize(&window, true)?;
    blocking(|| win::as_user(startup::list)).await
}
#[tauri::command]
pub(crate) async fn manager_shell(window: WebviewWindow, page: String) -> Result<(), String> {
    let owner = authorize(&window, true)?;
    blocking(move || {
        win::verify_account()?;
        win::shell_page(owner, &page)
    })
    .await
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scan {
    Volume {
        root: String,
        #[serde(default)]
        verify_duplicates: bool,
    },
    Folder {
        #[serde(default)]
        verify_duplicates: bool,
    },
    Cleanup,
}
#[tauri::command]
pub(crate) async fn manager_scan(
    window: WebviewWindow,
    target: Scan,
) -> Result<Option<storage::Status>, String> {
    let owner = authorize(&window, true)?;
    blocking(move || {
        win::verify_account()?;
        let (root, verify_duplicates) = match target {
            Scan::Volume {
                root,
                verify_duplicates,
            } if root.as_bytes().len() == 3
                && root.as_bytes()[0].is_ascii_uppercase()
                && root.ends_with(":\\") =>
            {
                (Some(PathBuf::from(root)), verify_duplicates)
            }
            Scan::Volume { .. } => return Err("无效的本地卷标识".into()),
            Scan::Folder { verify_duplicates } => match win::pick_folder(owner)? {
                Some(path) => (Some(path), verify_duplicates),
                None => return Ok(None),
            },
            Scan::Cleanup => (None, false),
        };
        win::as_user(|| {
            let checked = root.as_ref().map(|p| win::checked_path(p)).transpose()?;
            storage::start(checked, verify_duplicates).map(Some)
        })
    })
    .await
}
#[tauri::command]
pub(crate) async fn manager_scan_status(
    window: WebviewWindow,
    id: u64,
) -> Result<storage::Status, String> {
    authorize(&window, true)?;
    storage::status(id)
}
#[tauri::command]
pub(crate) async fn manager_save_volume_snapshot(
    window: WebviewWindow,
    id: u64,
) -> Result<storage::Status, String> {
    authorize(&window, true)?;
    blocking(move || win::as_user(|| storage::save_volume_snapshot(id))).await
}

#[tauri::command]
pub(crate) async fn manager_scan_page(
    window: WebviewWindow,
    id: u64,
    section: String,
    offset: usize,
) -> Result<Vec<storage::Row>, String> {
    authorize(&window, true)?;
    storage::page(id, &section, offset)
}
#[tauri::command]
pub(crate) async fn manager_scan_cancel(window: WebviewWindow, id: u64) -> Result<(), String> {
    authorize(&window, false)?;
    storage::cancel(Some(id))
}
#[tauri::command]
pub(crate) async fn manager_reveal(
    window: WebviewWindow,
    job_id: u64,
    row_id: u64,
) -> Result<(), String> {
    authorize(&window, true)?;
    blocking(move || win::as_user(|| win::reveal(&storage::paths(job_id, row_id)?))).await
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Cleanup { job_id: u64, ids: Vec<u64> },
    Startup { id: String, restore: bool },
    Close { process: String },
}
enum Planned {
    Cleanup(Vec<storage::Candidate>),
    Startup(startup::Entry, bool),
    Close(u32, u64, String),
}
struct Pending {
    time: Instant,
    action: Planned,
}
fn previews() -> &'static Mutex<BTreeMap<u64, Pending>> {
    static ITEMS: OnceLock<Mutex<BTreeMap<u64, Pending>>> = OnceLock::new();
    ITEMS.get_or_init(|| Mutex::new(BTreeMap::new()))
}
fn apply_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Serialize)]
pub struct Preview {
    id: u64,
    kind: &'static str,
    caption: &'static str,
    items: Vec<String>,
    selected_bytes: Option<u64>,
    recovery: &'static str,
}
fn make_preview(action: Action) -> win::Outcome<Preview> {
    let action = match action {
        Action::Cleanup { job_id, ids } => Planned::Cleanup(storage::candidates(job_id, &ids)?),
        Action::Startup { id, restore } => {
            Planned::Startup(startup::lookup(&id, restore)?, restore)
        }
        Action::Close { process } => {
            let (p, time) = win::close_stamp(&process).ok_or("进程没有完整的创建时间")?;
            Planned::Close(p, time, win::close_preview(p, time)?)
        }
    };
    let id = serial();
    let (kind, caption, items, selected_bytes, recovery) = match &action {
        Planned::Cleanup(items) => (
            "cleanup",
            "移入 Windows 回收站",
            items
                .iter()
                .map(|i| i.path.to_string_lossy().into_owned())
                .collect(),
            items
                .iter()
                .try_fold(0u64, |sum, i| sum.checked_add(i.allocated?)),
            "从 Windows 回收站还原。移入回收站不等于已经释放空间。",
        ),
        Planned::Startup(e, restore) => (
            "startup",
            if *restore {
                "恢复此用户启动项"
            } else {
                "禁用此用户启动项"
            },
            vec![e.name.clone()],
            None,
            if *restore {
                "保留 Windows 的其他现有设置，不覆盖同名启动项。"
            } else {
                "收到回执后可在「启动项」中恢复。"
            },
        ),
        Planned::Close(p, _, name) => (
            "close",
            "请求应用正常关闭",
            vec![format!("{name} (PID {p})")],
            None,
            "只发送正常关闭请求，应用可能提示保存、留在后台或拒绝；不强制结束。",
        ),
    };
    let mut queue = previews().lock();
    queue.retain(|_, p| p.time.elapsed() < TTL);
    if queue.len() >= 8 {
        queue.pop_first();
    }
    queue.insert(
        id,
        Pending {
            time: Instant::now(),
            action,
        },
    );
    Ok(Preview {
        id,
        kind,
        caption,
        items,
        selected_bytes,
        recovery,
    })
}

#[derive(Clone, Deserialize, Serialize)]
struct ItemResult {
    item: String,
    status: String,
    detail: String,
}
impl ItemResult {
    fn new(item: String, status: &str, detail: impl Into<String>) -> Self {
        Self {
            item,
            status: status.into(),
            detail: detail.into(),
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Receipt {
    id: u64,
    time_ms: u64,
    kind: String,
    canceled: bool,
    rows: Vec<ItemResult>,
    volumes: Vec<VolumeDelta>,
    warning: Option<String>,
}
#[derive(Clone, Deserialize, Serialize)]
struct VolumeDelta {
    root: String,
    free_before: Option<u64>,
    free_after: Option<u64>,
}

fn log_path() -> win::Outcome<PathBuf> {
    Ok(win::appdata()?.join("OpenRevoManager/receipts.json"))
}
fn read_receipts() -> win::Outcome<Vec<Receipt>> {
    let path = log_path()?;
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(win::error(e)),
    };
    win::checked_path(&path)?;
    if meta.len() > 8 * 1024 * 1024 {
        return Err("本机记录大小异常".into());
    }
    serde_json::from_slice(&std::fs::read(path).map_err(win::error)?).map_err(win::error)
}
fn save_receipt(receipt: &Receipt) -> win::Outcome<()> {
    let _safe_dir = win::state_dir()?;
    let path = log_path()?;
    let _parents = win::pin_parents(&path)?;
    let mut log = read_receipts()?;
    log.insert(0, receipt.clone());
    log.truncate(50);
    let mut data = serde_json::to_vec(&log).map_err(win::error)?;
    while data.len() > 8 * 1024 * 1024 && log.len() > 1 {
        log.pop();
        data = serde_json::to_vec(&log).map_err(win::error)?;
    }
    crate::system::config::persist_file(&path, &data).map_err(win::error)
}

fn apply(owner: usize, id: u64) -> win::Outcome<Receipt> {
    let _exclusive = apply_lock().try_lock().ok_or("另一项维护操作尚在执行")?;
    let pending = previews()
        .lock()
        .remove(&id)
        .filter(|p| p.time.elapsed() < TTL)
        .ok_or("预览已使用或过期，请重新预览")?;
    win::verify_account()?;
    let (kind, warning) = match &pending.action {
        Planned::Cleanup(items) => (
            "cleanup",
            format!(
                "共 {} 个已选文件。只移入回收站，不立即释放容量。",
                items.len()
            ),
        ),
        Planned::Startup(e, restore) => (
            "startup",
            format!(
                "{} {}\n仅操作当前登录用户的启动项。",
                if *restore { "恢复" } else { "禁用" },
                e.name
            ),
        ),
        Planned::Close(pid, _, name) => (
            "close",
            format!("请求正常关闭 {name} (PID {pid})。请留意应用的保存提示。"),
        ),
    };
    let mut out = Receipt {
        id,
        time_ms: now_ms(),
        kind: kind.into(),
        canceled: false,
        rows: Vec::new(),
        volumes: Vec::new(),
        warning: None,
    };
    if !win::confirm(owner, "确认 OpenRevo 维护操作", &warning) {
        out.canceled = true;
        return Ok(out);
    }
    win::as_user(|| {
        match pending.action {
            Planned::Cleanup(items) => {
                let mut volumes = BTreeMap::new();
                for item in &items {
                    if let Ok(v) = win::volume(&item.path) {
                        volumes.entry(v.root).or_insert(v.free);
                    }
                }
                for item in items {
                    let path = item.path.to_string_lossy().into_owned();
                    let row = match storage::validate_candidate(&item) {
                        Err(e) => ItemResult::new(path, "skipped", e),
                        Ok(()) => match win::recycle(&item.rule.root, &item.path, &item.stamp) {
                            Ok(()) => ItemResult::new(
                                path,
                                "done",
                                "已移入 Windows 回收站；未宣称释放容量",
                            ),
                            Err(e) => ItemResult::new(path, "unverified", e),
                        },
                    };
                    out.rows.push(row);
                }
                out.volumes = volumes
                    .into_iter()
                    .map(|(root, free_before)| {
                        let free_after = win::volume(Path::new(&root)).ok().and_then(|v| v.free);
                        VolumeDelta {
                            root,
                            free_before,
                            free_after,
                        }
                    })
                    .collect();
            }
            Planned::Startup(e, restore) => {
                let result = startup::change(&e, restore);
                out.rows.push(match result {
                    Ok(detail) => ItemResult::new(e.name, "done", detail),
                    Err(error) => ItemResult::new(e.name, "unverified", error),
                });
            }
            Planned::Close(pid, time, name) => {
                let name = format!("{name} (PID {pid})");
                out.rows.push(match win::close_app(pid, time) {
                    Ok(true) => ItemResult::new(name, "done", "进程已经退出"),
                    Ok(false) => ItemResult::new(name, "pending", "已发出关闭请求；该进程仍在运行"),
                    Err(e) => ItemResult::new(name, "skipped", e),
                });
            }
        }
        if let Err(e) = save_receipt(&out) {
            out.warning = Some(format!("操作已核对，但本机记录未保存：{e}"));
        }
        Ok(out)
    })
}
#[tauri::command]
pub(crate) async fn manager_preview(
    window: WebviewWindow,
    action: Action,
) -> Result<Preview, String> {
    authorize(&window, true)?;
    blocking(move || win::as_user(|| make_preview(action))).await
}
#[tauri::command]
pub(crate) async fn manager_apply(window: WebviewWindow, id: u64) -> Result<Receipt, String> {
    let owner = authorize(&window, true)?;
    blocking(move || apply(owner, id)).await
}
#[tauri::command]
pub(crate) async fn manager_receipts(window: WebviewWindow) -> Result<Vec<Receipt>, String> {
    authorize(&window, true)?;
    blocking(|| win::as_user(read_receipts)).await
}
#[tauri::command]
pub(crate) async fn manager_peaks(
    window: WebviewWindow,
) -> Result<Vec<crate::system::resources::Peak>, String> {
    authorize(&window, true)?;
    crate::system::resources::recent_peaks()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manager_accepts_only_the_expected_local_entry() {
        let entry = if cfg!(feature = "custom-protocol") {
            "http://tauri.localhost"
        } else {
            "http://localhost:1420"
        };
        let url = |s: &str| tauri::Url::parse(s).unwrap();
        assert!(is_manager_page(&url(&format!("{entry}/manager.html"))));
        assert!(!is_manager_page(&url(&format!("{entry}/index.html"))));
        assert!(!is_manager_page(&url(&format!(
            "{entry}/manager.html?window=widget"
        ))));
        assert!(!is_manager_page(&url(
            "http://unrelated.localhost/manager.html"
        )));
    }
}
