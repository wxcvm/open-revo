//! A single physical-volume accounting model. Shell recycle totals are only an
//! annotation, not part of the physical allocation equation.
use super::win;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use windows::{
    core::{s, w, HRESULT, PCWSTR},
    Win32::{
        Storage::FileSystem::{GetVolumeNameForVolumeMountPointW, DISK_SPACE_INFORMATION},
        System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
        UI::Shell::{
            FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX64,
            FOLDERID_ProgramFilesX86, FOLDERID_UserProfiles, SHQueryRecycleBinW, SHQUERYRBINFO,
        },
    },
};

const STORE: &str = "volume-snapshots-v1.json";
const STORE_LIMIT: usize = 72;
const PER_VOLUME_LIMIT: usize = 12;
const MAX_BYTES: u64 = 96 * 1024;
static HISTORY_WRITE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    User,
    Apps,
    Windows,
    SharedLinks,
    Other,
}

impl Category {
    fn index(self) -> usize {
        match self {
            Self::User => 0,
            Self::Apps => 1,
            Self::Windows => 2,
            Self::SharedLinks => 3,
            Self::Other => 4,
        }
    }
}

pub(super) struct Accounting {
    root: PathBuf,
    volume_id: Option<String>,
    user: Option<PathBuf>,
    apps: Vec<PathBuf>,
    windows: Option<PathBuf>,
    buckets: [u64; 5],
}

impl Accounting {
    pub(super) fn new(scope: &Path) -> Option<Self> {
        let root = win::drive_root(scope).ok()?;
        if root != scope {
            return None; // A selected folder is not a complete volume observation.
        }
        Some(Self {
            volume_id: volume_id(&root),
            root,
            user: win::known_folder(&FOLDERID_UserProfiles).ok(),
            apps: [
                FOLDERID_ProgramFiles,
                FOLDERID_ProgramFilesX86,
                FOLDERID_ProgramFilesX64,
                FOLDERID_ProgramData,
            ]
            .iter()
            .filter_map(|id| win::known_folder(id).ok())
            .collect(),
            windows: win::system_dir()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
            buckets: [0; 5],
        })
    }

    fn category(&self, path: &Path, links: u32) -> Category {
        if links > 1 {
            // A shared file is counted once by the scanner; do not attribute
            // it to whichever pathname happens to be visited first.
            Category::SharedLinks
        } else if self
            .windows
            .as_ref()
            .is_some_and(|p| win::child_of(path, p))
            || (path.parent() == Some(self.root.as_path())
                && path.file_name().is_some_and(|name| {
                    ["pagefile.sys", "hiberfil.sys", "swapfile.sys"]
                        .iter()
                        .any(|v| name.eq_ignore_ascii_case(v))
                }))
        {
            Category::Windows
        } else if self.apps.iter().any(|p| win::child_of(path, p)) {
            Category::Apps
        } else if self.user.as_ref().is_some_and(|p| win::child_of(path, p)) {
            Category::User
        } else {
            Category::Other
        }
    }

    pub(super) fn add(&mut self, path: &Path, links: u32, allocated: Option<u64>) {
        if let Some(amount) = allocated {
            let bucket = &mut self.buckets[self.category(path, links).index()];
            *bucket = bucket.saturating_add(amount);
        }
    }

    pub(super) fn finish(self, skipped: u64, unknown: u64) -> Overview {
        let before = volume_id(&self.root);
        let totals = capacity(&self.root);
        let recycle = recycle(&self.root);
        let after = volume_id(&self.root);
        let id_still_matches = self
            .volume_id
            .as_ref()
            .is_some_and(|id| before.as_ref() == Some(id) && after.as_ref() == Some(id));
        let identity_changed = self.volume_id.is_some() && !id_still_matches;
        let totals = if identity_changed { None } else { totals };
        let captured_ms = super::now_ms();
        let (previous, read_error) = if id_still_matches {
            match read_history() {
                Ok(history) => (history.latest(&self.volume_id, captured_ms).cloned(), None),
                Err(error) => (None, Some(error)),
            }
        } else {
            (None, None)
        };
        let current = Snapshot {
            captured_ms,
            totals,
            classified: self.buckets,
            unresolved: residual(totals.map(|t| t.used), self.buckets),
            recycle: if identity_changed { None } else { recycle },
            skipped,
            unknown_allocation: unknown,
        };
        Overview {
            current,
            previous,
            save_available: id_still_matches && totals.is_some(),
            saved: false,
            notice: read_error.or_else(|| {
                (!id_still_matches)
                    .then(|| "卷标识未读到或扫描期间已变化；此结果不写入历史。".into())
            }),
            root: self.root,
            volume_id: self.volume_id,
        }
    }
}

fn volume_id(root: &Path) -> Option<String> {
    let name = win::wide(root.as_os_str());
    let mut buf = [0u16; 128];
    unsafe {
        GetVolumeNameForVolumeMountPointW(PCWSTR(name.as_ptr()), &mut buf).ok()?;
    }
    let n = buf.iter().position(|&c| c == 0)?;
    let id = String::from_utf16(&buf[..n]).ok()?.to_ascii_lowercase();
    (id.starts_with(r"\\?\volume{") && id.ends_with("}\\")).then_some(id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct Totals {
    total: u64,
    free: u64,
    used: u64,
    #[serde(default)]
    reserved: Option<u64>,
    #[serde(default)]
    pool_unavailable: Option<u64>,
}

fn capacity(root: &Path) -> Option<Totals> {
    // This API is optional on older Windows builds. Do not pair caller-quota
    // capacity with whole-volume free space from a different data source.
    type Query = unsafe extern "system" fn(PCWSTR, *mut DISK_SPACE_INFORMATION) -> HRESULT;
    let address = unsafe {
        GetProcAddress(
            GetModuleHandleW(w!("kernel32.dll")).ok()?,
            s!("GetDiskSpaceInformationW"),
        )
    }?;
    let query: Query = unsafe { std::mem::transmute(address) };
    let name = win::wide(root.as_os_str());
    let mut out = DISK_SPACE_INFORMATION::default();
    unsafe {
        query(PCWSTR(name.as_ptr()), &mut out).ok().ok()?;
    }
    let unit =
        u64::from(out.SectorsPerAllocationUnit).checked_mul(u64::from(out.BytesPerSector))?;
    if unit == 0 {
        return None;
    }
    let total = out.ActualTotalAllocationUnits.checked_mul(unit)?;
    let optional_bytes = |n: u64| n.checked_mul(unit).filter(|&bytes| bytes <= total);
    let totals = Totals {
        total,
        free: out.ActualAvailableAllocationUnits.checked_mul(unit)?,
        used: out.UsedAllocationUnits.checked_mul(unit)?,
        reserved: optional_bytes(out.TotalReservedAllocationUnits),
        pool_unavailable: optional_bytes(out.ActualPoolUnavailableAllocationUnits),
    };
    (totals.total != 0 && totals.used <= totals.total && totals.free <= totals.total)
        .then_some(totals)
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Recycle {
    bytes: u64,
    items: u64,
}

fn recycle(root: &Path) -> Option<Recycle> {
    let name = win::wide(root.as_os_str());
    let mut entry = SHQUERYRBINFO {
        cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        SHQueryRecycleBinW(PCWSTR(name.as_ptr()), &mut entry).ok()?;
    }
    Some(Recycle {
        bytes: u64::try_from(entry.i64Size).ok()?,
        items: u64::try_from(entry.i64NumItems).ok()?,
    })
}

fn residual(used: Option<u64>, classified: [u64; 5]) -> Option<u64> {
    let available = used?;
    available.checked_sub(classified.into_iter().try_fold(0, u64::checked_add)?)
}

#[derive(Clone, Deserialize, Serialize)]
struct Snapshot {
    captured_ms: u64,
    totals: Option<Totals>,
    classified: [u64; 5],
    unresolved: Option<u64>,
    recycle: Option<Recycle>,
    skipped: u64,
    unknown_allocation: u64,
}

#[derive(Clone, Serialize)]
pub(crate) struct Overview {
    current: Snapshot,
    previous: Option<Snapshot>,
    save_available: bool,
    saved: bool,
    notice: Option<String>,
    #[serde(skip)]
    root: PathBuf,
    #[serde(skip)]
    volume_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct Entry {
    id: String,
    sample: Snapshot,
}

#[derive(Deserialize, Serialize)]
struct History {
    schema: u32,
    entries: Vec<Entry>,
}

impl History {
    fn empty() -> Self {
        Self {
            schema: 1,
            entries: Vec::new(),
        }
    }

    fn latest(&self, id: &Option<String>, before: u64) -> Option<&Snapshot> {
        let id = id.as_ref()?;
        self.entries
            .iter()
            .filter(|e| &e.id == id && e.sample.captured_ms < before)
            .max_by_key(|e| e.sample.captured_ms)
            .map(|e| &e.sample)
    }

    fn append(&mut self, id: String, sample: Snapshot) {
        self.entries
            .retain(|e| e.id != id || e.sample.captured_ms != sample.captured_ms);
        self.entries.push(Entry { id, sample });
        self.entries
            .sort_by_key(|e| std::cmp::Reverse(e.sample.captured_ms));
        let mut counts = std::collections::HashMap::new();
        self.entries.retain(|e| {
            let n = counts.entry(e.id.clone()).or_insert(0usize);
            *n += 1;
            *n <= PER_VOLUME_LIMIT
        });
        self.entries.truncate(STORE_LIMIT);
    }
}

fn read_history() -> win::Outcome<History> {
    let path = win::appdata()?.join("OpenRevoManager").join(STORE);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(History::empty()),
        Err(error) => return Err(win::error(error)),
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_BYTES => {}
        Ok(_) => return Err("磁盘快照路径或容量异常；原内容未改动。".into()),
    }
    let path = win::checked_path(&path)?;
    let _pins = win::pin_parents(&path)?;
    let mut bytes = Vec::new();
    win::open_file(&path, true, 1)
        .map_err(win::error)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(win::error)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("磁盘快照过大；原内容未改动。".into());
    }
    let history: History =
        serde_json::from_slice(&bytes).map_err(|_| "磁盘快照格式异常；原内容未改动。")?;
    if history.schema != 1 || history.entries.len() > STORE_LIMIT {
        return Err("磁盘快照版本或条数不受支持；原内容未改动。".into());
    }
    Ok(history)
}

pub(super) fn save(overview: &mut Overview) -> win::Outcome<()> {
    let _guard = HISTORY_WRITE.lock();
    let id = overview.volume_id.clone().ok_or("本轮缺少卷标识。")?;
    if !overview.save_available || volume_id(&overview.root).as_ref() != Some(&id) {
        return Err("卷标识或容量记录已变化，请重新扫描。".into());
    }
    // The only native snapshot write is a separate, explicit UI action.
    let mut history = read_history()?;
    let previous = history
        .latest(&Some(id.clone()), overview.current.captured_ms)
        .cloned();
    history.append(id, overview.current.clone());
    let path = win::state_dir()?.join(STORE);
    let _pins = win::pin_parents(&path)?;
    let bytes = serde_json::to_vec(&history).map_err(win::error)?;
    crate::system::config::persist_file(&path, &bytes).map_err(win::error)?;
    overview.previous = previous;
    overview.saved = true;
    overview.notice = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_volume_capacity_is_never_reconciled_against_logical_or_recycle_totals() {
        assert_eq!(residual(Some(100), [10, 8, 9, 2, 1]), Some(70));
        assert_eq!(residual(Some(25), [10, 8, 9, 2, 1]), None);
        assert_eq!(residual(None, [0; 5]), None);
        assert_eq!(residual(Some(u64::MAX), [1, 0, 0, 0, u64::MAX]), None);
    }

    #[test]
    fn allocation_is_unique_to_one_path_class_or_the_shared_link_bucket() {
        let mut a = Accounting {
            root: PathBuf::from(r"C:\"),
            volume_id: None,
            user: Some(PathBuf::from(r"C:\Users")),
            apps: vec![PathBuf::from(r"C:\Program Files")],
            windows: Some(PathBuf::from(r"C:\Windows")),
            buckets: [0; 5],
        };
        a.add(Path::new(r"c:\USERS\a\x.bin"), 1, Some(10));
        a.add(Path::new(r"C:\Program Files\a\x.bin"), 1, Some(20));
        a.add(Path::new(r"C:\Windows\a\x.bin"), 1, Some(30));
        a.add(Path::new(r"C:\pagefile.sys"), 1, Some(4));
        a.add(Path::new(r"C:\Windows\shared.dll"), 5, Some(40));
        a.add(Path::new(r"C:\WindowsOther\x.bin"), 1, Some(50));
        a.add(Path::new(r"C:\WindowsOther\unknown.bin"), 1, None);
        assert_eq!(a.buckets, [10, 20, 34, 40, 50]);
    }

    #[test]
    #[ignore = "read-only Windows physical-volume API check"]
    fn manager_live_volume_capacity() {
        let root = win::drive_root(&win::system_dir().unwrap()).unwrap();
        let sample = Accounting::new(&root).unwrap().finish(0, 0);
        let totals = sample
            .current
            .totals
            .expect("Windows physical-volume capacity");
        assert!(totals.total > 0 && totals.used <= totals.total);
        assert_eq!(sample.current.unresolved, Some(totals.used)); // No files sampled; no attribution.
        println!(
            "volume={} total_bytes={} used_bytes={} free_bytes={} reserved_bytes={:?} pool_unavailable_bytes={:?} capacity_gap_bytes={}",
            root.display(),
            totals.total,
            totals.used,
            totals.free,
            totals.reserved,
            totals.pool_unavailable,
            i128::from(totals.total) - i128::from(totals.used) - i128::from(totals.free),
        );
    }

    #[test]
    fn history_is_bounded_and_never_compares_different_volume_ids_or_the_same_scan() {
        let mut history = History::empty();
        for tick in 1..30 {
            history.append(
                "A".into(),
                Snapshot {
                    captured_ms: tick,
                    totals: None,
                    classified: [0; 5],
                    unresolved: None,
                    recycle: None,
                    skipped: 0,
                    unknown_allocation: 0,
                },
            );
        }
        assert_eq!(history.entries.len(), PER_VOLUME_LIMIT);
        assert_eq!(
            history.latest(&Some("A".into()), 29).unwrap().captured_ms,
            28
        );
        assert!(history.latest(&Some("B".into()), 29).is_none());
        assert!(history.latest(&Some("A".into()), 1).is_none());
    }
}
