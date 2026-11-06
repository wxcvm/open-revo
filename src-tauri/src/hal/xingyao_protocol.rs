// Pure codec/verified transactions: no Windows, service, DLL or register access.
use crate::hal::vault::xingyao as spec;

pub fn word(response: &[u8]) -> Result<u16, String> {
    if response.len() < 3 || response[0] != 0 {
        return Err("原生 WMI 状态无效或固件拒绝请求".into());
    }
    Ok(u16::from_le_bytes([response[1], response[2]]))
}

pub fn timer_frame(seconds: u32) -> Result<[u8; 4], String> {
    let bytes = u16::try_from(seconds)
        .map_err(|_| "背光延时必须在 0–65535 秒之间")?
        .to_le_bytes();
    Ok([spec::SET_TIMER[0], spec::SET_TIMER[1], bytes[0], bytes[1]])
}

pub fn set_timer(
    mut call: impl FnMut(&[u8]) -> Result<Vec<u8>, String>,
    seconds: u32,
) -> Result<(), String> {
    let frame = timer_frame(seconds)?;
    let response = call(&frame)?;
    if response.first() != Some(&0) {
        return Err("固件未接受背光延时设置".into());
    }
    let actual = word(&call(&spec::GET_TIMER)?)?;
    if u32::from(actual) != seconds {
        return Err(format!(
            "背光延时回读不符：请求 {seconds} 秒，实际 {actual} 秒"
        ));
    }
    Ok(())
}

pub fn raw_mode(ui_mode: u8) -> Result<u8, String> {
    match ui_mode {
        2 => Ok(spec::BALANCE_RAW),
        3 => Ok(spec::TURBO_RAW),
        _ => Err("本机固件仅提供均衡和性能两种原生模式".into()),
    }
}

pub fn ui_mode(response: &[u8]) -> Result<u8, String> {
    match word(response)? {
        value if value == u16::from(spec::BALANCE_RAW) => Ok(2),
        value if value == u16::from(spec::TURBO_RAW) => Ok(3),
        _ => Err("固件返回未知性能模式".into()),
    }
}

pub fn set_mode(
    mut call: impl FnMut(&[u8]) -> Result<Vec<u8>, String>,
    mode: u8,
) -> Result<(), String> {
    let raw = raw_mode(mode)?;
    let response = call(&[spec::SET_MODE[0], spec::SET_MODE[1], raw])?;
    let ack = if raw == spec::BALANCE_RAW {
        spec::BALANCE_ACK
    } else {
        spec::TURBO_ACK
    };
    // SPFM returns ECMD's command byte, not the zero status used by read methods.
    if response.first() != Some(&ack) && response.first() != Some(&0) {
        return Err("固件未接受性能模式设置".into());
    }
    if ui_mode(&call(&spec::GET_MODE)?)? != mode {
        return Err("性能模式回读不符，未确认切换成功".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_is_little_endian_seconds_not_a_boolean() {
        assert_eq!(timer_frame(0).unwrap(), [3, 12, 0, 0]);
        assert_eq!(timer_frame(15).unwrap(), [3, 12, 15, 0]);
        assert_eq!(timer_frame(1200).unwrap(), [3, 12, 176, 4]);
        assert_eq!(timer_frame(65535).unwrap(), [3, 12, 255, 255]);
        assert!(timer_frame(65536).is_err());
    }

    #[test]
    fn timer_requires_matching_readback_and_does_not_replay() {
        let mut calls = vec![];
        let result = set_timer(
            |frame| {
                calls.push(frame.to_vec());
                Ok(if calls.len() == 1 {
                    vec![0]
                } else {
                    vec![0, 88, 2]
                })
            },
            1200,
        );
        assert!(result.is_err());
        assert_eq!(calls, vec![vec![3, 12, 176, 4], vec![3, 11]]);
    }

    #[test]
    fn mode_uses_two_verified_firmware_codes() {
        assert_eq!(raw_mode(2).unwrap(), 1);
        assert_eq!(raw_mode(3).unwrap(), 2);
        assert!(raw_mode(1).is_err());
        assert!(raw_mode(4).is_err());
        assert!(ui_mode(&[0, 3, 0]).is_err());
    }

    #[test]
    fn mode_accepts_firmware_command_echo_and_checks_readback() {
        let mut calls = vec![];
        set_mode(
            |frame| {
                calls.push(frame.to_vec());
                Ok(if calls.len() == 1 {
                    vec![0x95]
                } else {
                    vec![0, 2, 0]
                })
            },
            3,
        )
        .unwrap();
        assert_eq!(calls, vec![vec![3, 10, 2], vec![3, 9]]);
        let mut count = 0;
        assert!(set_mode(
            |_| {
                count += 1;
                Ok(vec![1])
            },
            3
        )
        .is_err());
        assert_eq!(count, 1);
    }

    #[test]
    fn reads_reject_short_error_and_unknown_responses() {
        assert!(word(&[]).is_err());
        assert!(word(&[0, 1]).is_err());
        assert!(word(&[1, 88, 2]).is_err());
        assert_eq!(word(&[0, 88, 2]).unwrap(), 600);
    }
}
