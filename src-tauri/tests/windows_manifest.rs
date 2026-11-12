//! Prevent the lib-test loader regression without installing/copying DLLs.
#![cfg(all(windows, target_env = "msvc"))]

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut std::ffi::c_void;
    fn GetProcAddress(module: *mut std::ffi::c_void, name: *const u8) -> *mut std::ffi::c_void;
    fn FreeLibrary(module: *mut std::ffi::c_void) -> i32;
}

#[test]
fn common_controls_v6_is_active_for_test_executables() {
    let name: Vec<u16> = "comctl32.dll\0".encode_utf16().collect();
    unsafe {
        let module = LoadLibraryW(name.as_ptr());
        assert!(!module.is_null(), "Common-Controls DLL could not be loaded");
        let task_dialog = GetProcAddress(module, b"TaskDialogIndirect\0".as_ptr());
        FreeLibrary(module);
        assert!(!task_dialog.is_null(), "Test manifest must activate Common-Controls v6");
    }
}
