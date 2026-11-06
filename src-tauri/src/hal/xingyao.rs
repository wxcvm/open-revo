use super::{declarative, vault::xingyao as spec, xingyao_protocol as protocol};

pub fn is_active() -> bool {
    declarative::load_matched_manifest().is_some_and(|model| model.id == spec::MODEL_ID)
}

fn call(input: &[u8]) -> Result<Vec<u8>, String> {
    declarative::wmi_mailbox::invoke_wmi_mailbox(
        "root\\wmi",
        "OemWMIMethod",
        "OemWMIfun",
        input,
        64,
        256,
    )
}

pub fn read_timer() -> Result<u32, String> {
    protocol::word(&call(&spec::GET_TIMER)?).map(u32::from)
}

pub fn set_timer(seconds: u32) -> Result<(), String> {
    protocol::set_timer(call, seconds)
}

pub fn read_mode() -> Result<u8, String> {
    protocol::ui_mode(&call(&spec::GET_MODE)?)
}

pub fn set_mode(mode: u8) -> Result<(), String> {
    protocol::set_mode(call, mode)
}

pub fn read_fans() -> Result<(u16, u16), String> {
    let cpu = protocol::word(&call(&[spec::GET_FAN[0], spec::GET_FAN[1], 0])?)?;
    let second = protocol::word(&call(&[spec::GET_FAN[0], spec::GET_FAN[1], 1])?)?;
    Ok((cpu, second))
}
