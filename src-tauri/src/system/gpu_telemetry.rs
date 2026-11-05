// Read-only WDDM telemetry. ABI definitions follow the Windows SDK d3dkmthk.h.
// No OEM service, AMD runtime, kernel driver installation or register writes.
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct GpuSample {
    pub name: String,
    pub temperature: u32,
    pub core_mhz: u32,
    pub memory_mhz: u32,
    pub shared_total_mb: u64,
    pub memory_instance: String,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AdapterInfo {
    handle: u32,
    luid_low: u32,
    luid_high: u32,
    sources: u32,
    precise_regions: i32,
}

#[repr(C)]
struct EnumAdapters {
    count: u32,
    adapters: *mut AdapterInfo,
}

#[repr(C)]
struct QueryAdapter {
    handle: u32,
    kind: u32,
    data: *mut c_void,
    size: u32,
}

#[repr(C)]
#[derive(Default)]
struct NodePerf {
    node: u32,
    physical_index: u32,
    frequency: u64,
    max_frequency: u64,
    max_frequency_oc: u64,
    voltage: u32,
    max_voltage: u32,
    max_voltage_oc: u32,
    max_transition_latency: u64,
}

#[repr(C)]
#[derive(Default)]
struct AdapterPerf {
    physical_index: u32,
    memory_frequency: u64,
    max_memory_frequency: u64,
    max_memory_frequency_oc: u64,
    memory_bandwidth: u64,
    pcie_bandwidth: u64,
    fan_rpm: u32,
    power_percent_tenths: u32,
    temperature_tenths_celsius: u32,
    power_state_override: u8,
}

#[repr(C)]
#[derive(Default)]
struct SegmentSizes {
    physical_index: u32,
    dedicated_video: u64,
    dedicated_system: u64,
    shared_system: u64,
    local: u64,
    nonlocal: u64,
    nonbudget: u64,
}

#[repr(C)]
struct NodeMetadata {
    node_and_adapter: u32,
    engine_type: u32,
    friendly_name: [u16; 32],
    flags: u32,
    gpu_mmu: u8,
    io_mmu: u8,
}

#[link(name = "gdi32")]
extern "system" {
    fn D3DKMTEnumAdapters2(adapters: *mut EnumAdapters) -> i32;
    fn D3DKMTQueryAdapterInfo(query: *const QueryAdapter) -> i32;
    fn D3DKMTCloseAdapter(handle: *const u32) -> i32;
}

fn query<T>(handle: u32, kind: u32, data: &mut T) -> bool {
    let args = QueryAdapter {
        handle,
        kind,
        data: (data as *mut T).cast(),
        size: std::mem::size_of::<T>() as u32,
    };
    unsafe { D3DKMTQueryAdapterInfo(&args) >= 0 }
}

fn mhz(hz: u64) -> u32 {
    let value = hz / 1_000_000;
    if value <= 20_000 {
        value as u32
    } else {
        0
    }
}

struct Adapters(Vec<AdapterInfo>);
impl Drop for Adapters {
    fn drop(&mut self) {
        for adapter in &self.0 {
            if adapter.handle != 0 {
                unsafe {
                    D3DKMTCloseAdapter(&adapter.handle);
                }
            }
        }
    }
}

fn read_sample(find_node: fn(&str) -> Option<u32>) -> GpuSample {
    let mut args = EnumAdapters {
        count: 0,
        adapters: std::ptr::null_mut(),
    };
    if unsafe { D3DKMTEnumAdapters2(&mut args) } < 0 || args.count == 0 || args.count > 256 {
        return GpuSample::default();
    }
    let mut adapters = Adapters(vec![AdapterInfo::default(); args.count as usize]);
    args.adapters = adapters.0.as_mut_ptr();
    if unsafe { D3DKMTEnumAdapters2(&mut args) } < 0 {
        return GpuSample::default();
    }
    for adapter in adapters.0.iter().take(args.count as usize) {
        let mut kind = 0u32;
        // Render-capable physical adapter only: skip software and USB/indirect displays.
        if !query(adapter.handle, 15, &mut kind) || kind & 1 == 0 || kind & (4 | 64) != 0 {
            continue;
        }
        let mut strings = [0u16; 260 * 4];
        if !query(adapter.handle, 8, &mut strings) {
            continue;
        }
        let end = strings[..260].iter().position(|&c| c == 0).unwrap_or(260);
        let name = String::from_utf16_lossy(&strings[..end]);
        // This path is only used for iGPUs. Do not accidentally sample a discrete card.
        let upper = name.to_uppercase();
        if name.is_empty() || upper.contains("NVIDIA") || upper.contains("RADEON RX") {
            continue;
        }
        let mut perf = AdapterPerf::default();
        let available = query(adapter.handle, 62, &mut perf);
        let mut sizes = SegmentSizes::default();
        let memory_available = query(adapter.handle, 42, &mut sizes);
        let instance = format!(
            "luid_0x{:08x}_0x{:08x}_phys_0",
            adapter.luid_high, adapter.luid_low
        );
        let mut core_mhz = 0;
        if let Some(ordinal) = find_node(&instance) {
            let mut node = NodePerf {
                node: ordinal,
                ..Default::default()
            };
            if query(adapter.handle, 61, &mut node) {
                core_mhz = mhz(node.frequency);
            }
        }
        // Select a 3D node by metadata rather than assuming node 0 is graphics.
        for ordinal in 0..if core_mhz == 0 { 32 } else { 0 } {
            let mut metadata: NodeMetadata = unsafe { std::mem::zeroed() };
            metadata.node_and_adapter = ordinal;
            if !query(adapter.handle, 25, &mut metadata) {
                break;
            }
            if metadata.engine_type == 1 {
                let mut node = NodePerf {
                    node: ordinal,
                    ..Default::default()
                };
                if query(adapter.handle, 61, &mut node) {
                    core_mhz = mhz(node.frequency);
                }
                break;
            }
        }
        return GpuSample {
            name,
            temperature: if available && (1..=2000).contains(&perf.temperature_tenths_celsius) {
                perf.temperature_tenths_celsius / 10
            } else {
                0
            },
            core_mhz,
            memory_mhz: if available {
                mhz(perf.memory_frequency)
            } else {
                0
            },
            shared_total_mb: if memory_available {
                sizes.shared_system / (1024 * 1024)
            } else {
                0
            },
            memory_instance: instance,
        };
    }
    GpuSample::default()
}

pub fn sample(find_node: fn(&str) -> Option<u32>) -> GpuSample {
    static CACHE: Mutex<Option<(Instant, GpuSample)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, snapshot)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(1) {
            return snapshot.clone();
        }
    }
    let snapshot = read_sample(find_node);
    *cache = Some((Instant::now(), snapshot.clone()));
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_struct_layouts() {
        assert_eq!(std::mem::size_of::<AdapterInfo>(), 20);
        assert_eq!(std::mem::size_of::<QueryAdapter>(), 24);
        assert_eq!(std::mem::size_of::<NodePerf>(), 56);
        assert_eq!(std::mem::size_of::<AdapterPerf>(), 64);
        assert_eq!(std::mem::size_of::<SegmentSizes>(), 56);
        assert_eq!(std::mem::size_of::<NodeMetadata>(), 80);
        assert_eq!(mhz(821_000_000), 821);
        assert_eq!(mhz(u64::MAX), 0);
    }
    #[test]
    fn read_only_live_sample() {
        println!("{:?}", sample(|_| Some(0)));
    }
}
