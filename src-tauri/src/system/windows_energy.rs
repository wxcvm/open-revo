//! Windows overlay API shared by native scenes and legacy power linkage.
use windows::{
    core::{s, w},
    Win32::{
        Foundation::{HANDLE, HMODULE},
        System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32},
    },
};
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

type FnPowerSetActiveOverlayScheme = unsafe extern "system" fn(*const GUID) -> u32;
static POWER_LIBRARY: std::sync::LazyLock<Option<usize>> = std::sync::LazyLock::new(|| unsafe {
    LoadLibraryExW(
        w!("powrprof.dll"),
        HANDLE::default(),
        LOAD_LIBRARY_SEARCH_SYSTEM32,
    )
    .ok()
    .map(|module| module.0 as usize)
});

fn power_library() -> Option<HMODULE> {
    (*POWER_LIBRARY).map(|module| HMODULE(module as *mut std::ffi::c_void))
}

pub(crate) fn set_active_overlay_scheme(guid_ptr: *const GUID) -> u32 {
    unsafe {
        let Some(module) = power_library() else {
            return 1;
        };
        let Some(function) = GetProcAddress(module, s!("PowerSetActiveOverlayScheme")) else {
            return 2;
        };
        let function: FnPowerSetActiveOverlayScheme = std::mem::transmute(function);
        function(guid_ptr)
    }
}

pub(crate) fn get_actual_overlay_scheme() -> Result<GUID, String> {
    unsafe {
        let module = power_library().ok_or("Windows power library unavailable")?;
        let function = GetProcAddress(module, s!("PowerGetActualOverlayScheme"))
            .ok_or("Windows energy readback unavailable")?;
        let function: unsafe extern "system" fn(*mut GUID) -> u32 = std::mem::transmute(function);
        let mut guid = GUID_BALANCED;
        let result = function(&mut guid);
        if result != 0 {
            return Err(format!("Windows energy readback: {result}"));
        }
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
    fn live_energy_read() {
        println!(
            "WINDOWS_ENERGY={:?}",
            super::get_actual_overlay_scheme().unwrap()
        );
    }
}
