//! Live ADLX session over the installed driver DLL, and the `GfxBackend` implementation.
//!
//! SAFETY / POLICY CONSTRAINTS (enforced here, not in the UI)
//! * The DLL is loaded by absolute path from the system directory (System32) with
//!   `LOAD_LIBRARY_SEARCH_SYSTEM32`. There is no fallback to the application directory, the current
//!   directory or `PATH`, and no ADLX DLL is redistributed with OpenRevo.
//! * Every acquired ADLX interface is released through `IADLXInterface::Release`. The settings
//!   interfaces are acquired per read/write and released immediately, and the session's own
//!   interfaces are released in `Drop` *before* `ADLXTerminate` is called.
//! * Sessions are serialized by the caller (`service` owns one dedicated worker thread), which
//!   matches ADLX's requirement that initialization/termination is not interleaved.
//! * Every write is verified by a fresh full-state read in the engine, never assumed.

use std::ffi::{c_char, c_void};
use std::marker::PhantomData;

use super::abi::*;
use super::engine::*;

// ---------------------------------------------------------------------------------------------
// Minimal kernel32 ABI (only what the loader needs).
// ---------------------------------------------------------------------------------------------
type HModule = *mut c_void;

const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
const DLL_FILE_NAME: &str = "amdadlx64.dll";

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(path: *const u16, file: HModule, flags: u32) -> HModule;
    fn GetProcAddress(module: HModule, name: *const u8) -> *mut c_void;
    fn FreeLibrary(module: HModule) -> i32;
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    fn GetLastError() -> u32;
}

fn wide(path: &str) -> Vec<u16> {
    path.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Absolute path of the installed ADLX DLL: `%SystemRoot%\System32\amdadlx64.dll` as resolved by
/// `GetSystemDirectoryW` (never a relative path, never a search-path lookup).
pub fn system_adlx_dll_path() -> Result<String, String> {
    let mut buf = vec![0u16; 260];
    let mut len = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if len == 0 {
        return Err(format!(
            "GetSystemDirectoryW failed (GetLastError={})",
            unsafe { GetLastError() }
        ));
    }
    if len >= buf.len() {
        buf.resize(len + 1, 0);
        let len2 = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if len2 == 0 || len2 >= buf.len() {
            return Err("GetSystemDirectoryW returned an unusable buffer length".into());
        }
        len = len2;
    }
    let sys_dir = String::from_utf16_lossy(&buf[..len]);
    Ok(format!("{sys_dir}\\{DLL_FILE_NAME}"))
}

/// Releases one ADLX interface exactly once.
struct AdlxGuard(AdlxHandle);

impl Drop for AdlxGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                adlx_release(self.0);
            }
        }
    }
}

impl AdlxGuard {
    fn new(handle: AdlxHandle) -> Self {
        AdlxGuard(handle)
    }

    fn as_void(&self) -> *mut c_void {
        self.0
    }
}

/// One initialized ADLX session. Dropping it releases all interfaces, terminates ADLX and unloads
/// the DLL, in that order.
pub struct AdlxSession {
    lib: HModule,
    terminate: AdlxTerminateFn,
    /// Root object owned by ADLX; intentionally never Release()d (see `Drop`).
    _system: AdlxHandle,
    gpu_list: AdlxHandle,
    gpu: AdlxHandle,
    d3d: AdlxHandle,
    tuning: AdlxHandle,
    gpu_name: Option<String>,
    version: String,
}

impl Drop for AdlxSession {
    fn drop(&mut self) {
        // Reverse acquisition order, interfaces first, then ADLXTerminate, then unload.
        //
        // NOTE (ABI): IADLXSystem must NOT be released here. Unlike every other ADLX interface,
        // ISystem.h declares IADLXSystemVtbl without the IADLXInterface prefix: its slot 0 is
        // GetHybridGraphicsType, slot 1 is GetGPUs and slot 2 is QueryInterface. Calling the
        // type-erased `Release` on it would dispatch GetGPUs as "Release" and fault (observed as
        // STATUS_ACCESS_VIOLATION during the live driver test). ADLX owns the system object and
        // frees it in ADLXTerminate.
        unsafe {
            adlx_release(self.tuning);
            adlx_release(self.d3d);
            adlx_release(self.gpu);
            adlx_release(self.gpu_list);
            (self.terminate)();
            FreeLibrary(self.lib);
        }
    }
}

impl AdlxSession {
    /// Loads, version-checks and initializes ADLX. Any failure is returned as an explicit reason so
    /// the UI can show "unavailable" instead of an empty/optimistic panel.
    pub fn open() -> Result<Self, String> {
        if std::mem::size_of::<usize>() != 8 {
            return Err("ADLX bindings require a 64-bit (x64) process".into());
        }
        let path = system_adlx_dll_path()?;
        let path_w = wide(&path);

        unsafe {
            let lib = LoadLibraryExW(
                path_w.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            );
            if lib.is_null() {
                return Err(format!(
                    "could not load {path} (GetLastError={}); is the AMD driver installed?",
                    GetLastError()
                ));
            }

            let query_full = GetProcAddress(lib, EXPORT_QUERY_FULL_VERSION.as_ptr() as *const u8);
            let initialize = GetProcAddress(lib, EXPORT_INITIALIZE.as_ptr() as *const u8);
            let terminate = GetProcAddress(lib, EXPORT_TERMINATE.as_ptr() as *const u8);
            if query_full.is_null() || initialize.is_null() || terminate.is_null() {
                FreeLibrary(lib);
                return Err(format!(
                    "{path} does not export the ADLX 1.x entry points (ADLXQueryFullVersion / ADLXInitialize / ADLXTerminate)"
                ));
            }

            let query_full: AdlxQueryFullVersionFn = std::mem::transmute(query_full);
            let initialize: AdlxInitializeFn = std::mem::transmute(initialize);
            let terminate: AdlxTerminateFn = std::mem::transmute(terminate);

            let mut full: u64 = 0;
            let rc = query_full(&mut full);
            if adlx_failed(rc) || full == 0 {
                FreeLibrary(lib);
                return Err(format!(
                    "ADLXQueryFullVersion failed: rc={rc} ({})",
                    adlx_result_name(rc)
                ));
            }
            let (major, minor, build, revision) = decode_full_version(full);
            let version = format!("{major}.{minor}.{build}.{revision}");
            if major < 1 {
                FreeLibrary(lib);
                return Err(format!(
                    "installed ADLX version {version} is older than the 1.x base interfaces this module requires"
                ));
            }

            let mut system: AdlxHandle = std::ptr::null_mut();
            let rc = initialize(full, &mut system);
            if adlx_failed(rc) || system.is_null() {
                FreeLibrary(lib);
                return Err(format!(
                    "ADLXInitialize({version}) failed: rc={rc} ({})",
                    adlx_result_name(rc)
                ));
            }

            let mut session = AdlxSession {
                lib,
                terminate,
                _system: system,
                gpu_list: std::ptr::null_mut(),
                gpu: std::ptr::null_mut(),
                d3d: std::ptr::null_mut(),
                tuning: std::ptr::null_mut(),
                gpu_name: None,
                version,
            };

            // IADLXSystem::GetGPUs
            let vtbl = *(system as *const *const AdlxSystemVtbl);
            if vtbl.is_null() {
                return Err("IADLXSystem vtable was null".into());
            }
            let mut gpus: AdlxHandle = std::ptr::null_mut();
            let rc = ((*vtbl).get_gpus)(system, &mut gpus);
            if adlx_failed(rc) || gpus.is_null() {
                return Err(format!(
                    "IADLXSystem::GetGPUs failed: rc={rc} ({})",
                    adlx_result_name(rc)
                ));
            }
            session.gpu_list = gpus;

            let list_vtbl = *(gpus as *const *const AdlxGpuListVtbl);
            if list_vtbl.is_null() {
                return Err("IADLXGPUList vtable was null".into());
            }
            let count = ((*list_vtbl).size)(gpus);
            if count == 0 {
                return Err("ADLX reported no AMD GPU on this system".into());
            }
            let mut gpu: AdlxHandle = std::ptr::null_mut();
            let rc = ((*list_vtbl).at_gpu_list)(gpus, 0, &mut gpu);
            if adlx_failed(rc) || gpu.is_null() {
                return Err(format!(
                    "IADLXGPUList::At_GPUList(0) failed: rc={rc} ({})",
                    adlx_result_name(rc)
                ));
            }
            session.gpu = gpu;
            session.gpu_name = read_gpu_name(gpu);

            // IADLXSystem::Get3DSettingsServices and GetGPUTuningServices. Both are non-fatal: a
            // driver that refuses them simply yields an unavailable feature set, reported as such.
            let mut d3d: AdlxHandle = std::ptr::null_mut();
            let rc = ((*vtbl).get_3d_settings_services)(system, &mut d3d);
            if adlx_succeeded(rc) && !d3d.is_null() {
                session.d3d = d3d;
            }
            let mut tuning: AdlxHandle = std::ptr::null_mut();
            let rc = ((*vtbl).get_gpu_tuning_services)(system, &mut tuning);
            if adlx_succeeded(rc) && !tuning.is_null() {
                session.tuning = tuning;
            }

            Ok(session)
        }
    }

    pub fn metrics(&self) -> super::metrics::DriverMetrics {
        unsafe {
            super::metrics::read(
                self._system,
                self.gpu,
                self.gpu_name.as_deref().unwrap_or(""),
            )
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn backend(&self) -> AdlxBackend<'_> {
        AdlxBackend {
            gpu: self.gpu,
            d3d: self.d3d,
            tuning: self.tuning,
            gpu_name: self.gpu_name.clone(),
            version: self.version.clone(),
            _session: PhantomData,
        }
    }
}

unsafe fn read_gpu_name(gpu: AdlxHandle) -> Option<String> {
    let vtbl = *(gpu as *const *const AdlxGpuVtbl);
    if vtbl.is_null() {
        return None;
    }
    let mut name: *const c_char = std::ptr::null();
    let rc = ((*vtbl).name)(gpu, &mut name);
    if adlx_failed(rc) || name.is_null() {
        return None;
    }
    Some(
        std::ffi::CStr::from_ptr(name)
            .to_string_lossy()
            .into_owned(),
    )
}

/// Borrowed ADLX handles for one session. Never outlives the `AdlxSession` that owns them.
pub struct AdlxBackend<'a> {
    gpu: AdlxHandle,
    d3d: AdlxHandle,
    tuning: AdlxHandle,
    gpu_name: Option<String>,
    version: String,
    _session: PhantomData<&'a AdlxSession>,
}

unsafe fn rc_note(what: &str, rc: AdlxResult) -> String {
    format!("{what}: rc={rc} ({})", adlx_result_name(rc))
}

/// Reads one 3D toggle (AntiLag / ImageSharpening / EnhancedSync share the read-only slots 3 and 4).
unsafe fn read_toggle_setting(
    services: AdlxHandle,
    gpu: AdlxHandle,
    get: FnGet3DSetting,
    label: &str,
) -> ToggleState {
    if services.is_null() {
        return ToggleState {
            supported: false,
            enabled: None,
            read_error: Some(format!("IADLX3DSettingsServices unavailable for {label}")),
        };
    }
    let mut raw: AdlxHandle = std::ptr::null_mut();
    let rc = get(services, gpu, &mut raw);
    if adlx_failed(rc) || raw.is_null() {
        return ToggleState {
            supported: false,
            enabled: None,
            read_error: Some(rc_note(&format!("Get{label}"), rc)),
        };
    }
    let guard = AdlxGuard::new(raw);
    let vtbl = *(guard.as_void() as *const *const Adlx3DToggleVtbl);
    if vtbl.is_null() {
        return ToggleState {
            supported: false,
            enabled: None,
            read_error: Some(format!("{label} vtable was null")),
        };
    }
    let mut supported: u8 = 0;
    let rc = ((*vtbl).is_supported)(guard.as_void(), &mut supported);
    if adlx_failed(rc) {
        return ToggleState {
            supported: false,
            enabled: None,
            read_error: Some(rc_note(&format!("IADLX3D{label}::IsSupported"), rc)),
        };
    }
    if supported == 0 {
        return ToggleState {
            supported: false,
            enabled: None,
            read_error: None,
        };
    }
    let mut enabled: u8 = 0;
    let rc = ((*vtbl).is_enabled)(guard.as_void(), &mut enabled);
    if adlx_failed(rc) {
        return ToggleState {
            supported: true,
            enabled: None,
            read_error: Some(rc_note(&format!("IADLX3D{label}::IsEnabled"), rc)),
        };
    }
    ToggleState {
        supported: true,
        enabled: Some(enabled != 0),
        read_error: None,
    }
}

unsafe fn read_chill(services: AdlxHandle, gpu: AdlxHandle) -> ChillState {
    if services.is_null() {
        return ChillState {
            supported: false,
            read_error: Some("IADLX3DSettingsServices unavailable for Chill".into()),
            ..Default::default()
        };
    }
    let vtbl_services = *(services as *const *const Adlx3DSettingsServicesVtbl);
    if vtbl_services.is_null() {
        return ChillState::default();
    }
    let mut raw: AdlxHandle = std::ptr::null_mut();
    let rc = ((*vtbl_services).get_chill)(services, gpu, &mut raw);
    if adlx_failed(rc) || raw.is_null() {
        return ChillState {
            supported: false,
            read_error: Some(rc_note("GetChill", rc)),
            ..Default::default()
        };
    }
    let guard = AdlxGuard::new(raw);
    let vtbl = *(guard.as_void() as *const *const Adlx3DChillVtbl);
    if vtbl.is_null() {
        return ChillState::default();
    }
    let mut supported: u8 = 0;
    let rc = ((*vtbl).is_supported)(guard.as_void(), &mut supported);
    if adlx_failed(rc) {
        return ChillState {
            supported: false,
            read_error: Some(rc_note("IADLX3DChill::IsSupported", rc)),
            ..Default::default()
        };
    }
    if supported == 0 {
        return ChillState::default();
    }

    let mut state = ChillState {
        supported: true,
        ..Default::default()
    };
    let mut enabled: u8 = 0;
    let rc = ((*vtbl).is_enabled)(guard.as_void(), &mut enabled);
    state.enabled = if adlx_succeeded(rc) {
        Some(enabled != 0)
    } else {
        None
    };
    if adlx_failed(rc) {
        state.read_error = Some(rc_note("IADLX3DChill::IsEnabled", rc));
    }

    let mut min_fps: i32 = 0;
    let rc = ((*vtbl).get_min_fps)(guard.as_void(), &mut min_fps);
    if adlx_succeeded(rc) {
        state.min_fps = Some(min_fps);
    } else if state.read_error.is_none() {
        state.read_error = Some(rc_note("IADLX3DChill::GetMinFPS", rc));
    }
    let mut max_fps: i32 = 0;
    let rc = ((*vtbl).get_max_fps)(guard.as_void(), &mut max_fps);
    if adlx_succeeded(rc) {
        state.max_fps = Some(max_fps);
    } else if state.read_error.is_none() {
        state.read_error = Some(rc_note("IADLX3DChill::GetMaxFPS", rc));
    }

    let mut range = AdlxIntRange::default();
    let rc = ((*vtbl).get_fps_range)(guard.as_void(), &mut range);
    if adlx_succeeded(rc) && range.maxValue > range.minValue {
        state.range = Some(range.into());
    }
    state
}

unsafe fn read_frtc(services: AdlxHandle, gpu: AdlxHandle) -> FrtcState {
    if services.is_null() {
        return FrtcState {
            supported: false,
            read_error: Some(
                "IADLX3DSettingsServices unavailable for Frame Rate Target Control".into(),
            ),
            ..Default::default()
        };
    }
    let vtbl_services = *(services as *const *const Adlx3DSettingsServicesVtbl);
    if vtbl_services.is_null() {
        return FrtcState::default();
    }
    let mut raw: AdlxHandle = std::ptr::null_mut();
    let rc = ((*vtbl_services).get_frame_rate_target_control)(services, gpu, &mut raw);
    if adlx_failed(rc) || raw.is_null() {
        return FrtcState {
            supported: false,
            read_error: Some(rc_note("GetFrameRateTargetControl", rc)),
            ..Default::default()
        };
    }
    let guard = AdlxGuard::new(raw);
    let vtbl = *(guard.as_void() as *const *const Adlx3DFrameRateTargetControlVtbl);
    if vtbl.is_null() {
        return FrtcState::default();
    }
    let mut supported: u8 = 0;
    let rc = ((*vtbl).is_supported)(guard.as_void(), &mut supported);
    if adlx_failed(rc) {
        return FrtcState {
            supported: false,
            read_error: Some(rc_note("IADLX3DFrameRateTargetControl::IsSupported", rc)),
            ..Default::default()
        };
    }
    if supported == 0 {
        return FrtcState::default();
    }

    let mut state = FrtcState {
        supported: true,
        ..Default::default()
    };
    let mut enabled: u8 = 0;
    let rc = ((*vtbl).is_enabled)(guard.as_void(), &mut enabled);
    state.enabled = if adlx_succeeded(rc) {
        Some(enabled != 0)
    } else {
        None
    };
    if adlx_failed(rc) {
        state.read_error = Some(rc_note("IADLX3DFrameRateTargetControl::IsEnabled", rc));
    }

    let mut fps: i32 = 0;
    let rc = ((*vtbl).get_fps)(guard.as_void(), &mut fps);
    if adlx_succeeded(rc) {
        state.fps = Some(fps);
    } else if state.read_error.is_none() {
        state.read_error = Some(rc_note("IADLX3DFrameRateTargetControl::GetFPS", rc));
    }

    let mut range = AdlxIntRange::default();
    let rc = ((*vtbl).get_fps_range)(guard.as_void(), &mut range);
    if adlx_succeeded(rc) && range.maxValue > range.minValue {
        state.range = Some(range.into());
    }
    state
}

unsafe extern "system" fn dummy_get_setting(
    _this: *mut c_void,
    _gpu: *mut c_void,
    _out: *mut *mut c_void,
) -> AdlxResult {
    ADLX_FAIL
}

impl<'a> GfxBackend for AdlxBackend<'a> {
    fn read_state(&mut self) -> DriverGraphicsState {
        let mut state = DriverGraphicsState {
            available: true,
            detail: Some(format!("ADLX {} via amdadlx64.dll", self.version)),
            adlx_version: Some(self.version.clone()),
            gpu_name: self.gpu_name.clone(),
            ..Default::default()
        };

        unsafe {
            if self.d3d.is_null() {
                state.warnings.push(
                    "IADLXSystem::Get3DSettingsServices is unavailable; no 3D setting can be read"
                        .into(),
                );
                state.anti_lag.read_error = Some("3D settings services unavailable".into());
                state.image_sharpening.read_error = Some("3D settings services unavailable".into());
                state.enhanced_sync.read_error = Some("3D settings services unavailable".into());
                state.chill.read_error = Some("3D settings services unavailable".into());
                state.frame_rate_target.read_error =
                    Some("3D settings services unavailable".into());
            } else {
                let vtbl = *(self.d3d as *const *const Adlx3DSettingsServicesVtbl);
                let (get_anti_lag, get_sharpening, get_enhanced_sync) = if vtbl.is_null() {
                    (
                        dummy_get_setting as FnGet3DSetting,
                        dummy_get_setting as FnGet3DSetting,
                        dummy_get_setting as FnGet3DSetting,
                    )
                } else {
                    (
                        (*vtbl).get_anti_lag,
                        (*vtbl).get_image_sharpening,
                        (*vtbl).get_enhanced_sync,
                    )
                };
                state.anti_lag = read_toggle_setting(self.d3d, self.gpu, get_anti_lag, "AntiLag");
                state.image_sharpening =
                    read_toggle_setting(self.d3d, self.gpu, get_sharpening, "ImageSharpening");
                state.enhanced_sync =
                    read_toggle_setting(self.d3d, self.gpu, get_enhanced_sync, "EnhancedSync");
                state.chill = read_chill(self.d3d, self.gpu);
                state.frame_rate_target = read_frtc(self.d3d, self.gpu);
            }

            state.tuning = read_tuning_evidence(self.tuning, self.gpu);
        }

        // Failures that must never be silent.
        for (label, err) in [
            ("Anti-Lag", &state.anti_lag.read_error),
            ("Image Sharpening", &state.image_sharpening.read_error),
            ("Chill", &state.chill.read_error),
            (
                "Frame Rate Target Control",
                &state.frame_rate_target.read_error,
            ),
        ] {
            if let Some(err) = err {
                state.warnings.push(format!("{label} read failure: {err}"));
            }
        }

        state
    }

    fn write_enabled(&mut self, feature: GfxFeature, enabled: bool) -> WriteOutcome {
        unsafe {
            if self.d3d.is_null() {
                return WriteOutcome::with_detail(
                    ADLX_NOT_SUPPORTED,
                    "3D settings services unavailable",
                );
            }
            let vtbl = *(self.d3d as *const *const Adlx3DSettingsServicesVtbl);
            if vtbl.is_null() {
                return WriteOutcome::with_detail(
                    ADLX_INVALID_OBJECT,
                    "3D settings services vtable null",
                );
            }
            let getter = match feature {
                GfxFeature::AntiLag => (*vtbl).get_anti_lag,
                GfxFeature::Chill => (*vtbl).get_chill,
                GfxFeature::ImageSharpening => (*vtbl).get_image_sharpening,
                GfxFeature::FrameRateTarget => (*vtbl).get_frame_rate_target_control,
            };
            let mut raw: AdlxHandle = std::ptr::null_mut();
            let rc = getter(self.d3d, self.gpu, &mut raw);
            if adlx_failed(rc) || raw.is_null() {
                return WriteOutcome::with_detail(
                    rc,
                    format!("could not obtain the {} interface", feature.label()),
                );
            }
            let guard = AdlxGuard::new(raw);

            if feature == GfxFeature::FrameRateTarget {
                let vtbl = *(guard.as_void() as *const *const Adlx3DFrameRateTargetControlVtbl);
                if vtbl.is_null() {
                    return WriteOutcome::with_detail(ADLX_INVALID_OBJECT, "FRTC vtable null");
                }
                let value: u8 = if enabled { 1 } else { 0 };
                WriteOutcome::new(((*vtbl).set_enabled)(guard.as_void(), value))
            } else if feature == GfxFeature::ImageSharpening {
                let vtbl = *(guard.as_void() as *const *const Adlx3DImageSharpeningVtbl);
                if vtbl.is_null() {
                    return WriteOutcome::with_detail(
                        ADLX_INVALID_OBJECT,
                        "ImageSharpening vtable null",
                    );
                }
                WriteOutcome::new(((*vtbl).set_enabled)(guard.as_void(), u8::from(enabled)))
            } else if feature == GfxFeature::Chill {
                let vtbl = *(guard.as_void() as *const *const Adlx3DChillVtbl);
                if vtbl.is_null() {
                    return WriteOutcome::with_detail(ADLX_INVALID_OBJECT, "Chill vtable null");
                }
                let value: u8 = if enabled { 1 } else { 0 };
                WriteOutcome::new(((*vtbl).set_enabled)(guard.as_void(), value))
            } else {
                let vtbl = *(guard.as_void() as *const *const Adlx3DToggleVtbl);
                if vtbl.is_null() {
                    return WriteOutcome::with_detail(ADLX_INVALID_OBJECT, "toggle vtable null");
                }
                let value: u8 = if enabled { 1 } else { 0 };
                WriteOutcome::new(((*vtbl).set_enabled)(guard.as_void(), value))
            }
        }
    }

    fn write_chill_fps(&mut self, min: i32, max: i32) -> WriteOutcome {
        unsafe {
            if self.d3d.is_null() {
                return WriteOutcome::with_detail(
                    ADLX_NOT_SUPPORTED,
                    "3D settings services unavailable",
                );
            }
            let vtbl = *(self.d3d as *const *const Adlx3DSettingsServicesVtbl);
            if vtbl.is_null() {
                return WriteOutcome::with_detail(
                    ADLX_INVALID_OBJECT,
                    "3D settings services vtable null",
                );
            }
            let mut raw: AdlxHandle = std::ptr::null_mut();
            let rc = ((*vtbl).get_chill)(self.d3d, self.gpu, &mut raw);
            if adlx_failed(rc) || raw.is_null() {
                return WriteOutcome::with_detail(rc, "could not obtain the Chill interface");
            }
            let guard = AdlxGuard::new(raw);
            let vtbl = *(guard.as_void() as *const *const Adlx3DChillVtbl);
            if vtbl.is_null() {
                return WriteOutcome::with_detail(ADLX_INVALID_OBJECT, "Chill vtable null");
            }
            // A new interval wholly above the old interval needs its upper bound moved
            // first; min-first would temporarily violate min <= max and be refused.
            let mut old_max = 0;
            let rc = ((*vtbl).get_max_fps)(guard.as_void(), &mut old_max);
            if adlx_failed(rc) {
                return WriteOutcome::with_detail(rc, "GetMaxFPS before range update failed");
            }
            let min_call = ((*vtbl).set_min_fps, min, "SetMinFPS");
            let max_call = ((*vtbl).set_max_fps, max, "SetMaxFPS");
            let calls = if min > old_max {
                [max_call, min_call]
            } else {
                [min_call, max_call]
            };
            for (setter, value, name) in calls {
                let rc = setter(guard.as_void(), value);
                if adlx_failed(rc) {
                    return WriteOutcome::with_detail(
                        rc,
                        format!("{name}({value}) rejected; partial state will be read back"),
                    );
                }
            }
            WriteOutcome::new(ADLX_OK)
        }
    }

    fn write_frtc_fps(&mut self, fps: i32) -> WriteOutcome {
        unsafe {
            if self.d3d.is_null() {
                return WriteOutcome::with_detail(
                    ADLX_NOT_SUPPORTED,
                    "3D settings services unavailable",
                );
            }
            let vtbl = *(self.d3d as *const *const Adlx3DSettingsServicesVtbl);
            if vtbl.is_null() {
                return WriteOutcome::with_detail(
                    ADLX_INVALID_OBJECT,
                    "3D settings services vtable null",
                );
            }
            let mut raw: AdlxHandle = std::ptr::null_mut();
            let rc = ((*vtbl).get_frame_rate_target_control)(self.d3d, self.gpu, &mut raw);
            if adlx_failed(rc) || raw.is_null() {
                return WriteOutcome::with_detail(rc, "could not obtain the FRTC interface");
            }
            let guard = AdlxGuard::new(raw);
            let vtbl = *(guard.as_void() as *const *const Adlx3DFrameRateTargetControlVtbl);
            if vtbl.is_null() {
                return WriteOutcome::with_detail(ADLX_INVALID_OBJECT, "FRTC vtable null");
            }
            WriteOutcome::new(((*vtbl).set_fps)(guard.as_void(), fps))
        }
    }
}

unsafe fn read_tuning_evidence(services: AdlxHandle, gpu: AdlxHandle) -> TuningEvidence {
    let mut evidence = TuningEvidence::default();
    if services.is_null() {
        return evidence;
    }
    let vtbl = *(services as *const *const AdlxGpuTuningServicesVtbl);
    if vtbl.is_null() {
        return evidence;
    }
    let mut flag: u8 = 0;
    let rc = ((*vtbl).is_supported_manual_gfx_tuning)(services, gpu, &mut flag);
    evidence.manual_gfx = if adlx_succeeded(rc) {
        Some(flag != 0)
    } else {
        None
    };
    let mut flag: u8 = 0;
    let rc = ((*vtbl).is_supported_manual_vram_tuning)(services, gpu, &mut flag);
    evidence.manual_vram = if adlx_succeeded(rc) {
        Some(flag != 0)
    } else {
        None
    };
    let mut flag: u8 = 0;
    let rc = ((*vtbl).is_supported_manual_fan_tuning)(services, gpu, &mut flag);
    evidence.manual_fan = if adlx_succeeded(rc) {
        Some(flag != 0)
    } else {
        None
    };
    let mut flag: u8 = 0;
    let rc = ((*vtbl).is_supported_manual_power_tuning)(services, gpu, &mut flag);
    evidence.manual_power = if adlx_succeeded(rc) {
        Some(flag != 0)
    } else {
        None
    };
    let mut flag: u8 = 0;
    let rc = ((*vtbl).is_supported_auto_tuning)(services, gpu, &mut flag);
    evidence.auto_tuning = if adlx_succeeded(rc) {
        Some(flag != 0)
    } else {
        None
    };
    let mut flag: u8 = 0;
    let rc = ((*vtbl).is_supported_preset_tuning)(services, gpu, &mut flag);
    evidence.preset_tuning = if adlx_succeeded(rc) {
        Some(flag != 0)
    } else {
        None
    };
    evidence
}
