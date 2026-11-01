// OpenRevo - Generic GPU Utilisation Telemetry (PDH GPU Engine counters)
// NVAPI only covers NVIDIA dGPUs. On AMD / Intel iGPU machines (e.g. Ryzen AI 9 HX 365
// with Radeon 880M) the 显卡 card would otherwise stay at 0 %. This module sums the
// Windows "GPU Engine" performance counters (english counter paths, locale independent)
// so any WDDM adapter reports a real utilisation figure.

use std::sync::OnceLock;

use parking_lot::Mutex;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PdhFmtCountervalue {
    status: u32,
    double_value: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PdhFmtCountervalueItem {
    name: *const u16,
    value: PdhFmtCountervalue,
}

type PdhOpenQueryWFn = unsafe extern "system" fn(*const u16, usize, *mut isize) -> i32;
type PdhAddEnglishCounterWFn = unsafe extern "system" fn(isize, *const u16, usize, *mut isize) -> i32;
type PdhCollectQueryDataFn = unsafe extern "system" fn(isize) -> i32;
type PdhGetFormattedCounterArrayWFn =
    unsafe extern "system" fn(isize, u32, *mut u32, *mut u32, *mut u8) -> i32;

const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
const GPU_ENGINE_COUNTER: &str = r"\GPU Engine(*)\Utilization Percentage";

struct EnginePoll {
    query: isize,
    counter: isize,
    warm: bool,
}

struct PdhApi {
    collect: PdhCollectQueryDataFn,
    get_array: PdhGetFormattedCounterArrayWFn,
    open_query: PdhOpenQueryWFn,
    add_counter: PdhAddEnglishCounterWFn,
}

fn pdh_api() -> Option<&'static PdhApi> {
    static API: OnceLock<Option<PdhApi>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let lib: &'static libloading::Library =
            Box::leak(Box::new(libloading::Library::new("pdh.dll").ok()?));
        Some(PdhApi {
            collect: *lib.get::<PdhCollectQueryDataFn>(b"PdhCollectQueryData\0").ok()?,
            get_array: *lib
                .get::<PdhGetFormattedCounterArrayWFn>(b"PdhGetFormattedCounterArrayW\0")
                .ok()?,
            open_query: *lib.get::<PdhOpenQueryWFn>(b"PdhOpenQueryW\0").ok()?,
            add_counter: *lib.get::<PdhAddEnglishCounterWFn>(b"PdhAddEnglishCounterW\0").ok()?,
        })
    })
    .as_ref()
}

fn state() -> &'static Mutex<Option<EnginePoll>> {
    static STATE: OnceLock<Mutex<Option<EnginePoll>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn instance_name(pointer: *const u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let mut length = 0usize;
    unsafe {
        while *pointer.add(length) != 0 && length < 256 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length))
    }
}

/// Sum every 3D engine instance of every adapter, clamped to 0..100.
pub fn sample_usage() -> u32 {
    let Some(api) = pdh_api() else { return 0 };
    let mut guard = state().lock();

    if guard.is_none() {
        let path = wide(GPU_ENGINE_COUNTER);
        let mut query = 0isize;
        let mut counter = 0isize;
        unsafe {
            if (api.open_query)(std::ptr::null(), 0, &mut query) != 0 {
                return 0;
            }
            if (api.add_counter)(query, path.as_ptr(), 0, &mut counter) != 0 {
                return 0;
            }
            // Prime the counter: PDH needs two collections before it yields a rate.
            (api.collect)(query);
        }
        *guard = Some(EnginePoll {
            query,
            counter,
            warm: false,
        });
        return 0;
    }

    let collect_failed = match guard.as_mut() {
        Some(poll) => unsafe { (api.collect)(poll.query) != 0 },
        None => true,
    };
    if collect_failed {
        *guard = None;
        return 0;
    }

    let poll = match guard.as_mut() {
        Some(poll) => poll,
        None => return 0,
    };
    if !poll.warm {
        poll.warm = true;
        return 0;
    }
    unsafe { read_3d_sum(api, poll) }
}

unsafe fn read_3d_sum(api: &PdhApi, poll: &EnginePoll) -> u32 {
    let mut buffer_size = 0u32;
    let mut item_count = 0u32;
    // Size probe intentionally returns PDH_MORE_DATA (0x80000712) with the required size.
    let _ = (api.get_array)(
        poll.counter,
        PDH_FMT_DOUBLE,
        &mut buffer_size,
        &mut item_count,
        std::ptr::null_mut(),
    );
    if buffer_size == 0 || item_count == 0 || item_count > 4096 {
        return 0;
    }
    let mut buffer = vec![0u8; buffer_size as usize];
    if (api.get_array)(
        poll.counter,
        PDH_FMT_DOUBLE,
        &mut buffer_size,
        &mut item_count,
        buffer.as_mut_ptr(),
    ) != 0
    {
        return 0;
    }

    let item_size = std::mem::size_of::<PdhFmtCountervalueItem>();
    let mut total = 0.0f64;
    for index in 0..item_count as usize {
        let offset = index * item_size;
        if offset + item_size > buffer.len() {
            break;
        }
        let item = &*(buffer.as_ptr().add(offset) as *const PdhFmtCountervalueItem);
        if !instance_name(item.name).contains("engtype_3D") {
            continue;
        }
        if item.value.status == 0 && item.value.double_value.is_finite() {
            total += item.value.double_value;
        }
    }
    total.clamp(0.0, 100.0).round() as u32
}
