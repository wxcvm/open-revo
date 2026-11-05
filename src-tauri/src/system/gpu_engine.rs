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
type PdhAddEnglishCounterWFn =
    unsafe extern "system" fn(isize, *const u16, usize, *mut isize) -> i32;
type PdhCloseQueryFn = unsafe extern "system" fn(isize) -> i32;
type PdhCollectQueryDataFn = unsafe extern "system" fn(isize) -> i32;
type PdhGetFormattedCounterArrayWFn =
    unsafe extern "system" fn(isize, u32, *mut u32, *mut u32, *mut u8) -> i32;

const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
const GPU_ENGINE_COUNTER: &str = r"\GPU Engine(*)\Utilization Percentage";

struct EnginePoll {
    close: PdhCloseQueryFn,
    query: isize,
    counter: isize,
    warm: bool,
    graphics_nodes: std::collections::HashMap<String, u32>,
}

impl Drop for EnginePoll {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.query);
        }
    }
}

struct PdhApi {
    close: PdhCloseQueryFn,
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
            close: *lib.get::<PdhCloseQueryFn>(b"PdhCloseQuery\0").ok()?,
            collect: *lib
                .get::<PdhCollectQueryDataFn>(b"PdhCollectQueryData\0")
                .ok()?,
            get_array: *lib
                .get::<PdhGetFormattedCounterArrayWFn>(b"PdhGetFormattedCounterArrayW\0")
                .ok()?,
            open_query: *lib.get::<PdhOpenQueryWFn>(b"PdhOpenQueryW\0").ok()?,
            add_counter: *lib
                .get::<PdhAddEnglishCounterWFn>(b"PdhAddEnglishCounterW\0")
                .ok()?,
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
                (api.close)(query);
                return 0;
            }
            // Prime the counter: PDH needs two collections before it yields a rate.
            (api.collect)(query);
        }
        *guard = Some(EnginePoll {
            close: api.close,
            query,
            counter,
            warm: false,
            graphics_nodes: Default::default(),
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
    let poll = guard.as_mut().expect("initialized GPU counters");
    unsafe { read_3d_sum(api, poll) }
}

unsafe fn read_3d_sum(api: &PdhApi, poll: &mut EnginePoll) -> u32 {
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
    let mut buffer = vec![0u64; (buffer_size as usize + 7) / 8];
    if (api.get_array)(
        poll.counter,
        PDH_FMT_DOUBLE,
        &mut buffer_size,
        &mut item_count,
        buffer.as_mut_ptr().cast(),
    ) != 0
    {
        return 0;
    }

    let item_size = std::mem::size_of::<PdhFmtCountervalueItem>();
    let mut total = 0.0f64;
    for index in 0..item_count as usize {
        let offset = index * item_size;
        if offset + item_size > buffer_size as usize {
            break;
        }
        let item = &*(buffer
            .as_ptr()
            .cast::<u8>()
            .add(offset)
            .cast::<PdhFmtCountervalueItem>());
        let name = instance_name(item.name).to_ascii_lowercase();
        if !name.contains("engtype_3d") {
            continue;
        }
        if let Some((adapter, node)) = graphics_instance(&name) {
            poll.graphics_nodes.insert(adapter, node);
        }
        if item.value.status == 0 && item.value.double_value.is_finite() {
            total += item.value.double_value;
        }
    }
    total.clamp(0.0, 100.0).round() as u32
}

// The PDH engine name identifies the 3D node, even on drivers that do not
// implement KMTQAITYPE_NODEMETADATA.
fn graphics_instance(name: &str) -> Option<(String, u32)> {
    let start = name.find("luid_")?;
    let (_, node) = name[start..].split_once("_eng_")?;
    let node = node.split('_').next()?.parse().ok()?;
    let end = name[start..].find("_eng_")? + start;
    Some((name[start..end].to_ascii_lowercase(), node))
}

pub fn graphics_node(instance: &str) -> Option<u32> {
    state()
        .lock()
        .as_ref()?
        .graphics_nodes
        .get(&instance.to_ascii_lowercase())
        .copied()
}

pub fn shared_memory_mb(instance: &str) -> Option<u64> {
    static COUNTER: Mutex<Option<(String, EnginePoll)>> = Mutex::new(None);
    let api = pdh_api()?;
    let mut counter = COUNTER.lock();
    if counter
        .as_ref()
        .is_none_or(|(key, _)| !key.eq_ignore_ascii_case(instance))
    {
        // Adapter identity only changes after a display-device change.
        counter.take();
        let mut query = 0;
        let mut value = 0;
        let path = wide(&format!(r"\GPU Adapter Memory({instance})\Shared Usage"));
        unsafe {
            if (api.open_query)(std::ptr::null(), 0, &mut query) != 0 {
                return None;
            }
            if (api.add_counter)(query, path.as_ptr(), 0, &mut value) != 0 {
                (api.close)(query);
                return None;
            }
        }
        *counter = Some((
            instance.to_string(),
            EnginePoll {
                close: api.close,
                query,
                counter: value,
                warm: true,
                graphics_nodes: Default::default(),
            },
        ));
    }
    let (_, poll) = counter.as_ref()?;
    unsafe {
        if (api.collect)(poll.query) != 0 {
            return None;
        }
        let mut bytes = 0;
        let mut count = 0;
        (api.get_array)(
            poll.counter,
            PDH_FMT_DOUBLE,
            &mut bytes,
            &mut count,
            std::ptr::null_mut(),
        );
        if count != 1 || bytes < std::mem::size_of::<PdhFmtCountervalueItem>() as u32 {
            return None;
        }
        // u64 backing guarantees the alignment required by the PDH array.
        let mut buffer = vec![0u64; (bytes as usize + 7) / 8];
        if (api.get_array)(
            poll.counter,
            PDH_FMT_DOUBLE,
            &mut bytes,
            &mut count,
            buffer.as_mut_ptr().cast(),
        ) != 0
        {
            return None;
        }
        let item = &*buffer.as_ptr().cast::<PdhFmtCountervalueItem>();
        if item.value.status > 1
            || !item.value.double_value.is_finite()
            || item.value.double_value < 0.0
        {
            return None;
        }
        Some((item.value.double_value / 1048576.0).round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn engine_identity() {
        assert_eq!(
            graphics_instance("pid_10_luid_0x00000000_0x00013d39_phys_0_eng_0_engtype_3d"),
            Some(("luid_0x00000000_0x00013d39_phys_0".into(), 0))
        );
        assert_eq!(graphics_instance("missing_node"), None);
    }
}
