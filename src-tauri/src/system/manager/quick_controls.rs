//! Direct Windows actions limited to the current playback endpoint and OS power
//! overlay. No firmware, package execution, refresh rate or queued policies.
use super::win::{self, Apartment, Outcome};
use crate::system::windows_energy as energy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use windows::Win32::{
    Media::Audio::{
        eConsole, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator,
    },
    System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL},
};

static SERIAL: Mutex<()> = Mutex::new(());

#[derive(Serialize)]
struct Availability<T> {
    value: Option<T>,
    problem: Option<String>,
}

impl<T> From<Outcome<T>> for Availability<T> {
    fn from(result: Outcome<T>) -> Self {
        match result {
            Ok(value) => Self {
                value: Some(value),
                problem: None,
            },
            Err(problem) => Self {
                value: None,
                problem: Some(problem),
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct AudioReading {
    device: String,
    volume: u8,
    muted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PowerMode {
    Efficiency,
    Balanced,
    Performance,
}

impl PowerMode {
    fn from_guid(guid: energy::GUID) -> Outcome<Self> {
        match guid {
            energy::GUID_OFFICE_SAVER => Ok(Self::Efficiency),
            energy::GUID_BALANCED => Ok(Self::Balanced),
            energy::GUID_TURBO_PERF => Ok(Self::Performance),
            _ => Err("Windows 正在使用其他能源模式，本面板不覆盖它。".into()),
        }
    }

    fn guid(self) -> energy::GUID {
        match self {
            Self::Efficiency => energy::GUID_OFFICE_SAVER,
            Self::Balanced => energy::GUID_BALANCED,
            Self::Performance => energy::GUID_TURBO_PERF,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Snapshot {
    audio: Availability<AudioReading>,
    power: Availability<PowerMode>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Action {
    Volume {
        device: String,
        expected: u8,
        level: u8,
    },
    Mute {
        device: String,
        expected: bool,
        muted: bool,
    },
    Energy {
        expected: PowerMode,
        mode: PowerMode,
    },
}

fn output() -> Outcome<(String, IAudioEndpointVolume)> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(win::error)?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(win::error)?;
        let raw = device.GetId().map_err(win::error)?;
        let id = raw.to_string().map_err(win::error);
        CoTaskMemFree(Some(raw.0.cast()));
        let volume = device.Activate(CLSCTX_ALL, None).map_err(win::error)?;
        Ok((id?, volume))
    }
}

fn read_output(device: String, endpoint: &IAudioEndpointVolume) -> Outcome<AudioReading> {
    unsafe {
        let value = endpoint.GetMasterVolumeLevelScalar().map_err(win::error)?;
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err("Windows 返回了无效的音量读数。".into());
        }
        Ok(AudioReading {
            device,
            volume: (value * 100.0).round() as u8,
            muted: endpoint.GetMute().map_err(win::error)?.as_bool(),
        })
    }
}

fn read_audio() -> Outcome<AudioReading> {
    let _apartment = Apartment::new()?;
    let (id, endpoint) = output()?;
    read_output(id, &endpoint)
}

fn read_power(hardware: bool) -> Outcome<PowerMode> {
    if hardware {
        return Err("硬件会话中的能源模式在原硬件面板调节，以免覆盖其电源联动。".into());
    }
    PowerMode::from_guid(energy::get_actual_overlay_scheme()?)
}

fn snapshot(hardware: bool) -> Snapshot {
    Snapshot {
        audio: read_audio().into(),
        power: read_power(hardware).into(),
    }
}

pub(super) fn read(hardware: bool) -> Outcome<Snapshot> {
    win::verify_account()?;
    let _guard = SERIAL.lock();
    Ok(snapshot(hardware))
}

fn change_audio(
    device: &str,
    check: impl FnOnce(&AudioReading) -> bool,
    apply: impl FnOnce(&IAudioEndpointVolume) -> windows::core::Result<()>,
) -> Outcome<AudioReading> {
    let _apartment = Apartment::new()?;
    let (id, endpoint) = output()?;
    if id != device {
        return Err("默认播放设备已变化，未修改；已重新读取它。".into());
    }
    if !check(&read_output(id.clone(), &endpoint)?) {
        return Err("播放设备状态已变化，未覆盖系统设置；已重新读取。".into());
    }
    apply(&endpoint).map_err(win::error)?;
    let verified = read_output(id, &endpoint)?;
    if output()?.0 != device {
        return Err("原播放设备已回读，但默认设备在操作期间切换；已重新读取。".into());
    }
    Ok(verified)
}

pub(super) fn apply(action: Action, hardware: bool) -> Outcome<Snapshot> {
    win::verify_account()?;
    let _guard = SERIAL.lock();
    match action {
        Action::Volume {
            device,
            expected,
            level,
        } => {
            if level > 100 {
                return Err("音量范围为 0–100。".into());
            }
            let audio = change_audio(
                &device,
                |r| r.volume == expected,
                |endpoint| unsafe {
                    endpoint.SetMasterVolumeLevelScalar(f32::from(level) / 100.0, std::ptr::null())
                },
            )?;
            Ok(Snapshot {
                audio: Ok(audio).into(),
                power: read_power(hardware).into(),
            })
        }
        Action::Mute {
            device,
            expected,
            muted,
        } => {
            let audio = change_audio(
                &device,
                |r| r.muted == expected,
                |endpoint| unsafe { endpoint.SetMute(muted, std::ptr::null()) },
            )?;
            if audio.muted != muted {
                return Err("Windows 回读的静音状态与请求不同。".into());
            }
            Ok(Snapshot {
                audio: Ok(audio).into(),
                power: read_power(hardware).into(),
            })
        }
        Action::Energy { expected, mode } => {
            let _power_guard = crate::system::vendor_bus::ENERGY_LOCK.lock();
            let before = read_power(hardware)?;
            if before != expected {
                return Err("Windows 能源模式已发生变化，未覆盖；已重新读取。".into());
            }
            let guid = mode.guid();
            let status = energy::set_active_overlay_scheme(&guid);
            let after = read_power(hardware)?;
            if status != 0 || after != mode {
                return Err(format!(
                    "Windows 未确认所选能源模式（状态码 {status}；回读 {after:?}）。"
                ));
            }
            Ok(Snapshot {
                audio: read_audio().into(),
                power: Ok(after).into(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_overlay_guids_get_a_selectable_mode() {
        for mode in [
            PowerMode::Efficiency,
            PowerMode::Balanced,
            PowerMode::Performance,
        ] {
            assert_eq!(PowerMode::from_guid(mode.guid()).unwrap(), mode);
        }
        let mut unknown = PowerMode::Balanced.guid();
        unknown.data1 = 42;
        assert!(PowerMode::from_guid(unknown).is_err());
        assert!(read_power(true).is_err());
    }

    #[test]
    #[ignore = "read-only local playback and Windows overlay acceptance"]
    fn manager_live_quick_controls() {
        let value = read(false).unwrap();
        println!(
            "PLAYBACK={:?} POWER={:?}",
            value.audio.value.as_ref().map(|a| (a.volume, a.muted)),
            value.power.value
        );
        assert!(value.audio.value.is_some(), "{:?}", value.audio.problem);
        assert!(value.power.value.is_some(), "{:?}", value.power.problem);
    }
}
