//! Read-only Windows battery class driver queries. All values retain unknown sentinels.
use serde_json::{json, Value};
use windows::core::{GUID, PCWSTR};
use windows::Win32::{Devices::DeviceAndDriverInstallation::*, Foundation::CloseHandle,
    Storage::FileSystem::*, System::IO::DeviceIoControl};

#[repr(C)]
#[derive(Default)]
struct Information {
    capabilities: u32, technology: u8, reserved: [u8; 3], chemistry: [u8; 4],
    designed: u32, full: u32, alert1: u32, alert2: u32, bias: u32, cycles: u32,
}
#[repr(C)]
struct Query { tag: u32, level: u32, at_rate: i32 }
#[repr(C)]
struct Wait { tag: u32, timeout: u32, state: u32, low: u32, high: u32 }
#[repr(C)]
#[derive(Default)]
struct Status { state: u32, capacity: u32, voltage: u32, rate: i32 }

unsafe fn query<I, O: Default>(handle: windows::Win32::Foundation::HANDLE, code: u32, input: &I) -> Result<O, String> {
    let mut output = O::default(); let mut returned = 0;
    DeviceIoControl(handle, code, Some(input as *const I as _), std::mem::size_of::<I>() as u32,
        Some(&mut output as *mut O as _), std::mem::size_of::<O>() as u32, Some(&mut returned), None)
        .map_err(|e| e.to_string())?;
    if returned < std::mem::size_of::<O>() as u32 { return Err("Truncated battery response".into()); }
    Ok(output)
}

pub fn read_health() -> Result<Value, String> {
    unsafe {
        let guid = GUID::from_u128(0x72631e54_78a4_11d0_bcf7_00aa00b7b32a);
        let devices = SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE).map_err(|e| e.to_string())?;
        let mut batteries = vec![];
        let mut errors = vec![];
        for index in 0..32 {
            let mut interface = SP_DEVICE_INTERFACE_DATA::default();
            interface.cbSize = std::mem::size_of_val(&interface) as u32;
            if let Err(error) = SetupDiEnumDeviceInterfaces(devices, None, &guid, index, &mut interface) {
                if error.code().0 as u32 != (0x80070000 | 259) { errors.push(error.to_string()); }
                break;
            }
            let mut size = 0;
            let _ = SetupDiGetDeviceInterfaceDetailW(devices, &interface, None, 0, Some(&mut size), None);
            if size < 8 || size > 65536 { errors.push("Invalid battery device path size".into()); continue; }
            let mut buffer = vec![0u64; (size as usize + 7) / 8];
            let detail = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if let Err(e) = SetupDiGetDeviceInterfaceDetailW(devices, &interface, Some(detail), size, None, None) { errors.push(e.to_string()); continue; }
            let path = PCWSTR((*detail).DevicePath.as_ptr());
            let handle = match CreateFileW(path, windows::Win32::Foundation::GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None) {
                Ok(handle) => handle, Err(e) => { errors.push(e.to_string()); continue; }
            };
            let result = (|| -> Result<Value, String> {
                let tag: u32 = query(handle, 0x294040, &0u32)?;
                if tag == 0 { return Err("Battery temporarily unavailable".into()); }
                let info: Information = query(handle, 0x294044, &Query { tag, level: 0, at_rate: 0 })?;
                let status: Option<Status> = query(handle, 0x29404c, &Wait { tag, timeout: 0, state: 0, low: 0, high: u32::MAX }).ok();
                let known = |value: u32| if value == 0 || value == u32::MAX { None } else { Some(value) };
                let rate = status.as_ref().and_then(|s| if s.rate == i32::MIN { None } else { Some(s.rate) });
                Ok(json!({"instance": format!("battery-{index}"), "designed_capacity": known(info.designed),
                    "full_capacity": known(info.full), "cycles": known(info.cycles),
                    "capacity_relative": info.capabilities & 0x40000000,
                    "remaining_capacity": status.as_ref().and_then(|s| if s.capacity == u32::MAX { None } else { Some(s.capacity) }),
                    "charge_rate": rate.filter(|r| *r > 0),
                    "discharge_rate": rate.filter(|r| *r < 0).map(|r| -i64::from(r)),
                    "charging": status.as_ref().map(|s| s.state & 4 != 0),
                    "discharging": status.as_ref().map(|s| s.state & 2 != 0)}))
            })();
            let _ = CloseHandle(handle);
            match result { Ok(value) => batteries.push(value), Err(e) => errors.push(e) }
        }
        let _ = SetupDiDestroyDeviceInfoList(devices);
        if batteries.is_empty() && !errors.is_empty() { return Err(errors.join("; ")); }
        Ok(json!(batteries))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn battery_driver_abi_sizes() {
        assert_eq!(std::mem::size_of::<super::Information>(), 36);
        assert_eq!(std::mem::size_of::<super::Query>(), 12);
        assert_eq!(std::mem::size_of::<super::Wait>(), 20);
        assert_eq!(std::mem::size_of::<super::Status>(), 16);
    }
    #[test]
    #[ignore = "Read-only local Windows hardware acceptance"]
    fn live_battery_read() {
        let value = super::read_health().unwrap();
        println!("BATTERY_HEALTH={value}");
        assert!(!value.as_array().unwrap().is_empty());
    }
}
