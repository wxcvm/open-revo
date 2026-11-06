// P916F-STX only. Verified against local DSDT WMAA/GFNS/GPFM/SPFM/GKBT/SKBT.
pub const MODEL_ID: &str = "mechrevo_xingyao_14_p916f_stx";
pub const GET_TIMER: [u8; 2] = [3, 11];
pub const SET_TIMER: [u8; 2] = [3, 12];
pub const GET_MODE: [u8; 2] = [3, 9];
pub const SET_MODE: [u8; 2] = [3, 10];
pub const GET_FAN: [u8; 2] = [2, 1];
pub const BALANCE_RAW: u8 = 1;
pub const TURBO_RAW: u8 = 2;
pub const BALANCE_ACK: u8 = 0x94;
pub const TURBO_ACK: u8 = 0x95;
