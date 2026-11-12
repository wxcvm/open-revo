//! AMD ADLX driver graphics controls (Anti-Lag, Chill, Image Sharpening, Frame Rate Target
//! Control). Not overclocking: no clock, voltage, fan or power-limit write is bound, because the
//! installed driver reports every manual tuning path unsupported on this GPU.
//!
//! Layout:
//! * `abi`     - hand-transcribed C vtable/struct declarations (see the provenance header there).
//! * `engine`  - pure support/state model, write verification, mutual exclusion, restore ledger.
//! * `session` - absolute-path ADLX DLL loading, interface lifetime, `GfxBackend` implementation.
//! * `service` - serialized session entry points + process-wide ledger.

pub mod abi;
pub mod engine;
pub mod metrics;
pub mod service;
pub mod session;

pub use engine::{
    ChillState, DriverGraphicsState, FieldValue, FrtcState, GfxBackend, GfxController, GfxFeature,
    GfxField, IntRange, Ledger, RestoreReport, SessionInfo, ToggleState, TuningEvidence,
    UpdateReport,
};
pub use service::{
    changed_fields, read_state, restore_session, set_chill_fps, set_frtc_fps, set_toggle,
};
pub use session::{system_adlx_dll_path, AdlxBackend, AdlxSession};
