//! Local, read-only Windows metrics. No WMI/PowerShell polling or GPU access.
use super::model::*;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE},
        NetworkManagement::{
            IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2},
            Ndis::IfOperStatusUp,
        },
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            Performance::*,
            ProcessStatus::{
                GetPerformanceInfo, K32GetProcessMemoryInfo, PERFORMANCE_INFORMATION,
                PROCESS_MEMORY_COUNTERS_EX, PROCESS_MEMORY_COUNTERS_EX2,
            },
            SystemInformation::GetPhysicallyInstalledSystemMemory,
            Threading::{
                GetActiveProcessorCount, GetProcessTimes, OpenProcess,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    },
};

struct Query(isize);
impl Query {
    fn open() -> Option<Self> {
        let mut query = 0;
        (unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } == 0).then(|| Self(query))
    }
    fn add(&self, path: &str) -> Option<isize> {
        let path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut counter = 0;
        (unsafe { PdhAddEnglishCounterW(self.0, PCWSTR(path.as_ptr()), 0, &mut counter) } == 0)
            .then_some(counter)
    }
    fn collect(&self) -> bool {
        unsafe { PdhCollectQueryData(self.0) == 0 }
    }
}
impl Drop for Query {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.0);
        }
    }
}

fn status_ok(status: u32) -> bool {
    matches!(status, 0 | 1)
}

fn formatted(counter: Option<isize>, fmt: PDH_FMT) -> Option<PDH_FMT_COUNTERVALUE_0> {
    let mut result = PDH_FMT_COUNTERVALUE::default();
    let rc = unsafe { PdhGetFormattedCounterValue(counter?, fmt, None, &mut result) };
    (rc == 0 && status_ok(result.CStatus)).then_some(result.Anonymous)
}
fn gauge(counter: Option<isize>) -> Option<u64> {
    u64::try_from(unsafe { formatted(counter, PDH_FMT_LARGE)?.largeValue }).ok()
}
fn valid_double(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value)
}
fn scalar_rate(counter: Option<isize>, warm: bool) -> Option<f64> {
    if !warm {
        return None;
    }
    valid_double(unsafe { formatted(counter, PDH_FMT_DOUBLE)?.doubleValue })
}

// The API supplies data including in-buffer name pointers. Allocate at the ABI's
// alignment, bound allocations and retries, and copy values before freeing it.
fn counter_array<T, R>(
    mut get: impl FnMut(&mut u32, &mut u32, Option<*mut T>) -> u32,
    mut extract: impl FnMut(&T) -> Option<(PWSTR, R)>,
) -> Option<HashMap<String, R>> {
    let (mut bytes, mut count) = (0, 0);
    if get(&mut bytes, &mut count, None) != PDH_MORE_DATA {
        return None;
    }
    for _ in 0..3 {
        if bytes == 0 || bytes > 4 * 1024 * 1024 {
            return None;
        }
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
        let data = buffer.as_mut_ptr().cast::<T>();
        match get(&mut bytes, &mut count, Some(data)) {
            PDH_MORE_DATA => continue,
            0 => {}
            _ => return None,
        }
        if count as usize > buffer.len() * 8 / std::mem::size_of::<T>() {
            return None;
        }
        let mut rows = HashMap::new();
        for item in unsafe { std::slice::from_raw_parts(data, count as usize) } {
            if let Some((name, value)) = extract(item) {
                if !name.is_null() {
                    if let Ok(name) = unsafe { name.to_string() } {
                        rows.insert(name, value);
                    }
                }
            }
        }
        return Some(rows);
    }
    None
}

fn rates(counter: Option<isize>) -> HashMap<String, f64> {
    let Some(counter) = counter else {
        return HashMap::new();
    };
    counter_array(
        |bytes, count, items| unsafe {
            PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, bytes, count, items)
        },
        |item: &PDH_FMT_COUNTERVALUE_ITEM_W| {
            if status_ok(item.FmtValue.CStatus) {
                Some((
                    item.szName,
                    valid_double(unsafe { item.FmtValue.Anonymous.doubleValue })?,
                ))
            } else {
                None
            }
        },
    )
    .unwrap_or_default()
}

struct SystemQuery {
    query: Query,
    previous: Option<Instant>,
    cpu: Option<isize>,
    disk: [Option<isize>; 4],
    memory: [Option<isize>; 9],
}
const DISK_METRICS: [&str; 4] = [
    "Disk Read Bytes/sec",
    "Disk Write Bytes/sec",
    "Disk Reads/sec",
    "Disk Writes/sec",
];
impl SystemQuery {
    fn open() -> Option<Self> {
        let query = Query::open()?;
        let cpu = query.add(r"\Processor Information(_Total)\% Processor Time");
        let disk = DISK_METRICS.map(|name| query.add(&format!(r"\PhysicalDisk(_Total)\{name}")));
        let memory = [
            "Available Bytes",
            "Standby Cache Reserve Bytes",
            "Standby Cache Normal Priority Bytes",
            "Standby Cache Core Bytes",
            "Free & Zero Page List Bytes",
            "Modified Page List Bytes",
            "Pool Nonpaged Bytes",
            "Pool Paged Resident Bytes",
            "System Cache Resident Bytes",
        ]
        .map(|name| query.add(&format!(r"\Memory\{name}")));
        Some(Self {
            query,
            previous: None,
            cpu,
            disk,
            memory,
        })
    }
    fn sample(&mut self, now: Instant, m: &mut Memory, summary: &mut Values) {
        let warm = self
            .previous
            .replace(now)
            .is_some_and(|p| now.duration_since(p).as_secs_f64() <= 20.0);
        if !self.query.collect() {
            self.previous = None;
            return;
        }
        summary[3] = scalar_rate(self.cpu, warm).filter(|v| *v <= 100.0);
        for (i, counter) in self.disk.iter().enumerate() {
            summary[i + 4] = scalar_rate(*counter, warm);
        }

        let [available, standby_r, standby_n, standby_c, free, modified, nonpaged, paged, cache] =
            self.memory.map(gauge);
        // If the same PDH snapshot agrees closely with the PSAPI total, use its
        // available count for the split instead of mixing independently sampled counters.
        if let Some((visible, pdh_available)) = m.visible.zip(available) {
            if pdh_available <= visible
                && m.available
                    .is_some_and(|v| v.abs_diff(pdh_available) <= 64 * 1024 * 1024)
            {
                m.available = Some(pdh_available);
                m.used = visible.checked_sub(pdh_available);
                let standby = standby_r
                    .zip(standby_n)
                    .zip(standby_c)
                    .and_then(|((r, n), c)| r.checked_add(n)?.checked_add(c));
                m.reconcile_available(standby, free);
            }
        }
        m.modified = modified;
        m.nonpaged_pool = nonpaged.or(m.nonpaged_pool);
        m.paged_pool_resident = paged;
        m.cache_resident = cache;
    }
}

fn memory(installed: Option<u64>) -> Memory {
    let mut m = Memory {
        installed,
        available_split_error: Some("没有同一采样的待机与空闲页数据"),
        ..Default::default()
    };
    let mut p = PERFORMANCE_INFORMATION::default();
    let size = std::mem::size_of_val(&p) as u32;
    p.cb = size;
    if unsafe { GetPerformanceInfo(&mut p, size) }.is_ok() && p.PageSize > 0 {
        let bytes = |pages: usize| (pages as u64).checked_mul(p.PageSize as u64);
        m.visible = bytes(p.PhysicalTotal).filter(|v| *v > 0);
        m.available =
            bytes(p.PhysicalAvailable).filter(|v| m.visible.is_some_and(|total| *v <= total));
        m.used = m
            .visible
            .zip(m.available)
            .and_then(|(t, a)| t.checked_sub(a));
        m.reserved = installed.zip(m.visible).and_then(|(i, v)| i.checked_sub(v));
        m.commit = bytes(p.CommitTotal);
        m.commit_limit = bytes(p.CommitLimit).filter(|v| *v > 0);
        m.nonpaged_pool = bytes(p.KernelNonpaged);
    }
    m
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn filetime(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

fn process_memory(handle: HANDLE) -> (Option<u64>, Option<u64>) {
    let mut full = PROCESS_MEMORY_COUNTERS_EX2::default();
    full.cb = std::mem::size_of_val(&full) as u32;
    if unsafe {
        K32GetProcessMemoryInfo(
            handle,
            (&mut full as *mut PROCESS_MEMORY_COUNTERS_EX2).cast(),
            full.cb,
        )
    }
    .as_bool()
    {
        return (
            Some(full.PrivateWorkingSetSize as u64),
            Some(full.PrivateUsage as u64),
        );
    }
    // Older builds support private commit, but that is not a substitute for residence.
    let mut basic = PROCESS_MEMORY_COUNTERS_EX::default();
    basic.cb = std::mem::size_of_val(&basic) as u32;
    let commit = unsafe {
        K32GetProcessMemoryInfo(
            handle,
            (&mut basic as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            basic.cb,
        )
    }
    .as_bool()
    .then_some(basic.PrivateUsage as u64);
    (None, commit)
}

#[derive(Default)]
struct ProcessQuery {
    prev_cpu: HashMap<(u32, u64), (Instant, u64)>,
}
impl ProcessQuery {
    fn sample(&mut self, logical_cpus: u32) -> Option<Vec<Process>> {
        let previous = std::mem::take(&mut self.prev_cpu);
        let snapshot = Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?);
        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        unsafe { Process32FirstW(snapshot.0, &mut entry) }.ok()?;
        let mut rows = Vec::new();
        loop {
            let pid = entry.th32ProcessID;
            if pid != 0 {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|n| *n == 0)
                    .unwrap_or(entry.szExeFile.len());
                let mut row = Process {
                    group: None,
                    key: None,
                    pid,
                    name: String::from_utf16_lossy(&entry.szExeFile[..end]),
                    private_resident: None,
                    private_commit: None,
                    cpu_percent: None,
                };
                if let Ok(handle) =
                    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
                {
                    let p = Handle(handle);
                    (row.private_resident, row.private_commit) = process_memory(p.0);
                    let now = Instant::now();
                    let (mut start, mut exit, mut kernel, mut user) = Default::default();
                    if unsafe {
                        GetProcessTimes(p.0, &mut start, &mut exit, &mut kernel, &mut user)
                    }
                    .is_ok()
                    {
                        let created = filetime(start);
                        if let Some(cpu_time) = filetime(kernel)
                            .checked_add(filetime(user))
                            .filter(|_| created != 0 && filetime(exit) == 0)
                        {
                            let id = (pid, created);
                            row.key = Some(format!("proc:{pid}:{created}"));
                            row.group = crate::system::manager::process_group(handle);
                            self.prev_cpu.insert(id, (now, cpu_time));
                            row.cpu_percent = previous.get(&id).and_then(|(at, cpu)| {
                                let pct =
                                    rate(*cpu, cpu_time, now.duration_since(*at).as_secs_f64())?
                                        / (f64::from(logical_cpus.max(1)) * 100_000.0);
                                (pct <= 100.0).then_some(pct)
                            });
                        }
                    }
                }
                rows.push(row);
            }
            if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
                break;
            }
        }
        rows.sort_unstable_by_key(|row| row.pid);
        Some(rows)
    }
}

struct DiskQuery {
    query: Query,
    counters: [Option<isize>; 4],
    warm: bool,
}
impl DiskQuery {
    fn open() -> Option<Self> {
        let query = Query::open()?;
        let counters = DISK_METRICS.map(|name| query.add(&format!(r"\PhysicalDisk(*)\{name}")));
        counters.iter().any(Option::is_some).then_some(Self {
            query,
            counters,
            warm: false,
        })
    }
    fn sample(&mut self) -> Option<Vec<Disk>> {
        let ok = self.query.collect();
        let warm = std::mem::replace(&mut self.warm, ok);
        if !ok || !warm {
            return None;
        }
        let [read, write, riops, wiops] = self.counters.map(rates);
        let names = read
            .keys()
            .chain(write.keys())
            .chain(riops.keys())
            .chain(wiops.keys())
            .filter(|n| !n.eq_ignore_ascii_case("_Total"))
            .collect::<HashSet<_>>();
        let mut rows = names
            .into_iter()
            .map(|n| Disk {
                key: format!("disk:{n}"),
                name: n.clone(),
                read: read.get(n).copied(),
                write: write.get(n).copied(),
                read_iops: riops.get(n).copied(),
                write_iops: wiops.get(n).copied(),
            })
            .collect::<Vec<_>>();
        rows.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        (!rows.is_empty()).then_some(rows)
    }
}

#[derive(Default)]
struct Network {
    previous: HashMap<u64, (u64, u64, bool, windows::core::GUID)>,
    active_physical: HashSet<u64>,
    previous_at: Option<Instant>,
}
impl Network {
    fn sample(
        &mut self,
        now: Instant,
        detail: bool,
        summary: &mut Values,
    ) -> Option<Vec<Interface>> {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if unsafe { GetIfTable2(&mut table) }.0 != 0 || table.is_null() {
            self.previous.clear();
            self.previous_at = None;
            return None;
        }
        struct Table(*mut MIB_IF_TABLE2);
        impl Drop for Table {
            fn drop(&mut self) {
                unsafe {
                    FreeMibTable(self.0.cast());
                }
            }
        }
        let guard = Table(table);
        let count = unsafe { (*guard.0).NumEntries as usize };
        if count > 8192 {
            self.previous.clear();
            self.previous_at = None;
            return None;
        }
        let raw = unsafe {
            std::slice::from_raw_parts(
                std::ptr::addr_of!((*guard.0).Table)
                    .cast::<windows::Win32::NetworkManagement::IpHelper::MIB_IF_ROW2>(),
                count,
            )
        };
        let mut interfaces = Vec::new();
        let mut next = HashMap::new();
        let mut active_physical = HashSet::new();
        let elapsed = self
            .previous_at
            .replace(now)
            .map(|p| now.duration_since(p).as_secs_f64());
        let mut aggregate = elapsed
            .filter(|v| (0.5..=20.0).contains(v))
            .map(|_| (0.0, 0.0));
        for row in raw {
            let luid = unsafe { row.InterfaceLuid.Value };
            let up = row.OperStatus == IfOperStatusUp;
            // First flag of MIB_IF_ROW2::InterfaceAndOperStatusFlags is HardwareInterface.
            let physical = row.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
            let previous = self
                .previous
                .get(&luid)
                .filter(|p| p.2 && up && p.3 == row.InterfaceGuid);
            let (rx, tx) = previous
                .zip(elapsed)
                .map(|(p, d)| (rate(p.0, row.InOctets, d), rate(p.1, row.OutOctets, d)))
                .unwrap_or((None, None));
            if physical && up {
                active_physical.insert(luid);
                aggregate = aggregate
                    .zip(rx.zip(tx))
                    .map(|((r, t), (a, b))| (r + a, t + b));
            }
            next.insert(luid, (row.InOctets, row.OutOctets, up, row.InterfaceGuid));
            if detail {
                let end = row
                    .Alias
                    .iter()
                    .position(|x| *x == 0)
                    .unwrap_or(row.Alias.len());
                interfaces.push(Interface {
                    key: format!("net:{luid}:{:?}", row.InterfaceGuid),
                    name: String::from_utf16_lossy(&row.Alias[..end]),
                    physical,
                    up,
                    rx,
                    tx,
                });
            }
        }
        self.previous = next;
        if self.active_physical == active_physical {
            if let Some((rx, tx)) = aggregate {
                summary[8] = Some(rx);
                summary[9] = Some(tx);
            }
        }
        self.active_physical = active_physical;
        Some(interfaces)
    }
}

pub struct Sampler {
    system: Option<SystemQuery>,
    detail_disks: Option<DiskQuery>,
    processes: Option<ProcessQuery>,
    network: Network,
    installed: Option<u64>,
    prev: Option<Instant>,
}
impl Sampler {
    pub fn new() -> Self {
        let mut kb = 0u64;
        let installed = unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) }
            .ok()
            .and_then(|()| kb.checked_mul(1024));
        Self {
            system: None,
            detail_disks: None,
            processes: None,
            network: Network::default(),
            installed,
            prev: None,
        }
    }
    pub fn clear_detail(&mut self) {
        self.detail_disks = None;
        self.processes = None;
    }
    pub fn reset(&mut self) {
        self.system = None;
        self.clear_detail();
        self.network = Network::default();
        self.prev = None;
    }
    pub fn collect(&mut self, now: Instant, wall_ms: u64, detail: bool) -> Snapshot {
        if self
            .prev
            .replace(now)
            .is_some_and(|p| now.duration_since(p).as_secs_f64() > 20.0)
        {
            self.reset();
            self.prev = Some(now);
        }
        let start = Instant::now();
        let mut m = memory(self.installed);
        let logical_cpus = unsafe { GetActiveProcessorCount(0xffff) }.max(1);
        if self.system.is_none() {
            self.system = SystemQuery::open();
        }
        let mut summary = [None; SERIES.len()];
        if let Some(query) = &mut self.system {
            query.sample(now, &mut m, &mut summary);
        }
        summary[0] = m.used.map(|n| n as f64);
        summary[1] = m.available.map(|n| n as f64);
        summary[2] = m.commit.map(|n| n as f64);
        let interfaces = self.network.sample(now, detail, &mut summary);
        let mut result = Snapshot {
            summary,
            memory: m,
            time_ms: wall_ms,
            logical_cpus,
            net_available: interfaces.is_some(),
            interfaces: interfaces.unwrap_or_default(),
            ..Default::default()
        };
        if detail {
            if self.processes.is_none() {
                self.processes = Some(ProcessQuery::default());
            }
            if let Some(query) = &mut self.processes {
                if let Some(rows) = query.sample(logical_cpus) {
                    result.process_source = Some("Windows PSAPI / GetProcessTimes");
                    result.memory.reconcile_processes(&rows);
                    result.processes = rows;
                }
            }
            if self.detail_disks.is_none() {
                self.detail_disks = DiskQuery::open();
            }
            if let Some(rows) = self.detail_disks.as_mut().and_then(DiskQuery::sample) {
                result.disk_detail_available = true;
                result.disks = rows;
            }
        } else {
            self.clear_detail();
        }
        result.sampled_in_ms = start.elapsed().as_millis() as u64;
        result
    }
}
