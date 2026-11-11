//! Windows overlay API shared by native scenes and legacy power linkage.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

// Windows 10/11 Official Power Overlay Scheme GUIDs
// Best Power Efficiency: 961cc777-2547-4f9d-8174-7d86181b8a7a
pub const GUID_OFFICE_SAVER: GUID = GUID {
    data1: 0x961cc777,
    data2: 0x2547,
    data3: 0x4f9d,
    data4: [0x81, 0x74, 0x7d, 0x86, 0x18, 0x1b, 0x8a, 0x7a],
};

pub const GUID_BALANCED: GUID = GUID {
    data1: 0x00000000,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0, 0, 0, 0, 0, 0, 0, 0],
};

pub const GUID_TURBO_PERF: GUID = GUID {
    data1: 0xded574b5,
    data2: 0x45a0,
    data3: 0x4f42,
    data4: [0x87, 0x37, 0x46, 0x34, 0x5c, 0x09, 0xc2, 0x38],
};

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(lp_lib_filename: *const u8) -> *mut std::ffi::c_void;
    fn GetProcAddress(h_module: *mut std::ffi::c_void, lp_proc_name: *const u8) -> *mut std::ffi::c_void;
}

type FnPowerSetActiveOverlayScheme = unsafe extern "system" fn(*const GUID) -> u32;
static POWER_LIBRARY: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| unsafe { LoadLibraryA(b"powrprof.dll\0".as_ptr()) as usize });

pub(crate) fn set_active_overlay_scheme(guid_ptr: *const GUID) -> u32 {
    unsafe {
        let module = *POWER_LIBRARY as *mut std::ffi::c_void;
        if module.is_null() { return 1; }
        let function = GetProcAddress(module, b"PowerSetActiveOverlayScheme\0".as_ptr());
        if function.is_null() { return 2; }
        let function: FnPowerSetActiveOverlayScheme = std::mem::transmute(function);
        function(guid_ptr)
    }
}

pub(crate) fn get_actual_overlay_scheme() -> Result<GUID, String> {
    unsafe {
        let module = *POWER_LIBRARY as *mut std::ffi::c_void;
        if module.is_null() { return Err("Windows power library unavailable".into()); }
        let function = GetProcAddress(module, b"PowerGetActualOverlayScheme\0".as_ptr());
        if function.is_null() { return Err("Windows energy readback unavailable".into()); }
        let function: unsafe extern "system" fn(*mut GUID) -> u32 = std::mem::transmute(function);
        let mut guid = GUID_BALANCED;
        let result = function(&mut guid);
        if result != 0 { return Err(format!("Windows energy readback: {result}")); }
        Ok(guid)
    }
}


#[cfg(test)]
mod tests {
    #[test]
    fn three_energy_modes_have_distinct_guids() {
        assert_ne!(super::GUID_OFFICE_SAVER, super::GUID_BALANCED);
        assert_ne!(super::GUID_TURBO_PERF, super::GUID_BALANCED);
        assert_ne!(super::GUID_OFFICE_SAVER, super::GUID_TURBO_PERF);
    }
    #[test]
    #[ignore = "Read-only local Windows energy acceptance"]
    fn live_energy_read() { println!("WINDOWS_ENERGY={:?}", super::get_actual_overlay_scheme().unwrap()); }
}
