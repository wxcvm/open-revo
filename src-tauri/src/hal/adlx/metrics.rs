//! Read-only ADLX base-interface prefixes, independently mapped from official
//! IPerformanceMonitoring.h. No tracking, history, sampling or tuning setter is called.
use super::abi::{adlx_release, adlx_succeeded, AdlxHandle, AdlxSystemVtbl, AdlxUnknownVtbl};
use serde::Serialize;
use std::ffi::c_void;

#[derive(Clone, Debug, Default, Serialize)]
pub struct DriverMetrics {
    pub gpu_name: String,
    pub temperature_c: Option<f64>,
    pub core_mhz: Option<i32>,
    pub memory_mhz: Option<i32>,
}
type Flag = unsafe extern "system" fn(AdlxHandle, *mut u8) -> i32;
type Int = unsafe extern "system" fn(AdlxHandle, *mut i32) -> i32;
type Double = unsafe extern "system" fn(AdlxHandle, *mut f64) -> i32;
type GetGpu = unsafe extern "system" fn(AdlxHandle, AdlxHandle, *mut AdlxHandle) -> i32;
#[repr(C)]
struct PerfPrefix {
    base: AdlxUnknownVtbl,
    unused_slots_3_through_17: [*const c_void; 15],
    current_gpu: GetGpu, // slot 18
    unused_slots_19_through_20: [*const c_void; 2],
    supported_gpu: GetGpu, // slot 21
}
#[repr(C)]
struct SupportPrefix {
    base: AdlxUnknownVtbl,
    usage: Flag,
    clock: Flag,
    memory_clock: Flag,
    temperature: Flag,
    hotspot: Flag,
    power: Flag,
}
#[repr(C)]
struct MetricsPrefix {
    base: AdlxUnknownVtbl,
    timestamp: unsafe extern "system" fn(AdlxHandle, *mut i64) -> i32,
    usage: Double,
    clock: Int,
    memory_clock: Int,
    temperature: Double,
    hotspot: Double,
    power: Double,
}
struct Guard(AdlxHandle);
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            adlx_release(self.0);
        }
    }
}
unsafe fn acquire(rc: i32, ptr: AdlxHandle) -> Option<Guard> {
    if !ptr.is_null() {
        let guard = Guard(ptr);
        if adlx_succeeded(rc) {
            return Some(guard);
        }
    }
    None
}
unsafe fn supported(ptr: AdlxHandle, get: Flag) -> bool {
    let mut value = 0;
    adlx_succeeded(get(ptr, &mut value)) && value == 1
}
unsafe fn integer(ptr: AdlxHandle, get: Int) -> Option<i32> {
    let mut value = 0;
    if adlx_succeeded(get(ptr, &mut value)) && (0..=20000).contains(&value) {
        Some(value)
    } else {
        None
    }
}
/// Caller guarantees these are live borrowed session handles on the ADLX owner thread.
pub(super) unsafe fn read(system: AdlxHandle, gpu: AdlxHandle, name: &str) -> DriverMetrics {
    let mut out = DriverMetrics {
        gpu_name: name.into(),
        ..Default::default()
    };
    let system_vtbl = *(system as *const *const AdlxSystemVtbl);
    let mut raw = std::ptr::null_mut();
    let rc = ((*system_vtbl).get_performance_monitoring_services)(system, &mut raw);
    let Some(perf) = acquire(rc, raw) else {
        return out;
    };
    let p = *(perf.0 as *const *const PerfPrefix);
    if p.is_null() {
        return out;
    }
    let mut raw = std::ptr::null_mut();
    let rc = ((*p).supported_gpu)(perf.0, gpu, &mut raw);
    let Some(support) = acquire(rc, raw) else {
        return out;
    };
    let mut raw = std::ptr::null_mut();
    let rc = ((*p).current_gpu)(perf.0, gpu, &mut raw);
    let Some(metrics) = acquire(rc, raw) else {
        return out;
    };
    let s = *(support.0 as *const *const SupportPrefix);
    let m = *(metrics.0 as *const *const MetricsPrefix);
    if s.is_null() || m.is_null() {
        return out;
    }
    if supported(support.0, (*s).clock) {
        out.core_mhz = integer(metrics.0, (*m).clock);
    }
    if supported(support.0, (*s).memory_clock) {
        out.memory_mhz = integer(metrics.0, (*m).memory_clock);
    }
    if supported(support.0, (*s).temperature) {
        let mut value = 0.0;
        if adlx_succeeded(((*m).temperature)(metrics.0, &mut value))
            && value.is_finite()
            && (0.0..=150.0).contains(&value)
        {
            out.temperature_c = Some(value);
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn official_metric_slot_offsets() {
        let p = std::mem::size_of::<usize>();
        assert_eq!(std::mem::offset_of!(PerfPrefix, current_gpu), 18 * p);
        assert_eq!(std::mem::offset_of!(PerfPrefix, supported_gpu), 21 * p);
        assert_eq!(std::mem::offset_of!(MetricsPrefix, temperature), 7 * p);
        assert_eq!(std::mem::offset_of!(SupportPrefix, temperature), 6 * p);
    }
}
