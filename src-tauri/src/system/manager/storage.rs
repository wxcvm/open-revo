use super::{volume_overview, win};
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Read,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, OnceLock,
    },
};

const AGE: u64 = 7 * 24 * 60 * 60 * 10_000_000;
const FILETIME_EPOCH: u64 = 116_444_736_000_000_000;
const LARGE_LIMIT: usize = 250;
const GROUP_LIMIT: usize = 8_192;
const DUP_CANDIDATES: usize = 24_000;
const DUP_BUDGET: u64 = 4 * 1024 * 1024 * 1024;
const CLEAN_LIMIT: usize = 10_000;

#[derive(Clone)]
pub(super) struct Rule {
    pub root: PathBuf,
    pub label: &'static str,
    browser: Option<&'static str>,
}
#[derive(Clone)]
pub(super) struct Candidate {
    pub path: PathBuf,
    pub stamp: win::Stamp,
    pub rule: Rule,
    pub allocated: Option<u64>,
}

pub(super) fn app_running(name: &str) -> win::Outcome<bool> {
    use windows::Win32::{Foundation::CloseHandle, System::Diagnostics::ToolHelp::*};
    unsafe {
        let h = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(win::error)?;
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let result = (|| {
            Process32FirstW(h, &mut e).map_err(win::error)?;
            loop {
                let n = e
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(e.szExeFile.len());
                if String::from_utf16_lossy(&e.szExeFile[..n]).eq_ignore_ascii_case(name) {
                    return Ok(true);
                }
                if let Err(err) = Process32NextW(h, &mut e) {
                    return if err.code() == windows::core::HRESULT::from_win32(18) {
                        Ok(false)
                    } else {
                        Err(win::error(err))
                    };
                }
            }
        })();
        let _ = CloseHandle(h);
        result
    }
}

fn browser_roots(local: &Path) -> [(PathBuf, &'static str, &'static str); 3] {
    [
        (
            local.join("Microsoft/Edge/User Data"),
            "Edge 缓存",
            "msedge.exe",
        ),
        (
            local.join("Google/Chrome/User Data"),
            "Chrome 缓存",
            "chrome.exe",
        ),
        (
            local.join("BraveSoftware/Brave-Browser/User Data"),
            "Brave 缓存",
            "brave.exe",
        ),
    ]
}

fn browser_profile(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "Default"
        || name
            .strip_prefix("Profile ")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}

fn rules(job: &Job) -> win::Outcome<Vec<Rule>> {
    let local = win::checked_path(&win::appdata()?)?;
    let mut out = Vec::new();
    let temp = local.join("Temp");
    match win::checked_path(&temp) {
        Ok(path) => out.push(Rule {
            root: path,
            label: "当前用户临时文件",
            browser: None,
        }),
        Err(e) => job
            .notes
            .lock()
            .push(format!("当前用户临时目录未读取：{e}")),
    }
    for (base, label, process) in browser_roots(&local) {
        if !base.exists() {
            continue;
        }
        if app_running(process).unwrap_or(true) {
            job.notes
                .lock()
                .push(format!("{}：浏览器仍在运行或状态未取得，已跳过", label));
            continue;
        }
        match win::checked_path(&base)
            .ok()
            .and_then(|path| fs::read_dir(path).ok())
        {
            Some(profiles) => {
                for profile in profiles
                    .flatten()
                    .filter(|p| browser_profile(&p.file_name()))
                {
                    for sub in [
                        "Cache/Cache_Data",
                        "Code Cache/js",
                        "Code Cache/wasm",
                        "GPUCache",
                    ] {
                        if let Ok(root) = win::checked_path(&profile.path().join(sub)) {
                            out.push(Rule {
                                root,
                                label,
                                browser: Some(process),
                            });
                        }
                    }
                }
            }
            None => job.notes.lock().push(format!("{}：缓存位置未取得", label)),
        }
    }
    Ok(out)
}

fn old(stamp: &win::Stamp) -> bool {
    let now = FILETIME_EPOCH.saturating_add(super::now_ms().saturating_mul(10_000));
    stamp.written > 0 && now.saturating_sub(stamp.written) >= AGE
}
fn temp_name(path: &Path) -> bool {
    path.extension().and_then(|p| p.to_str()).is_some_and(|s| {
        ["tmp", "temp", "log", "etl", "dmp"]
            .iter()
            .any(|e| s.eq_ignore_ascii_case(e))
    })
}

pub(super) fn validate_candidate(item: &Candidate) -> win::Outcome<()> {
    let local = win::checked_path(&win::appdata()?)?;
    if !old(&item.stamp)
        || !win::child_of(&item.path, &item.rule.root)
        || !win::child_of(&item.rule.root, &local)
    {
        return Err("清理范围或文件时间已变化".into());
    }
    match item.rule.browser {
        None if item.rule.root == local.join("Temp") && temp_name(&item.path) => Ok(()),
        Some(browser) if !app_running(browser)? => Ok(()), // The root was generated by the native allow-list; never received from the UI.
        Some(_) => Err("浏览器已重新打开，已跳过缓存".into()),
        None => Err("不属于可处理的临时文件".into()),
    }
}

#[derive(Clone, Serialize, Default)]
struct Stats {
    files: u64,
    logical: u64,
    allocated: u64,
    unknown_allocation: u64,
    skipped: u64,
    aliases: u64,
    uncategorized: u64,
    cleanup_limited: bool,
    duplicates_limited: bool,
}
#[derive(Clone, Serialize)]
pub(crate) struct Row {
    pub id: u64,
    pub path: String,
    pub category: String,
    pub logical: u64,
    pub allocated: Option<u64>,
    pub count: u64,
    pub group: Option<u64>,
}
impl Row {
    fn file(
        id: u64,
        path: &Path,
        stamp: &win::Stamp,
        allocated: Option<u64>,
        category: &str,
    ) -> Self {
        Self {
            id,
            path: path.to_string_lossy().into_owned(),
            category: category.into(),
            logical: stamp.len,
            allocated,
            count: 1,
            group: None,
        }
    }
}
#[derive(Default)]
struct ResultSet {
    overview: Option<volume_overview::Overview>,
    stats: Stats,
    tree: Vec<Row>,
    largest: Vec<Row>,
    duplicates: Vec<Row>,
    cleanup: Vec<(Row, Candidate)>,
    paths: HashMap<u64, PathBuf>,
}
#[derive(Serialize)]
struct Progress {
    files: u64,
    skipped: u64,
    logical: u64,
    allocated: u64,
}
#[derive(Serialize)]
pub(crate) struct Status {
    id: u64,
    kind: &'static str,
    state: &'static str,
    scope: String,
    volume: Option<win::Volume>,
    overview: Option<volume_overview::Overview>,
    duplicates_requested: bool,
    progress: Progress,
    total: Vec<usize>,
    stats: Option<Stats>,
    notes: Vec<String>,
    error: Option<String>,
}

pub(super) struct Job {
    pub id: u64,
    root: Option<PathBuf>,
    verify_duplicates: bool,
    cancel: AtomicBool,
    files: AtomicU64,
    skipped: AtomicU64,
    logical: AtomicU64,
    allocated: AtomicU64,
    result: Mutex<Option<win::Outcome<ResultSet>>>,
    notes: Mutex<Vec<String>>,
}
impl Job {
    fn new(root: Option<PathBuf>, verify_duplicates: bool) -> Self {
        Self {
            id: super::serial(),
            root,
            verify_duplicates,
            cancel: false.into(),
            files: 0.into(),
            skipped: 0.into(),
            logical: 0.into(),
            allocated: 0.into(),
            result: Mutex::new(None),
            notes: Mutex::new(Vec::new()),
        }
    }
    fn halted(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn status(&self) -> Status {
        let result = self.result.lock();
        let ok = result.as_ref().and_then(|r| r.as_ref().ok());
        let state = match result.as_ref() {
            None if self.halted() => "canceling",
            None => "running",
            Some(_) if self.halted() => "canceled",
            Some(Err(_)) => "failed",
            Some(Ok(_)) => "complete",
        };
        Status {
            id: self.id,
            kind: if self.root.is_some() {
                "space"
            } else {
                "cleanup"
            },
            state: if state == "complete"
                && ok.is_some_and(|r| {
                    r.stats.skipped > 0
                        || r.stats.unknown_allocation > 0
                        || r.stats.cleanup_limited
                        || r.stats.duplicates_limited
                        || !self.notes.lock().is_empty()
                }) {
                "partial"
            } else {
                state
            },
            scope: self.root.as_ref().map_or_else(
                || "当前用户 · 临时文件与受支持缓存".into(),
                |r| r.to_string_lossy().into_owned(),
            ),
            volume: self.root.as_ref().and_then(|root| win::volume(root).ok()),
            overview: ok.and_then(|r| r.overview.clone()),
            duplicates_requested: self.verify_duplicates,
            progress: Progress {
                files: self.files.load(Ordering::Relaxed),
                skipped: self.skipped.load(Ordering::Relaxed),
                logical: self.logical.load(Ordering::Relaxed),
                allocated: self.allocated.load(Ordering::Relaxed),
            },
            total: ok.map_or_else(Vec::new, |r| {
                vec![
                    r.tree.len(),
                    r.largest.len(),
                    r.duplicates.len(),
                    r.cleanup.len(),
                ]
            }),
            stats: ok.map(|r| r.stats.clone()),
            notes: self.notes.lock().clone(),
            error: result.as_ref().and_then(|r| r.as_ref().err().cloned()),
        }
    }
}

fn slot() -> &'static Mutex<Option<Arc<Job>>> {
    static JOB: OnceLock<Mutex<Option<Arc<Job>>>> = OnceLock::new();
    JOB.get_or_init(|| Mutex::new(None))
}
fn get(id: u64) -> win::Outcome<Arc<Job>> {
    slot()
        .lock()
        .as_ref()
        .filter(|job| job.id == id)
        .cloned()
        .ok_or_else(|| "扫描已结束或已被新任务替换".into())
}
pub(super) fn cancel(id: Option<u64>) -> win::Outcome<()> {
    if let Some(job) = slot().lock().as_ref() {
        if id.is_none_or(|id| id == job.id) && job.result.lock().is_none() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
    Ok(())
}
pub(super) fn start(root: Option<PathBuf>, verify_duplicates: bool) -> win::Outcome<Status> {
    let mut s = slot().lock();
    if s.as_ref().is_some_and(|j| j.result.lock().is_none()) {
        return Err("已有扫描，请等待或先取消".into());
    }
    let job = Arc::new(Job::new(root, verify_duplicates));
    let worker = job.clone();
    std::thread::Builder::new()
        .name("openrevo-storage-scan".into())
        .spawn(move || {
            let output = win::as_user(|| Scanner::run(&worker));
            *worker.result.lock() = Some(output);
        })
        .map_err(win::error)?;
    *s = Some(job.clone());
    Ok(job.status())
}
pub(super) fn status(id: u64) -> win::Outcome<Status> {
    Ok(get(id)?.status())
}
pub(super) fn save_volume_snapshot(id: u64) -> win::Outcome<Status> {
    let job = get(id)?;
    {
        let mut result = job.result.lock();
        if job.halted() {
            return Err("已取消的扫描不写入卷快照。".into());
        }
        let overview = result
            .as_mut()
            .and_then(|r| r.as_mut().ok())
            .and_then(|r| r.overview.as_mut())
            .ok_or("仅已完成的整卷扫描可保存卷快照。")?;
        volume_overview::save(overview)?;
    }
    Ok(job.status())
}
pub(super) fn page(id: u64, section: &str, offset: usize) -> win::Outcome<Vec<Row>> {
    let job = get(id)?;
    let result = job.result.lock();
    let r = result
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .ok_or("扫描结果未就绪")?;
    Ok(match section {
        "tree" => r.tree.iter().skip(offset).take(40).cloned().collect(),
        "largest" => r.largest.iter().skip(offset).take(40).cloned().collect(),
        "duplicates" => r.duplicates.iter().skip(offset).take(40).cloned().collect(),
        "cleanup" => r
            .cleanup
            .iter()
            .skip(offset)
            .take(40)
            .map(|p| p.0.clone())
            .collect(),
        _ => return Err("未知的扫描结果类别".into()),
    })
}
pub(super) fn paths(id: u64, row: u64) -> win::Outcome<PathBuf> {
    let job = get(id)?;
    let result = job.result.lock();
    result
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .and_then(|r| r.paths.get(&row))
        .cloned()
        .ok_or_else(|| "该项目没有可定位的路径".into())
}
pub(super) fn candidates(id: u64, ids: &[u64]) -> win::Outcome<Vec<Candidate>> {
    if ids.is_empty() || ids.len() > 200 || ids.iter().collect::<HashSet<_>>().len() != ids.len() {
        return Err("每次选择 1–200 个不同的可回收项目".into());
    }
    let job = get(id)?;
    if job.halted() {
        return Err("扫描已取消，请重新扫描".into());
    }
    let result = job.result.lock();
    let r = result
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .ok_or("扫描结果未就绪")?;
    ids.iter()
        .map(|id| {
            r.cleanup
                .iter()
                .find(|p| p.0.id == *id)
                .map(|p| p.1.clone())
                .ok_or_else(|| "所选项目不在此次扫描中".into())
        })
        .collect()
}

#[derive(Default)]
struct Bucket {
    logical: u64,
    allocated: u64,
    seen: u64,
    unknown: u64,
}
type FileEntry = (PathBuf, win::Stamp, Option<u64>);
struct Scanner<'a> {
    job: &'a Job,
    count: u64,
    seen_links: HashSet<(u64, [u8; 16])>,
    top: BTreeMap<(u64, u64), FileEntry>,
    bins: BTreeMap<PathBuf, Bucket>,
    dups: HashMap<u64, Vec<FileEntry>>,
    dup_count: usize,
    output: ResultSet,
    volume: Option<volume_overview::Accounting>,
}

impl<'a> Scanner<'a> {
    fn run(job: &'a Job) -> win::Outcome<ResultSet> {
        let mut s = Self {
            job,
            count: 0,
            seen_links: HashSet::new(),
            top: BTreeMap::new(),
            bins: BTreeMap::new(),
            dups: HashMap::new(),
            dup_count: 0,
            output: ResultSet::default(),
            volume: None,
        };
        if let Some(root) = &job.root {
            let root = win::checked_path(root)?;
            s.volume = volume_overview::Accounting::new(&root);
            s.walk(&root, None, None, 0);
        } else {
            for rule in rules(job)? {
                if job.halted() {
                    break;
                }
                s.walk(&rule.root, None, Some(&rule), 0);
            }
        }
        s.finish()
    }

    fn skip(&mut self) {
        self.output.stats.skipped += 1;
        self.job.skipped.fetch_add(1, Ordering::Relaxed);
    }

    fn walk(&mut self, dir: &Path, bucket: Option<&Path>, rule: Option<&Rule>, depth: u16) {
        if self.job.halted() {
            return;
        }
        if depth >= 96 {
            self.skip();
            return;
        }
        // Pin ancestor directories while walking. A path exchanged for a junction is not traversed.
        let pin = OpenOptions::new()
            .access_mode(0x80)
            .share_mode(1 | 2)
            .custom_flags(0x0200_0000 | 0x0020_0000)
            .open(dir);
        let Ok(pin) = pin else {
            self.skip();
            return;
        };
        if !pin.metadata().is_ok_and(|m| m.is_dir() && win::regular(&m)) {
            self.skip();
            return;
        }
        let Ok(children) = fs::read_dir(dir) else {
            self.skip();
            return;
        };
        for entry in children {
            if self.job.halted() {
                break;
            }
            let Ok(entry) = entry else {
                self.skip();
                continue;
            };
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                self.skip();
                continue;
            };
            if !win::regular(&meta) {
                self.skip();
                continue;
            }
            if rule.is_none()
                && depth == 0
                && ["System Volume Information", "$Recycle.Bin"]
                    .iter()
                    .any(|n| entry.file_name().eq_ignore_ascii_case(n))
            {
                self.skip();
                continue;
            }
            let bucket = bucket.or(Some(path.as_path()));
            if meta.is_dir() {
                self.walk(&path, bucket, rule, depth + 1);
            } else if meta.is_file() {
                self.file(&path, bucket.unwrap_or(dir), rule);
            } else {
                self.skip();
            }
        }
    }

    fn file(&mut self, path: &Path, bucket: &Path, rule: Option<&Rule>) {
        if let Some(rule) = rule {
            if rule.browser.is_none() && !temp_name(path) {
                return;
            }
        }
        let info = win::open_file(path, false, 1 | 2 | 4)
            .map_err(win::error)
            .and_then(|f| win::file_info(&f));
        let (stamp, allocated, links) = match info {
            Ok(info) => info,
            Err(_) if rule.is_none() => {
                self.output.stats.unknown_allocation += 1;
                self.skip();
                return;
            }
            Err(_) => {
                self.skip();
                return;
            }
        };
        if links > 1 {
            if rule.is_some() {
                self.skip();
                return;
            }
            if !self.seen_links.insert((stamp.volume, stamp.id)) {
                self.output.stats.aliases += 1;
                return;
            }
        }
        if rule.is_some() && !old(&stamp) {
            return;
        }
        self.count += 1;
        self.output.stats.files += 1;
        self.output.stats.logical = self.output.stats.logical.saturating_add(stamp.len);
        self.output.stats.allocated = self
            .output
            .stats
            .allocated
            .saturating_add(allocated.unwrap_or(0));
        self.output.stats.unknown_allocation += u64::from(allocated.is_none());
        self.job.files.fetch_add(1, Ordering::Relaxed);
        self.job.logical.fetch_add(stamp.len, Ordering::Relaxed);
        self.job
            .allocated
            .fetch_add(allocated.unwrap_or(0), Ordering::Relaxed);
        if let Some(rule) = rule {
            if self.output.cleanup.len() < CLEAN_LIMIT {
                let row = Row::file(self.count, path, &stamp, allocated, rule.label);
                self.output.paths.insert(row.id, path.to_path_buf());
                self.output.cleanup.push((
                    row,
                    Candidate {
                        path: path.to_path_buf(),
                        stamp,
                        rule: rule.clone(),
                        allocated,
                    },
                ));
            } else {
                self.output.stats.cleanup_limited = true;
            }
            return;
        }
        if let Some(volume) = self.volume.as_mut() {
            volume.add(path, links, allocated);
        }
        if self.bins.len() < GROUP_LIMIT || self.bins.contains_key(bucket) {
            let b = self.bins.entry(bucket.to_path_buf()).or_default();
            b.seen += 1;
            b.logical = b.logical.saturating_add(stamp.len);
            b.allocated = b.allocated.saturating_add(allocated.unwrap_or(0));
            b.unknown += u64::from(allocated.is_none());
        } else {
            self.output.stats.uncategorized += 1;
        }
        let size = allocated.unwrap_or(stamp.len);
        if self.top.len() < LARGE_LIMIT
            || self
                .top
                .first_key_value()
                .is_some_and(|((n, _), _)| size > *n)
        {
            self.top.insert(
                (size, self.count),
                (path.to_path_buf(), stamp.clone(), allocated),
            );
            if self.top.len() > LARGE_LIMIT {
                self.top.pop_first();
            }
        }
        if self.job.verify_duplicates && stamp.len >= 1024 * 1024 {
            if self.dup_count < DUP_CANDIDATES {
                self.dup_count += 1;
                self.dups.entry(stamp.len).or_default().push((
                    path.to_path_buf(),
                    stamp,
                    allocated,
                ));
            } else {
                self.output.stats.duplicates_limited = true;
            }
        }
    }

    fn finish(mut self) -> win::Outcome<ResultSet> {
        for (path, b) in std::mem::take(&mut self.bins) {
            self.count += 1;
            self.output.paths.insert(self.count, path.clone());
            self.output.tree.push(Row {
                id: self.count,
                path: path.to_string_lossy().into_owned(),
                category: "占用分布".into(),
                logical: b.logical,
                allocated: (b.unknown == 0).then_some(b.allocated),
                count: b.seen,
                group: None,
            });
        }
        self.output
            .tree
            .sort_by_key(|r| std::cmp::Reverse(r.allocated.unwrap_or(r.logical)));
        for ((_, id), (path, stamp, alloc)) in std::mem::take(&mut self.top).into_iter().rev() {
            self.output.paths.insert(id, path.clone());
            self.output
                .largest
                .push(Row::file(id, &path, &stamp, alloc, "大文件"));
        }
        let mut hashes: BTreeMap<(u64, [u8; 32]), Vec<FileEntry>> = BTreeMap::new();
        let mut buckets: Vec<_> = std::mem::take(&mut self.dups)
            .into_iter()
            .filter(|(_, group)| group.len() > 1)
            .collect();
        buckets.sort_unstable_by_key(|(size, _)| *size);
        let mut allowance = DUP_BUDGET;
        'sizes: for (size, files) in buckets {
            if self.job.halted() {
                break;
            }
            if size.saturating_mul(files.len() as u64) > allowance {
                self.output.stats.duplicates_limited = true;
                break;
            }
            for entry in files {
                if self.job.halted() {
                    break 'sizes;
                }
                allowance -= size;
                if let Some(hash) = hash_verified(self.job, &entry) {
                    hashes.entry((size, hash)).or_default().push(entry);
                } else {
                    self.output.stats.duplicates_limited = true;
                }
            }
        }
        for (group, (_, paths)) in hashes
            .into_iter()
            .filter(|(_, files)| files.len() > 1)
            .rev()
            .enumerate()
        {
            for (path, stamp, allocated) in paths {
                self.count += 1;
                self.output.paths.insert(self.count, path.clone());
                let mut row = Row::file(self.count, &path, &stamp, allocated, "内容相同");
                row.group = Some(group as u64 + 1);
                self.output.duplicates.push(row);
            }
        }
        self.output
            .cleanup
            .sort_by_key(|p| std::cmp::Reverse(p.0.allocated.unwrap_or(p.0.logical)));
        if !self.job.halted() {
            if let Some(volume) = self.volume.take() {
                self.output.overview = Some(volume.finish(
                    self.output.stats.skipped,
                    self.output.stats.unknown_allocation,
                ));
            }
        }
        Ok(self.output)
    }
}
fn hash_verified(job: &Job, item: &FileEntry) -> Option<[u8; 32]> {
    let (path, stamp, _) = item;
    let _pins = win::pin_parents(path).ok()?;
    let mut file = win::open_file(path, true, 1).ok()?;
    if win::file_info(&file).ok()?.0 != *stamp {
        return None;
    }
    let mut digest = Sha256::new();
    let mut buf = [0; 64 * 1024];
    let mut read = 0;
    loop {
        if job.halted() {
            return None;
        }
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        read += n as u64;
        if read > stamp.len {
            return None;
        }
        digest.update(&buf[..n]);
    }
    (read == stamp.len && win::file_info(&file).ok()?.0 == *stamp).then(|| digest.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_dir() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "revo-scan-{}-{}",
            std::process::id(),
            super::super::serial()
        ));
        fs::create_dir(&root).unwrap();
        root
    }
    #[test]
    fn scan_deduplicates_links_and_requires_content_match() {
        let root = make_dir();
        let a = root.join("a");
        fs::write(&a, vec![17; 1024 * 1024]).unwrap();
        fs::hard_link(&a, root.join("alias")).unwrap();
        fs::write(root.join("same"), vec![17; 1024 * 1024]).unwrap();
        fs::write(root.join("different"), vec![29; 1024 * 1024]).unwrap();
        let r = Scanner::run(&Job::new(Some(root.clone()), true)).unwrap();
        assert_eq!(
            (r.stats.files, r.stats.aliases, r.duplicates.len()),
            (3, 1, 2)
        );
        for name in ["a", "alias", "same", "different"] {
            fs::remove_file(root.join(name)).unwrap();
        }
        fs::remove_dir(root).unwrap();
    }
    #[test]
    fn canceled_scan_and_pure_browser_profile_scope() {
        let root = make_dir();
        let job = Job::new(Some(root.clone()), true);
        job.cancel.store(true, Ordering::Relaxed);
        assert_eq!(Scanner::run(&job).unwrap().stats.files, 0);
        assert!(browser_profile(std::ffi::OsStr::new("Profile 42")));
        assert!(!browser_profile(std::ffi::OsStr::new("Profile ../../")));
        assert!(!temp_name(Path::new("documents.zip")));
        fs::remove_dir(root).unwrap();
    }
}
