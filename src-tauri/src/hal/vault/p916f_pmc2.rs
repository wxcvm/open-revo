// P916F-STX (MECHREVO Xingyao 14) battery charge-limit vault.
//
// Every port, driver IOCTL, PMC2 command/subcommand, status bit and numeric
// boundary used by `hal::p916f_charge` lives here. The business/HAL layers must
// reference these names instead of repeating wire values.
//
// Provenance: P916F-STX IT5571 EC + ITE PMC2 host interface, as documented in
// the original reverse-engineering work (YMGPwcca/Mechrevo-Xingyao-14) and the
// local validation notes battery-charge-limit.md / battery-threshold-semantics.md.
// Nothing in this file claims a live write was performed by this build.

/// Exact declarative manifest id this transport is scoped to.
pub const MODEL_ID: &str = "mechrevo_xingyao_14_p916f_stx";

/// Registry `HARDWARE\DESCRIPTION\System\BIOS` value this transport is scoped to.
pub const BIOS_VERSION: &str = "1.15";

/// Existing signed inpoutx64 device. This code opens it; it never installs,
/// starts, deletes or downloads a driver.
pub const DEVICE_PATH: &str = r"\\.\inpoutx64";

/// CTL_CODE(40000, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS):
/// input = port LE16, output = one byte.
pub const IOCTL_READ_PORT_UCHAR: u32 = 0x9C40_2004;

/// CTL_CODE(40000, 0x802, METHOD_BUFFERED, FILE_ANY_ACCESS):
/// input = [port lo, port hi, value].
pub const IOCTL_WRITE_PORT_UCHAR: u32 = 0x9C40_2008;

/// PMC2 data port.
pub const PMC2_DATA_PORT: u16 = 0x68;
/// PMC2 command/status port.
pub const PMC2_COMMAND_PORT: u16 = 0x6C;

/// Status bit: output buffer full / data available.
pub const PMC2_STATUS_OBF: u8 = 0x01;
/// Status bit: input buffer full / controller busy.
pub const PMC2_STATUS_IBF: u8 = 0x02;
/// A floating/absent controller reads as all ones.
pub const PMC2_ABSENT: u8 = 0xFF;

/// Charge-limit command family.
pub const PMC2_CMD_CHARGE_LIMIT: u8 = 0xF1;
/// Set threshold 1 (echoes the written byte).
pub const PMC2_CMD_SET_T1: u8 = 0xF2;
/// Set threshold 2 (echoes the written byte).
pub const PMC2_CMD_SET_T2: u8 = 0xF3;

/// Disable/reset. Static-confirmed only in the source material; it also clears
/// both stored thresholds on the EC.
pub const PMC2_SUB_DISABLE: u8 = 0x10;
/// Enable. No response framing established; caller must reread state.
pub const PMC2_SUB_ENABLE: u8 = 0x11;
/// Read enabled state.
pub const PMC2_SUB_READ_ENABLED: u8 = 0x12;
/// Read threshold 1.
pub const PMC2_SUB_READ_T1: u8 = 0x13;
/// Read threshold 2.
pub const PMC2_SUB_READ_T2: u8 = 0x14;

/// Bounded wait for IBF clear / OBF set.
pub const PMC2_WAIT_TIMEOUT_MS: u64 = 200;
/// Poll interval inside the bounded wait.
pub const PMC2_WAIT_POLL_MS: u64 = 1;
/// Absolute poll cap so a broken/stationary clock cannot spin forever.
pub const PMC2_WAIT_MAX_POLLS: u32 = 512;

/// Firmware-accepted numeric range for either threshold field.
pub const PMC2_VALUE_MIN: u8 = 0;
pub const PMC2_VALUE_MAX: u8 = 100;

/// Enabled-state field values.
pub const PMC2_ENABLED_OFF: u8 = 0;
pub const PMC2_ENABLED_ON: u8 = 1;

/// UI/apply policy: T1 must be a real cap in 1..=99.
pub const CHARGE_T1_MIN: u8 = 1;
pub const CHARGE_T1_MAX: u8 = 99;
/// T2 is pinned to 100. Values below 100 are an active-discharge upper boundary
/// on this platform, not a conventional resume threshold.
pub const CHARGE_T2_FULL: u8 = 100;

/// Named mutex serialising PMC2 transactions across processes.
pub const PMC2_MUTEX_NAME: &str = r"Global\OpenRevo.P916F.PMC2";
/// Bounded cross-process ownership wait.
pub const PMC2_MUTEX_WAIT_MS: u32 = 2_000;

/// UI presets: protocol values only. They are not behavioural guarantees.
pub const PRESET_WORKSTATION_T1: u8 = 60;
pub const PRESET_LONG_LIFE_T1: u8 = 80;
