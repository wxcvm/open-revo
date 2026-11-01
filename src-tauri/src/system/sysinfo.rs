// OpenRevo - System Memory & Volume Capacity Telemetry
// Zero-dependency raw kernel32 FFI collectors for the 系统监控 (System Monitor) dashboard.
// Provides RAM utilisation and per-fixed-volume capacity snapshots. Every failure path
// degrades to a zeroed snapshot so the telemetry heartbeat can never panic on exotic hosts.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const DRIVE_FIXED: u32 = 3;
/// Capacity refresh is throttled: disk enumeration touches every mounted volume.
const DISK_CACHE_TTL: Duration = Duration::from_secs(3);

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct MemoryStatusEx {
    length: u32,
    memory_load: u32,
    total_phys: u64,
    avail_phys: u64,
    total_page_file: u64,
    avail_page_file: u64,
    total_virtual: u64,
    avail_virtual: u64,
    avail_extended_virtual: u64,
}

type GlobalMemoryStatusExFn = unsafe extern "system" fn(*mut MemoryStatusEx) -> i32;
type GetLogicalDrivesFn = unsafe extern "system" fn() -> u32;
type GetDriveTypeWFn = unsafe extern "system" fn(*const u16) -> u32;
type GetDiskFreeSpaceExWFn =
    unsafe extern "system" fn(*const u16, *mut u64, *mut u64, *mut u64) -> i32;
type GetVolumeInformationWFn = unsafe extern "system" fn(
    *const u16,
    *mut u16,
    u32,
    *mut u32,
    *mut u32,
    *mut u32,
    *mut u16,
    u32,
) -> i32;

fn kernel32() -> Option<&'static libloading::Library> {
    static LIB: OnceLock<Option<&'static libloading::Library>> = OnceLock::new();
    *LIB.get_or_init(|| unsafe {
        libloading::Library::new("kernel32.dll")
            .ok()
            .map(|lib| Box::leak(Box::new(lib)) as &'static libloading::Library)
    })
}

macro_rules! kernel32_symbol {
    ($wrapper:ident, $ty:ty, $name:literal) => {
        fn $wrapper() -> Option<$ty> {
            static CACHE: OnceLock<Option<$ty>> = OnceLock::new();
            *CACHE.get_or_init(|| {
                let lib = kernel32()?;
                unsafe {
                    lib.get::<$ty>(concat!($name, "\0").as_bytes())
                        .ok()
                        .map(|symbol| *symbol)
                }
            })
        }
    };
}

kernel32_symbol!(
    global_memory_status_ex,
    GlobalMemoryStatusExFn,
    "GlobalMemoryStatusEx"
);
kernel32_symbol!(get_logical_drives, GetLogicalDrivesFn, "GetLogicalDrives");
kernel32_symbol!(get_drive_type_w, GetDriveTypeWFn, "GetDriveTypeW");
kernel32_symbol!(
    get_disk_free_space_ex_w,
    GetDiskFreeSpaceExWFn,
    "GetDiskFreeSpaceExW"
);
kernel32_symbol!(
    get_volume_information_w,
    GetVolumeInformationWFn,
    "GetVolumeInformationW"
);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemoryStats {
    pub total_mb: u64,
    pub used_mb: u64,
    pub free_mb: u64,
    /// 0..100 used ratio (frontend meter)
    pub usage: u32,
    /// Win32 MEMORYSTATUSEX.dwMemoryLoad (includes cache pressure, differs slightly)
    pub percent_load: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiskUsage {
    /// "C:", "D:" ...
    pub letter: String,
    /// Volume label, empty when none
    pub label: String,
    /// Filesystem name, e.g. "NTFS"
    pub fs: String,
    pub total_gb: f64,
    pub used_gb: f64,
    pub free_gb: f64,
    /// 0..100 used ratio
    pub usage: u32,
    pub is_system: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SystemStats {
    pub mem: MemoryStats,
    pub disks: Vec<DiskUsage>,
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|ch| *ch == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).trim().to_string()
}

fn bytes_to_gb(bytes: u64) -> f64 {
    ((bytes as f64 / GIB) * 10.0).round() / 10.0
}

fn ratio(part: f64, total: f64) -> u32 {
    if total <= 0.0 {
        return 0;
    }
    (((part / total) * 100.0).round() as u32).min(100)
}

/// Read live physical memory pressure (cheap: single syscall, no allocation beyond the struct).
pub fn read_memory() -> MemoryStats {
    let Some(global_memory_status_ex) = global_memory_status_ex() else {
        return MemoryStats::default();
    };
    let mut raw = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        ..Default::default()
    };
    if unsafe { global_memory_status_ex(&mut raw) } == 0 || raw.total_phys == 0 {
        return MemoryStats::default();
    }
    let total_mb = raw.total_phys / MIB;
    let free_mb = raw.avail_phys / MIB;
    let used_mb = total_mb.saturating_sub(free_mb);
    MemoryStats {
        total_mb,
        used_mb,
        free_mb,
        usage: ratio(used_mb as f64, total_mb as f64),
        percent_load: raw.memory_load.min(100),
    }
}

/// Enumerate fixed volumes (SSD/HDD partitions) with capacity, throttled by DISK_CACHE_TTL.
pub fn read_disks() -> Vec<DiskUsage> {
    static CACHE: OnceLock<Mutex<Option<(Instant, Vec<DiskUsage>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache.lock();
        if let Some((sampled_at, snapshot)) = guard.as_ref() {
            if sampled_at.elapsed() < DISK_CACHE_TTL {
                return snapshot.clone();
            }
        }
    }
    let fresh = probe_fixed_disks();
    if !fresh.is_empty() {
        *cache.lock() = Some((Instant::now(), fresh.clone()));
    }
    fresh
}

fn probe_fixed_disks() -> Vec<DiskUsage> {
    let mut disks = Vec::new();
    let (Some(get_logical_drives), Some(get_drive_type_w), Some(get_disk_free_space_ex_w)) = (
        get_logical_drives(),
        get_drive_type_w(),
        get_disk_free_space_ex_w(),
    ) else {
        return disks;
    };
    let mask = unsafe { get_logical_drives() };
    if mask == 0 {
        return disks;
    }
    let system_drive = system_drive_letter();
    for index in 0..26u32 {
        if mask & (1u32 << index) == 0 {
            continue;
        }
        let letter = (b'A' + index as u8) as char;
        let root = format!("{}:\\", letter);
        let root_wide = wide(&root);
        if unsafe { get_drive_type_w(root_wide.as_ptr()) } != DRIVE_FIXED {
            continue;
        }
        let (mut available_to_caller, mut total_bytes, mut free_bytes) = (0u64, 0u64, 0u64);
        let ok = unsafe {
            get_disk_free_space_ex_w(
                root_wide.as_ptr(),
                &mut available_to_caller,
                &mut total_bytes,
                &mut free_bytes,
            )
        };
        if ok == 0 || total_bytes == 0 {
            continue;
        }
        let total_gb = bytes_to_gb(total_bytes);
        let free_gb = bytes_to_gb(free_bytes);
        let used_gb = bytes_to_gb(total_bytes.saturating_sub(free_bytes));
        let (label, fs) = volume_identity(&root_wide);
        let tag = format!("{}:", letter);
        disks.push(DiskUsage {
            letter: tag.clone(),
            label,
            fs,
            total_gb,
            used_gb,
            free_gb,
            usage: ratio(used_gb, total_gb),
            is_system: tag.eq_ignore_ascii_case(&system_drive),
        });
    }
    disks.sort_by(|left, right| {
        right
            .is_system
            .cmp(&left.is_system)
            .then_with(|| left.letter.cmp(&right.letter))
    });
    disks
}

fn volume_identity(root_wide: &[u16]) -> (String, String) {
    let Some(get_volume_information_w) = get_volume_information_w() else {
        return (String::new(), String::new());
    };
    let mut label = [0u16; 64];
    let mut fs_name = [0u16; 16];
    let (mut serial, mut max_component, mut flags) = (0u32, 0u32, 0u32);
    let ok = unsafe {
        get_volume_information_w(
            root_wide.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            &mut serial,
            &mut max_component,
            &mut flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return (String::new(), String::new());
    }
    (wide_to_string(&label), wide_to_string(&fs_name))
}

fn system_drive_letter() -> String {
    let raw = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    let trimmed = raw.trim().trim_end_matches('\\');
    if trimmed.is_empty() {
        "C:".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Full snapshot consumed by the 系统监控 view (RAM + volumes).
pub fn collect() -> SystemStats {
    SystemStats {
        mem: read_memory(),
        disks: read_disks(),
    }
}
