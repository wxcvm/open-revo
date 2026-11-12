//! AMD ADLX (AMD Display Library eXtended) minimal C ABI bindings.
//!
//! PROVENANCE / SCOPE
//! ------------------
//! Every layout and every vtable slot order below was transcribed by hand from the official
//! C-language declarations shipped with the ADLX SDK headers used for validation on this
//! machine:
//!
//!   <headers>/ADLX.h                  - module exports ADLXQueryFullVersion / ADLXInitialize /
//!                                       ADLXTerminate, their calling conventions and typedefs.
//!   <headers>/ADLXDefines.h           - ADLX_RESULT enum values, adlx_bool / adlx_int / adlx_uint
//!                                       typedefs, ADLX_STD_CALL (__stdcall) and the
//!                                       IADLXInterfaceVtbl (Acquire, Release, QueryInterface).
//!   <headers>/ADLXStructures.h        - ADLX_IntRange { minValue, maxValue, step }.
//!   <headers>/ISystem.h               - IADLXSystemVtbl, IADLXGPUVtbl, IADLXGPUListVtbl.
//!   <headers>/ICollections.h          - IADLXListVtbl ordering inherited by IADLXGPUList.
//!   <headers>/I3DSettings.h           - IADLX3DSettingsServicesVtbl and the
//!                                       AntiLag / Chill / ImageSharpening / EnhancedSync /
//!                                       FrameRateTargetControl vtables.
//!   <headers>/IGPUTuning.h            - IADLXGPUTuningServicesVtbl (support queries only).
//!
//! Only the base (ADLX 1.x) interfaces are used; no extension interfaces are obtained, so the
//! bindings do not depend on any version-gated interface. No SDK source file, header or binary is
//! vendored into this repository: this module is an independent transcription of the published
//! C ABI. The SDK headers stay outside the tree (validation folder only).
//!
//! ABI NOTES
//! ---------
//! * `adlx_bool` is `bool` in C++ and `adlx_uint8` in C. Both are one byte; Rust `u8` is used for
//!   out-parameters and `u8` (0/1) for in-parameters to avoid Rust `bool` validity requirements on
//!   values produced by foreign code.
//! * `adlx_int` is `int32_t`, `adlx_uint` is `uint32_t`, `adlx_long` is Windows C `long` (32-bit even on x64 / LLP64).
//! * All interface methods use `__stdcall`; on x64 Windows that is the single platform calling
//!   convention, declared here as `extern "system"`. The three exported functions are `__cdecl`
//!   (`extern "C"`).
//! * Every interface object begins with a pointer to its vtable, and reference-counted vtables start with the
//!   three IADLXInterface slots (Acquire, Release, QueryInterface). `AdlxUnknownVtbl` relies on
//!   that documented prefix for type-erased `Release`. IADLXSystem is a non-refcounted
//!   singleton with its own vtable layout and must never use this prefix.

#![allow(dead_code, non_snake_case, clippy::too_many_arguments)]

use std::ffi::{c_char, c_void};

// ---------------------------------------------------------------------------------------------
// ADLXDefines.h : ADLX_RESULT enumeration (values are positional; order matters)
// ---------------------------------------------------------------------------------------------
pub type AdlxResult = i32;

pub const ADLX_OK: AdlxResult = 0;
pub const ADLX_ALREADY_ENABLED: AdlxResult = 1;
pub const ADLX_ALREADY_INITIALIZED: AdlxResult = 2;
pub const ADLX_FAIL: AdlxResult = 3;
pub const ADLX_INVALID_ARGS: AdlxResult = 4;
pub const ADLX_BAD_VER: AdlxResult = 5;
pub const ADLX_UNKNOWN_INTERFACE: AdlxResult = 6;
pub const ADLX_TERMINATED: AdlxResult = 7;
pub const ADLX_ADL_INIT_ERROR: AdlxResult = 8;
pub const ADLX_NOT_FOUND: AdlxResult = 9;
pub const ADLX_INVALID_OBJECT: AdlxResult = 10;
pub const ADLX_ORPHAN_OBJECTS: AdlxResult = 11;
pub const ADLX_NOT_SUPPORTED: AdlxResult = 12;
pub const ADLX_PENDING_OPERATION: AdlxResult = 13;
pub const ADLX_GPU_INACTIVE: AdlxResult = 14;
pub const ADLX_GPU_IN_USE: AdlxResult = 15;
pub const ADLX_TIMEOUT_OPERATION: AdlxResult = 16;
pub const ADLX_NOT_ACTIVE: AdlxResult = 17;
pub const ADLX_RESET_NEEDED: AdlxResult = 18;

/// ADLX_SUCCEEDED(x): ADLX_OK, ADLX_ALREADY_ENABLED, ADLX_ALREADY_INITIALIZED.
pub fn adlx_succeeded(rc: AdlxResult) -> bool {
    rc == ADLX_OK || rc == ADLX_ALREADY_ENABLED || rc == ADLX_ALREADY_INITIALIZED
}

/// ADLX_FAILED(x).
pub fn adlx_failed(rc: AdlxResult) -> bool {
    !adlx_succeeded(rc)
}

/// Human readable result code, used for honest error reporting in the UI.
pub fn adlx_result_name(rc: AdlxResult) -> &'static str {
    match rc {
        ADLX_OK => "ADLX_OK",
        ADLX_ALREADY_ENABLED => "ADLX_ALREADY_ENABLED",
        ADLX_ALREADY_INITIALIZED => "ADLX_ALREADY_INITIALIZED",
        ADLX_FAIL => "ADLX_FAIL",
        ADLX_INVALID_ARGS => "ADLX_INVALID_ARGS",
        ADLX_BAD_VER => "ADLX_BAD_VER",
        ADLX_UNKNOWN_INTERFACE => "ADLX_UNKNOWN_INTERFACE",
        ADLX_TERMINATED => "ADLX_TERMINATED",
        ADLX_ADL_INIT_ERROR => "ADLX_ADL_INIT_ERROR",
        ADLX_NOT_FOUND => "ADLX_NOT_FOUND",
        ADLX_INVALID_OBJECT => "ADLX_INVALID_OBJECT",
        ADLX_ORPHAN_OBJECTS => "ADLX_ORPHAN_OBJECTS",
        ADLX_NOT_SUPPORTED => "ADLX_NOT_SUPPORTED",
        ADLX_PENDING_OPERATION => "ADLX_PENDING_OPERATION",
        ADLX_GPU_INACTIVE => "ADLX_GPU_INACTIVE",
        ADLX_GPU_IN_USE => "ADLX_GPU_IN_USE",
        ADLX_TIMEOUT_OPERATION => "ADLX_TIMEOUT_OPERATION",
        ADLX_NOT_ACTIVE => "ADLX_NOT_ACTIVE",
        ADLX_RESET_NEEDED => "ADLX_RESET_NEEDED",
        _ => "ADLX_UNKNOWN_RESULT",
    }
}

// ---------------------------------------------------------------------------------------------
// ADLXStructures.h : ADLX_IntRange { adlx_int minValue; adlx_int maxValue; adlx_int step; }
// ---------------------------------------------------------------------------------------------
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdlxIntRange {
    pub minValue: i32,
    pub maxValue: i32,
    pub step: i32,
}

// ---------------------------------------------------------------------------------------------
// ADLXDefines.h : IADLXInterfaceVtbl { Acquire, Release, QueryInterface }
// ---------------------------------------------------------------------------------------------
#[repr(C)]
pub struct AdlxUnknownVtbl {
    pub acquire: unsafe extern "system" fn(*mut c_void) -> i32,
    pub release: unsafe extern "system" fn(*mut c_void) -> i32,
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> AdlxResult,
}

/// Type-erased handle to any ADLX interface (its first field is the vtable pointer).
pub type AdlxHandle = *mut c_void;

/// Calls IADLXInterface::Release through the documented vtable prefix.
///
/// # Safety
/// `handle` must be a live ADLX interface pointer of any kind, only released once.
pub unsafe fn adlx_release(handle: AdlxHandle) -> i32 {
    if handle.is_null() {
        return 0;
    }
    let vtbl = *(handle as *const *const AdlxUnknownVtbl);
    if vtbl.is_null() {
        return 0;
    }
    ((*vtbl).release)(handle)
}

/// IADLXInterface::QueryInterface for an extension interface id, used only for diagnostics.
///
/// # Safety
/// `handle` must be a live ADLX interface pointer.
pub unsafe fn adlx_query_interface(handle: AdlxHandle, iid: &[u16]) -> (AdlxResult, AdlxHandle) {
    let mut out: AdlxHandle = std::ptr::null_mut();
    if handle.is_null() {
        return (ADLX_INVALID_ARGS, out);
    }
    let vtbl = *(handle as *const *const AdlxUnknownVtbl);
    if vtbl.is_null() {
        return (ADLX_INVALID_OBJECT, out);
    }
    let rc = ((*vtbl).query_interface)(handle, iid.as_ptr(), &mut out);
    (rc, out)
}

// ---------------------------------------------------------------------------------------------
// ADLX.h : module exports (__cdecl)
//   ADLX_RESULT ADLXQueryFullVersion (adlx_uint64* fullVersion);
//   ADLX_RESULT ADLXInitialize (adlx_uint64 version, IADLXSystem** ppSystem);
//   ADLX_RESULT ADLXTerminate ();
//   ADLX_RESULT ADLXQueryVersion (const char** version);
// ---------------------------------------------------------------------------------------------
pub type AdlxQueryFullVersionFn = unsafe extern "C" fn(*mut u64) -> AdlxResult;
pub type AdlxQueryVersionFn = unsafe extern "C" fn(*mut *const c_char) -> AdlxResult;
pub type AdlxInitializeFn = unsafe extern "C" fn(u64, *mut AdlxHandle) -> AdlxResult;
pub type AdlxTerminateFn = unsafe extern "C" fn() -> AdlxResult;

pub const EXPORT_QUERY_FULL_VERSION: &[u8] = b"ADLXQueryFullVersion\0";
pub const EXPORT_QUERY_VERSION: &[u8] = b"ADLXQueryVersion\0";
pub const EXPORT_INITIALIZE: &[u8] = b"ADLXInitialize\0";
pub const EXPORT_TERMINATE: &[u8] = b"ADLXTerminate\0";

/// ADLX encodes the full version as four packed 16-bit fields, most significant first:
/// major << 48 | minor << 32 | build << 16 | revision.
pub fn decode_full_version(full: u64) -> (u16, u16, u16, u16) {
    (
        ((full >> 48) & 0xFFFF) as u16,
        ((full >> 32) & 0xFFFF) as u16,
        ((full >> 16) & 0xFFFF) as u16,
        (full & 0xFFFF) as u16,
    )
}

// ---------------------------------------------------------------------------------------------
// IADLXGPU vtable (ISystem.h) - only Name/VendorId/UniqueId are used, but the full slot order is
// reproduced so no offset is guessed.
// ---------------------------------------------------------------------------------------------
pub type FnGetCharPtr = unsafe extern "system" fn(*mut c_void, *mut *const c_char) -> AdlxResult;

#[repr(C)]
pub struct AdlxGpuVtbl {
    pub acquire: unsafe extern "system" fn(*mut c_void) -> i32,
    pub release: unsafe extern "system" fn(*mut c_void) -> i32,
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> AdlxResult,
    pub vendor_id: FnGetCharPtr,
    pub asic_family_type: unsafe extern "system" fn(*mut c_void, *mut i32) -> AdlxResult,
    pub gpu_type: unsafe extern "system" fn(*mut c_void, *mut i32) -> AdlxResult,
    pub is_external: unsafe extern "system" fn(*mut c_void, *mut u8) -> AdlxResult,
    pub name: FnGetCharPtr,
    pub driver_path: FnGetCharPtr,
    pub pnp_string: FnGetCharPtr,
    pub has_desktops: unsafe extern "system" fn(*mut c_void, *mut u8) -> AdlxResult,
    pub total_vram: unsafe extern "system" fn(*mut c_void, *mut u32) -> AdlxResult,
    pub vram_type: FnGetCharPtr,
    pub bios_info: unsafe extern "system" fn(
        *mut c_void,
        *mut *const c_char,
        *mut *const c_char,
        *mut *const c_char,
    ) -> AdlxResult,
    pub device_id: FnGetCharPtr,
    pub revision_id: FnGetCharPtr,
    pub subsystem_id: FnGetCharPtr,
    pub subsystem_vendor_id: FnGetCharPtr,
    pub unique_id: unsafe extern "system" fn(*mut c_void, *mut i32) -> AdlxResult,
}

#[repr(C)]
pub struct AdlxGpu {
    pub vtbl: *const AdlxGpuVtbl,
}

// ---------------------------------------------------------------------------------------------
// IADLXGPUList vtable (ISystem.h, inherits the IADLXList slots from ICollections.h)
// ---------------------------------------------------------------------------------------------
#[repr(C)]
pub struct AdlxGpuListVtbl {
    pub acquire: unsafe extern "system" fn(*mut c_void) -> i32,
    pub release: unsafe extern "system" fn(*mut c_void) -> i32,
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> AdlxResult,
    pub size: unsafe extern "system" fn(*mut c_void) -> u32,
    pub empty: unsafe extern "system" fn(*mut c_void) -> u8,
    pub begin: unsafe extern "system" fn(*mut c_void) -> u32,
    pub end: unsafe extern "system" fn(*mut c_void) -> u32,
    pub at: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> AdlxResult,
    pub clear: unsafe extern "system" fn(*mut c_void) -> AdlxResult,
    pub remove_back: unsafe extern "system" fn(*mut c_void) -> AdlxResult,
    pub add_back: unsafe extern "system" fn(*mut c_void, *mut c_void) -> AdlxResult,
    pub at_gpu_list: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> AdlxResult,
    pub add_back_gpu_list: unsafe extern "system" fn(*mut c_void, *mut c_void) -> AdlxResult,
}

#[repr(C)]
pub struct AdlxGpuList {
    pub vtbl: *const AdlxGpuListVtbl,
}

// ---------------------------------------------------------------------------------------------
// IADLXGPUTuningServices vtable (IGPUTuning.h). Used strictly for support queries; no tuning
// setter is bound because the driver reports all manual tuning paths unsupported on this GPU.
// ---------------------------------------------------------------------------------------------
pub type FnIsSupportedForGpu =
    unsafe extern "system" fn(*mut c_void, *mut c_void, *mut u8) -> AdlxResult;

#[repr(C)]
pub struct AdlxGpuTuningServicesVtbl {
    pub acquire: unsafe extern "system" fn(*mut c_void) -> i32,
    pub release: unsafe extern "system" fn(*mut c_void) -> i32,
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> AdlxResult,
    pub get_gpu_tuning_changed_handling:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> AdlxResult,
    pub is_at_factory: FnIsSupportedForGpu,
    pub reset_to_factory: unsafe extern "system" fn(*mut c_void, *mut c_void) -> AdlxResult,
    pub is_supported_auto_tuning: FnIsSupportedForGpu,
    pub is_supported_preset_tuning: FnIsSupportedForGpu,
    pub is_supported_manual_gfx_tuning: FnIsSupportedForGpu,
    pub is_supported_manual_vram_tuning: FnIsSupportedForGpu,
    pub is_supported_manual_fan_tuning: FnIsSupportedForGpu,
    pub is_supported_manual_power_tuning: FnIsSupportedForGpu,
    pub get_auto_tuning:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> AdlxResult,
    pub get_preset_tuning:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> AdlxResult,
    pub get_manual_gfx_tuning:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> AdlxResult,
    pub get_manual_vram_tuning:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> AdlxResult,
    pub get_manual_fan_tuning:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> AdlxResult,
    pub get_manual_power_tuning:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> AdlxResult,
}

#[repr(C)]
pub struct AdlxGpuTuningServices {
    pub vtbl: *const AdlxGpuTuningServicesVtbl,
}

// ---------------------------------------------------------------------------------------------
// IADLXSystem singleton vtable (ISystem.h) - no reference-counting prefix.
// ---------------------------------------------------------------------------------------------
#[repr(C)]
pub struct AdlxSystemVtbl {
    pub get_hybrid_graphics_type: unsafe extern "system" fn(*mut c_void, *mut i32) -> AdlxResult,
    pub get_gpus: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> AdlxResult,
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> AdlxResult,
    pub get_displays_services:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> AdlxResult,
    pub get_desktops_services:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> AdlxResult,
    pub get_gpus_changed_handling:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> AdlxResult,
    pub enable_log:
        unsafe extern "system" fn(*mut c_void, i32, i32, *mut c_void, *const u16) -> AdlxResult,
    pub get_3d_settings_services:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> AdlxResult,
    pub get_gpu_tuning_services:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> AdlxResult,
    pub get_performance_monitoring_services:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> AdlxResult,
    pub total_system_ram: unsafe extern "system" fn(*mut c_void, *mut u32) -> AdlxResult,
    pub get_i2c:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> AdlxResult,
}

#[repr(C)]
pub struct AdlxSystem {
    pub vtbl: *const AdlxSystemVtbl,
}

// ---------------------------------------------------------------------------------------------
// IADLX3D* toggle interfaces (I3DSettings.h). AntiLag and EnhancedSync have
// exactly the same three slots after IADLXInterface.
// ---------------------------------------------------------------------------------------------
#[repr(C)]
pub struct Adlx3DToggleVtbl {
    pub acquire: unsafe extern "system" fn(*mut c_void) -> i32,
    pub release: unsafe extern "system" fn(*mut c_void) -> i32,
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> AdlxResult,
    pub is_supported: unsafe extern "system" fn(*mut c_void, *mut u8) -> AdlxResult,
    pub is_enabled: unsafe extern "system" fn(*mut c_void, *mut u8) -> AdlxResult,
    pub set_enabled: unsafe extern "system" fn(*mut c_void, u8) -> AdlxResult,
}

#[repr(C)]
pub struct Adlx3DAntiLag {
    pub vtbl: *const Adlx3DToggleVtbl,
}

// ImageSharpening has two getter slots BEFORE SetEnabled; it cannot share the
// AntiLag setter layout. Calling slot 5 as SetEnabled passes a bool as an output pointer.
#[repr(C)]
pub struct Adlx3DImageSharpeningVtbl {
    pub acquire: unsafe extern "system" fn(*mut c_void) -> i32,
    pub release: unsafe extern "system" fn(*mut c_void) -> i32,
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> AdlxResult,
    pub is_supported: unsafe extern "system" fn(*mut c_void, *mut u8) -> AdlxResult,
    pub is_enabled: unsafe extern "system" fn(*mut c_void, *mut u8) -> AdlxResult,
    pub get_sharpness_range:
        unsafe extern "system" fn(*mut c_void, *mut AdlxIntRange) -> AdlxResult,
    pub get_sharpness: unsafe extern "system" fn(*mut c_void, *mut i32) -> AdlxResult,
    pub set_enabled: unsafe extern "system" fn(*mut c_void, u8) -> AdlxResult,
    pub set_sharpness: unsafe extern "system" fn(*mut c_void, i32) -> AdlxResult,
}

#[repr(C)]
pub struct Adlx3DImageSharpening {
    pub vtbl: *const Adlx3DImageSharpeningVtbl,
}

#[repr(C)]
pub struct Adlx3DEnhancedSync {
    pub vtbl: *const Adlx3DToggleVtbl,
}

// ---------------------------------------------------------------------------------------------
// IADLX3DChill vtable (I3DSettings.h):
//   IsSupported, IsEnabled, GetFPSRange, GetMinFPS, GetMaxFPS, SetEnabled, SetMinFPS, SetMaxFPS
// ---------------------------------------------------------------------------------------------
#[repr(C)]
pub struct Adlx3DChillVtbl {
    pub acquire: unsafe extern "system" fn(*mut c_void) -> i32,
    pub release: unsafe extern "system" fn(*mut c_void) -> i32,
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> AdlxResult,
    pub is_supported: unsafe extern "system" fn(*mut c_void, *mut u8) -> AdlxResult,
    pub is_enabled: unsafe extern "system" fn(*mut c_void, *mut u8) -> AdlxResult,
    pub get_fps_range: unsafe extern "system" fn(*mut c_void, *mut AdlxIntRange) -> AdlxResult,
    pub get_min_fps: unsafe extern "system" fn(*mut c_void, *mut i32) -> AdlxResult,
    pub get_max_fps: unsafe extern "system" fn(*mut c_void, *mut i32) -> AdlxResult,
    pub set_enabled: unsafe extern "system" fn(*mut c_void, u8) -> AdlxResult,
    pub set_min_fps: unsafe extern "system" fn(*mut c_void, i32) -> AdlxResult,
    pub set_max_fps: unsafe extern "system" fn(*mut c_void, i32) -> AdlxResult,
}

#[repr(C)]
pub struct Adlx3DChill {
    pub vtbl: *const Adlx3DChillVtbl,
}

// ---------------------------------------------------------------------------------------------
// IADLX3DFrameRateTargetControl vtable (I3DSettings.h):
//   IsSupported, IsEnabled, GetFPSRange, GetFPS, SetEnabled, SetFPS
// ---------------------------------------------------------------------------------------------
#[repr(C)]
pub struct Adlx3DFrameRateTargetControlVtbl {
    pub acquire: unsafe extern "system" fn(*mut c_void) -> i32,
    pub release: unsafe extern "system" fn(*mut c_void) -> i32,
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> AdlxResult,
    pub is_supported: unsafe extern "system" fn(*mut c_void, *mut u8) -> AdlxResult,
    pub is_enabled: unsafe extern "system" fn(*mut c_void, *mut u8) -> AdlxResult,
    pub get_fps_range: unsafe extern "system" fn(*mut c_void, *mut AdlxIntRange) -> AdlxResult,
    pub get_fps: unsafe extern "system" fn(*mut c_void, *mut i32) -> AdlxResult,
    pub set_enabled: unsafe extern "system" fn(*mut c_void, u8) -> AdlxResult,
    pub set_fps: unsafe extern "system" fn(*mut c_void, i32) -> AdlxResult,
}

#[repr(C)]
pub struct Adlx3DFrameRateTargetControl {
    pub vtbl: *const Adlx3DFrameRateTargetControlVtbl,
}

// ---------------------------------------------------------------------------------------------
// IADLX3DSettingsServices vtable (I3DSettings.h) - all fourteen slots in declaration order. Only
// the five settings this module exposes are called, the rest are kept so offsets stay verifiable.
// ---------------------------------------------------------------------------------------------
pub type FnGet3DSetting =
    unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> AdlxResult;

#[repr(C)]
pub struct Adlx3DSettingsServicesVtbl {
    pub acquire: unsafe extern "system" fn(*mut c_void) -> i32,
    pub release: unsafe extern "system" fn(*mut c_void) -> i32,
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> AdlxResult,
    pub get_anti_lag: FnGet3DSetting,
    pub get_chill: FnGet3DSetting,
    pub get_boost: FnGet3DSetting,
    pub get_image_sharpening: FnGet3DSetting,
    pub get_enhanced_sync: FnGet3DSetting,
    pub get_wait_for_vertical_refresh: FnGet3DSetting,
    pub get_frame_rate_target_control: FnGet3DSetting,
    pub get_anti_aliasing: FnGet3DSetting,
    pub get_morphological_anti_aliasing: FnGet3DSetting,
    pub get_anisotropic_filtering: FnGet3DSetting,
    pub get_tessellation: FnGet3DSetting,
    pub get_radeon_super_resolution:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> AdlxResult,
    pub get_reset_shader_cache: FnGet3DSetting,
    pub get_3d_settings_changed_handling:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> AdlxResult,
}

#[repr(C)]
pub struct Adlx3DSettingsServices {
    pub vtbl: *const Adlx3DSettingsServicesVtbl,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_codes_match_header_enum_order() {
        assert_eq!(ADLX_OK, 0);
        assert_eq!(ADLX_ALREADY_ENABLED, 1);
        assert_eq!(ADLX_ALREADY_INITIALIZED, 2);
        assert_eq!(ADLX_FAIL, 3);
        assert_eq!(ADLX_INVALID_ARGS, 4);
        assert_eq!(ADLX_BAD_VER, 5);
        assert_eq!(ADLX_UNKNOWN_INTERFACE, 6);
        assert_eq!(ADLX_NOT_SUPPORTED, 12);
        assert_eq!(ADLX_RESET_NEEDED, 18);
        assert!(adlx_succeeded(ADLX_OK));
        assert!(adlx_succeeded(ADLX_ALREADY_ENABLED));
        assert!(!adlx_succeeded(ADLX_NOT_SUPPORTED));
        assert!(adlx_failed(ADLX_NOT_SUPPORTED));
        assert!(!adlx_failed(ADLX_ALREADY_INITIALIZED));
    }

    #[test]
    fn full_version_decodes_like_the_validation_probe() {
        // adlx-3d-capabilities.log: version=281496451547260 on ADLX 1.5.0.124.
        assert_eq!(decode_full_version(281_496_451_547_260), (1, 5, 0, 124));
    }

    #[test]
    fn int_range_layout_is_three_i32() {
        assert_eq!(std::mem::size_of::<AdlxIntRange>(), 12);
    }

    #[test]
    fn interface_objects_are_one_pointer() {
        assert_eq!(
            std::mem::size_of::<AdlxSystem>(),
            std::mem::size_of::<usize>()
        );
        assert_eq!(std::mem::size_of::<AdlxGpu>(), std::mem::size_of::<usize>());
        assert_eq!(
            std::mem::size_of::<Adlx3DChill>(),
            std::mem::size_of::<usize>()
        );
    }

    /// Guards the transcribed slot counts: on x64 every slot is 8 bytes, so the size of each vtable
    /// struct is a direct check of "how many entries did the header declare".
    #[test]
    fn vtable_slot_counts_match_headers() {
        let p = std::mem::size_of::<usize>();
        // IADLXSystem: 12 slots.
        assert_eq!(std::mem::size_of::<AdlxSystemVtbl>(), 12 * p);
        // IADLXGPU: 3 + 16.
        assert_eq!(std::mem::size_of::<AdlxGpuVtbl>(), 19 * p);
        // IADLXGPUList: 3 + 8 IADLXList slots + 2 GPUList slots = 13.
        assert_eq!(std::mem::size_of::<AdlxGpuListVtbl>(), 13 * p);
        // IADLXGPUTuningServices: 3 + 15.
        assert_eq!(std::mem::size_of::<AdlxGpuTuningServicesVtbl>(), 18 * p);
        // IADLX3DSettingsServices: 3 + 14.
        assert_eq!(std::mem::size_of::<Adlx3DSettingsServicesVtbl>(), 17 * p);
        // IADLX3DChill: 3 + 8.
        assert_eq!(std::mem::size_of::<Adlx3DChillVtbl>(), 11 * p);
        // IADLX3DFrameRateTargetControl: 3 + 6.
        assert_eq!(
            std::mem::size_of::<Adlx3DFrameRateTargetControlVtbl>(),
            9 * p
        );
        // IADLX3DAntiLag / EnhancedSync: 3 + 3.
        assert_eq!(std::mem::size_of::<Adlx3DToggleVtbl>(), 6 * p);
    }
}

#[cfg(test)]
mod sharpening_abi_regression {
    use super::*;
    #[test]
    fn sharpening_setter_uses_slot_seven_not_the_range_getter() {
        assert_eq!(
            std::mem::offset_of!(Adlx3DImageSharpeningVtbl, set_enabled),
            7 * std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::size_of::<Adlx3DImageSharpeningVtbl>(),
            9 * std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::offset_of!(Adlx3DToggleVtbl, set_enabled),
            5 * std::mem::size_of::<usize>()
        );
    }
}
