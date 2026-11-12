//! Windows PowrProf adapter for the CPU policy transaction.
//!
//! Reads and writes always target the exact power-scheme GUID captured by the
//! snapshot. After saving values, the adapter re-applies that SAME plan with
//! `PowerSetActiveScheme` because Windows does not make saved active-plan
//! values effective otherwise. It never chooses or switches to another plan.
//! The independent Windows energy overlay is read before/after activation and
//! restored only from a known exact prior value; an unknown overlay is never
//! overwritten. Values are Windows power-policy values, not measured silicon
//! clocks.

use super::cpu_policy::{Backend, Field, SideValues, Snapshot, Source, Value};
use std::ffi::c_void;
use std::ptr;
use windows::core::GUID;
use windows::Win32::Foundation::{
    LocalFree, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ERROR_SUCCESS,
    HLOCAL, WIN32_ERROR,
};
use windows::Win32::System::Power::{
    PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, PowerSetActiveScheme,
    PowerWriteACValueIndex, PowerWriteDCValueIndex,
};
use windows::Win32::System::Registry::HKEY;

const PROCESSOR_SUBGROUP: GUID = GUID::from_u128(0x54533251_82be_4824_96c1_47b60b740d00);
const MAX_PROCESSOR_STATE: GUID = GUID::from_u128(0xbc5038f7_23e0_4960_96da_33abaf5935ec);
const BOOST_MODE: GUID = GUID::from_u128(0xbe337238_0d82_4146_a960_4f3749d470c7);
const MAX_FREQUENCY_MHZ: GUID = GUID::from_u128(0x75b0ae3f_bce0_45a7_8c89_c9611c25e100);

fn setting(field: Field) -> GUID {
    match field {
        Field::MaxProcessorState(_) => MAX_PROCESSOR_STATE,
        Field::BoostMode(_) => BOOST_MODE,
        Field::MaxFrequencyMhz(_) => MAX_FREQUENCY_MHZ,
    }
}

fn code_is_unsupported(code: u32) -> bool {
    code == ERROR_FILE_NOT_FOUND.0
        || code == ERROR_INVALID_PARAMETER.0
        || code == ERROR_NOT_SUPPORTED.0
}

enum RawRead {
    Value(u32),
    Unsupported,
}

fn read_raw(plan: &GUID, field: Field) -> Result<RawRead, String> {
    let mut value = 0u32;
    let code = unsafe {
        match field {
            Field::MaxProcessorState(Source::Ac)
            | Field::BoostMode(Source::Ac)
            | Field::MaxFrequencyMhz(Source::Ac) => {
                PowerReadACValueIndex(
                    HKEY::default(),
                    Some(plan as *const GUID),
                    Some(&PROCESSOR_SUBGROUP as *const GUID),
                    Some(&setting(field) as *const GUID),
                    &mut value,
                )
                .0
            }
            Field::MaxProcessorState(Source::Dc)
            | Field::BoostMode(Source::Dc)
            | Field::MaxFrequencyMhz(Source::Dc) => PowerReadDCValueIndex(
                HKEY::default(),
                Some(plan as *const GUID),
                Some(&PROCESSOR_SUBGROUP as *const GUID),
                Some(&setting(field) as *const GUID),
                &mut value,
            ),
        }
    };
    if code == ERROR_SUCCESS.0 {
        Ok(RawRead::Value(value))
    } else if code_is_unsupported(code) {
        Ok(RawRead::Unsupported)
    } else {
        Err(format!(
            "PowrProf read {} failed with Windows error {}",
            field.name(),
            code
        ))
    }
}

fn write_raw(plan: &GUID, field: Field, value: u32) -> Result<(), String> {
    let code = unsafe {
        match field {
            Field::MaxProcessorState(Source::Ac)
            | Field::BoostMode(Source::Ac)
            | Field::MaxFrequencyMhz(Source::Ac) => {
                PowerWriteACValueIndex(
                    HKEY::default(),
                    plan as *const GUID,
                    Some(&PROCESSOR_SUBGROUP as *const GUID),
                    Some(&setting(field) as *const GUID),
                    value,
                )
                .0
            }
            Field::MaxProcessorState(Source::Dc)
            | Field::BoostMode(Source::Dc)
            | Field::MaxFrequencyMhz(Source::Dc) => PowerWriteDCValueIndex(
                HKEY::default(),
                plan as *const GUID,
                Some(&PROCESSOR_SUBGROUP as *const GUID),
                Some(&setting(field) as *const GUID),
                value,
            ),
        }
    };
    if code == ERROR_SUCCESS.0 {
        Ok(())
    } else {
        Err(format!(
            "PowrProf write {} failed with Windows error {}",
            field.name(),
            code
        ))
    }
}

fn guid_text(guid: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7]
    )
}

fn parse_guid(text: &str) -> Result<GUID, String> {
    let text = text.trim();
    let parts: Vec<&str> = text.split('-').collect();
    let lengths = [8usize, 4, 4, 4, 12];
    if parts.len() != 5
        || parts
            .iter()
            .zip(lengths)
            .any(|(part, len)| part.len() != len)
    {
        return Err(format!("invalid Windows power plan GUID: {text}"));
    }
    if parts
        .iter()
        .any(|part| !part.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(format!("invalid Windows power plan GUID: {text}"));
    }
    let data1 = u32::from_str_radix(parts[0], 16).map_err(|_| "invalid GUID data1".to_string())?;
    let data2 = u16::from_str_radix(parts[1], 16).map_err(|_| "invalid GUID data2".to_string())?;
    let data3 = u16::from_str_radix(parts[2], 16).map_err(|_| "invalid GUID data3".to_string())?;
    let raw4 = format!("{}{}", parts[3], parts[4]);
    let mut data4 = [0u8; 8];
    for (index, byte) in data4.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw4[index * 2..index * 2 + 2], 16)
            .map_err(|_| "invalid GUID data4".to_string())?;
    }
    Ok(GUID {
        data1,
        data2,
        data3,
        data4,
    })
}

fn active_plan_guid() -> Result<(GUID, String), String> {
    let mut raw: *mut GUID = ptr::null_mut();
    let code: WIN32_ERROR = unsafe { PowerGetActiveScheme(HKEY::default(), &mut raw) };
    if code != ERROR_SUCCESS || raw.is_null() {
        return Err(format!(
            "PowerGetActiveScheme failed with Windows error {}",
            code.0
        ));
    }
    let plan = unsafe { *raw };
    unsafe {
        let _ = LocalFree(HLOCAL(raw as *mut c_void));
    }
    Ok((plan, guid_text(&plan)))
}

/// Query the exact active plan GUID and its AC/DC processor policy values.
pub fn query_snapshot() -> Result<Snapshot, String> {
    let (plan, plan_guid) = active_plan_guid()?;

    let plan_guid_for_errors = plan_guid.clone();
    let read_side = |source: Source| -> Result<SideValues, String> {
        let max_processor_state = match read_raw(&plan, Field::MaxProcessorState(source))? {
            RawRead::Value(value) if value <= 100 => value,
            RawRead::Value(value) => {
                return Err(format!(
                    "{} maximum processor state read back as {value}%, outside the Windows 0-100% policy domain",
                    source.name()
                ))
            }
            RawRead::Unsupported => {
                return Err(format!(
                    "{} maximum processor state is not exposed by Windows power plan {plan_guid_for_errors}",
                    source.name()
                ))
            }
        };
        let boost_mode = match read_raw(&plan, Field::BoostMode(source))? {
            RawRead::Value(value) if value <= super::cpu_policy::MAX_BOOST_MODE => value,
            RawRead::Value(value) => {
                return Err(format!(
                    "{} processor boost mode read back as unknown value {value}",
                    source.name()
                ))
            }
            RawRead::Unsupported => {
                return Err(format!(
                    "{} processor boost mode is not exposed by Windows power plan {plan_guid_for_errors}",
                    source.name()
                ))
            }
        };
        let max_mhz = match read_raw(&plan, Field::MaxFrequencyMhz(source))? {
            RawRead::Value(value)
                if value == 0
                    || (super::cpu_policy::MIN_MAX_FREQUENCY_MHZ
                        ..=super::cpu_policy::MAX_MAX_FREQUENCY_MHZ)
                        .contains(&value) =>
            {
                Some(value)
            }
            RawRead::Value(value) => {
                return Err(format!(
                    "{} maximum processor frequency read back as unknown value {value} MHz",
                    source.name()
                ))
            }
            // The hidden maximum-frequency policy is not present on every
            // plan/processor. Do not invent a MHz ceiling.
            RawRead::Unsupported => None,
        };
        Ok(SideValues {
            max_processor_state,
            boost_mode,
            max_mhz,
        })
    };

    Ok(Snapshot {
        plan_guid,
        ac: read_side(Source::Ac)?,
        dc: read_side(Source::Dc)?,
    })
}

/// PowrProf implementation of the pure transaction backend.
pub struct WindowsPowrProf;

fn current_overlay() -> Result<Option<[u8; 16]>, String> {
    let guid = crate::system::windows_energy::get_actual_overlay_scheme()?;
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&guid.data1.to_be_bytes());
    bytes[4..6].copy_from_slice(&guid.data2.to_be_bytes());
    bytes[6..8].copy_from_slice(&guid.data3.to_be_bytes());
    bytes[8..16].copy_from_slice(&guid.data4);
    Ok(Some(bytes))
}

impl Backend for WindowsPowrProf {
    fn active_plan(&mut self) -> Result<String, String> {
        active_plan_guid().map(|(_, text)| text)
    }

    fn read(&mut self, plan: &str, field: Field) -> Result<Option<Value>, String> {
        let plan = parse_guid(plan)?;
        match read_raw(&plan, field)? {
            RawRead::Value(value) => Ok(Some(match field {
                Field::MaxProcessorState(_) => Value::Percent(value),
                Field::BoostMode(_) => Value::Boost(value),
                Field::MaxFrequencyMhz(_) => Value::Mhz(value),
            })),
            RawRead::Unsupported => Ok(None),
        }
    }

    fn write(&mut self, plan: &str, field: Field, value: &Value) -> Result<(), String> {
        let plan = parse_guid(plan)?;
        let raw = match (*value, field) {
            (Value::Percent(value), Field::MaxProcessorState(_)) => value,
            (Value::Boost(value), Field::BoostMode(_)) => value,
            (Value::Mhz(value), Field::MaxFrequencyMhz(_)) => value,
            _ => {
                return Err(format!(
                    "internal CPU policy value/field mismatch for {}",
                    field.name()
                ))
            }
        };
        write_raw(&plan, field, raw)
    }

    fn activate(&mut self, plan: &str) -> Result<(), String> {
        let plan = parse_guid(plan)?;
        let code = unsafe { PowerSetActiveScheme(HKEY::default(), Some(&plan as *const GUID)) };
        if code == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!(
                "PowerSetActiveScheme(same plan) returned Windows error {}",
                code.0
            ))
        }
    }

    fn read_overlay(&mut self) -> Result<Option<[u8; 16]>, String> {
        current_overlay()
    }

    fn restore_overlay(&mut self, overlay: &[u8; 16]) -> Result<(), String> {
        let guid = crate::system::power::GUID {
            data1: u32::from_be_bytes([overlay[0], overlay[1], overlay[2], overlay[3]]),
            data2: u16::from_be_bytes([overlay[4], overlay[5]]),
            data3: u16::from_be_bytes([overlay[6], overlay[7]]),
            data4: [
                overlay[8],
                overlay[9],
                overlay[10],
                overlay[11],
                overlay[12],
                overlay[13],
                overlay[14],
                overlay[15],
            ],
        };
        let code = crate::system::power::set_active_overlay_scheme(&guid);
        if code == 0 {
            Ok(())
        } else {
            Err(format!(
                "PowerSetActiveOverlayScheme returned Windows error {code}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{guid_text, parse_guid};

    #[test]
    fn guid_text_roundtrips_through_parser() {
        let guid = parse_guid("381b4222-f694-41f0-9685-ff5bb260df2e").unwrap();
        assert_eq!(guid_text(&guid), "381b4222-f694-41f0-9685-ff5bb260df2e");
    }

    #[test]
    fn malformed_guid_is_rejected() {
        assert!(parse_guid("381b4222").is_err());
        assert!(parse_guid("381b4222-f694-41f0-9685-ff5bb260df2z").is_err());
    }

    #[test]
    #[ignore = "Read-only local Windows CPU policy acceptance"]
    fn live_cpu_policy_read() {
        let snapshot = super::query_snapshot().expect("read active Windows CPU policy");
        println!("CPU_POLICY_PLAN={}", snapshot.plan_guid);
        println!("CPU_POLICY_AC={:?}", snapshot.ac);
        println!("CPU_POLICY_DC={:?}", snapshot.dc);
    }

    #[test]
    #[ignore = "Temporarily changes DC processor policy, activates same plan, restores exact state"]
    fn live_cpu_policy_roundtrip() {
        use super::super::cpu_policy::Backend;
        use crate::system::cpu_policy::{self, SideUpdate, Source, Update};
        let before = super::query_snapshot().unwrap();
        let overlay = super::current_overlay().unwrap();
        let old = before.dc.max_processor_state;
        let target = if old == 99 { 100 } else { 99 };
        struct Restore(Option<(String, u32)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                let Some((plan, value)) = self.0.as_ref() else {
                    return;
                };
                let mut backend = super::WindowsPowrProf;
                if backend.active_plan().ok().as_deref() == Some(plan.as_str()) {
                    let overlay = backend.read_overlay().ok().flatten();
                    let _ = backend.write(
                        plan,
                        super::Field::MaxProcessorState(Source::Dc),
                        &super::Value::Percent(*value),
                    );
                    let _ = backend.activate(plan);
                    if let Some(overlay) = overlay {
                        let _ = backend.restore_overlay(&overlay);
                    }
                }
            }
        }
        let mut restore = Restore(Some((before.plan_guid.clone(), old)));
        let mut backend = super::WindowsPowrProf;
        let request = Update {
            ac: SideUpdate::default(),
            dc: SideUpdate {
                max_processor_state: Some(target),
                ..Default::default()
            },
        };
        let result = cpu_policy::execute(&mut backend, &before, &request).unwrap();
        assert!(result.succeeded(), "{result:?}");
        let changed = super::query_snapshot().unwrap();
        assert_eq!(changed.plan_guid, before.plan_guid);
        assert_eq!(changed.dc.max_processor_state, target);
        assert_eq!(super::current_overlay().unwrap(), overlay);
        let request = Update {
            ac: SideUpdate::default(),
            dc: SideUpdate {
                max_processor_state: Some(old),
                ..Default::default()
            },
        };
        let result = cpu_policy::execute(&mut backend, &changed, &request).unwrap();
        assert!(result.succeeded(), "{result:?}");
        assert_eq!(super::query_snapshot().unwrap(), before);
        assert_eq!(super::current_overlay().unwrap(), overlay);
        restore.0 = None;
        println!("CPU_POLICY_ROUNDTRIP DC {old}% -> {target}% -> {old}%; same plan and overlay preserved");
    }
}
