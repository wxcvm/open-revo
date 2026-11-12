#![allow(dead_code)]
#[path = "../../src-tauri/src/system/battery.rs"]
mod battery;
#[path = "../../src-tauri/src/system/display.rs"]
mod display;
#[path = "../../src-tauri/src/system/display_policy.rs"]
mod display_policy;
#[path = "../../src-tauri/src/system/windows_energy.rs"]
mod windows_energy;
#[path = "../../src-tauri/src/system/usage_txn.rs"]
pub mod usage_txn;
#[path = "../../src-tauri/src/system/cpu_policy.rs"]
pub mod cpu_policy;

/// AMD ADLX driver-graphics control model, included source-only (no Tauri, no FFI at runtime).
/// `session.rs` is deliberately NOT included: these tests must never touch the real driver.
pub mod adlx {
    // Inline modules resolve `#[path]` relative to a virtual `adlx/` directory, hence the extra
    // `../` compared to the top-level includes above.
    #[path = "../../../src-tauri/src/hal/adlx/abi.rs"]
    pub mod abi;
    #[path = "../../../src-tauri/src/hal/adlx/engine.rs"]
    pub mod engine;
    #[path = "../../../src-tauri/src/hal/adlx/metrics.rs"]
    pub mod metrics;
}

pub mod vault {
    #[path = "../../../src-tauri/src/hal/vault/p916f_pmc2.rs"]
    pub mod p916f_pmc2;
}
#[path = "../../src-tauri/src/hal/p916f_charge.rs"]
pub mod p916f_charge;
