# Native usability enhancement - 2026-11-11

## Delivered behavior

- Removed fixed refresh writes on AC transitions, including the old highest-rate-on-AC behavior. Old `auto_min_refresh_on_battery` preferences read as false; attempts to re-enable the old command return a migration explanation.
- Manual fixed rates validate first, are temporary (no display database write), and retain a refresh-aware `QueryDisplayConfig` snapshot. Returning to Windows or a normal process exit restores it only if the state still matches the app's last write. External changes revoke ownership. No historical DRR state is guessed and DRR is not automatically enabled.
- Added three editable STX usage presets with separate firmware and Windows energy modes. Optional brightness, keyboard idle timeout and fixed internal-display refresh remain unset by default. AC/battery bindings are opt-in and execute only on actual transitions, not polling or initial page display.
- Profile apply preflights old state, verifies accepted changes, reports individual failures and performs conditional rollback. An explicit independent energy choice disables the previous firmware/Windows linkage. Other machine HAL implementations remain intact.
- Battery health uses read-only battery class driver IOCTLs, with no PowerShell, OEM service or charging request. Relative units, unknown capacity/rate and missing cycle counts are preserved.

## Current-machine evidence

- Production battery module: designed 74,978 mWh, full 80,690 mWh, 9 cycles. Charging power is a live sample, not a static specification. Full/design above 100% is reported as raw calibration/capacity information.
- Production display query: DRR flag currently false, virtual rate 30 Hz. This does not establish hardware DRR support or identify who changed it.
- Elevated production display roundtrip: 30 -> 60 -> 30 Hz passed, including exact snapshot restoration. Non-interactive unprivileged tool sessions returned access/validation errors; these are not claimed as product acceptance failures.
- Production Windows energy getter returned best performance. An enabled-DRR roundtrip, real plug/unplug profile trigger and full profile UI/hardware roundtrip remain separate acceptance items.

## Hardware boundaries and remaining acceptance

- The previously captured local WMAA fan dispatcher exposes GFNS reads, not a verified duty/full-speed setter. The charging threshold interface is still unproven on this board. No speculative EC writes, fabricated charging presets or manual fan curve controls were added. This is an inspection of retained local evidence, not a new exhaustive firmware/provider investigation.
- No automatic OEM backend installation or service revival. Optional compatibility remains unavailable until it adds a demonstrated capability and supports exclusive ownership.
- A force kill/crash cannot execute normal-exit restoration. Temporary rate changes do not update the Windows display database; after an abnormal exit the user may need to restore the rate in Windows. Crash-recovery journaling is not implemented.
- The main app's full lib test executable still fails during loader startup with `0xc0000139`; do not count its tests as passed. The isolated Windows system test crate imports the production battery, display and Windows energy modules to avoid unrelated runtime imports.

## Reproduction

```powershell
node --test tools/native-capability-regression.test.cjs
npm run test:vendor-ui
cargo check --locked --manifest-path src-tauri/Cargo.toml --all-targets
cargo test --locked --manifest-path tools/native-system-tests/Cargo.toml
# Read-only hardware samples; no display writes:
cargo test --locked --manifest-path tools/native-system-tests/Cargo.toml live_battery_read -- --ignored --nocapture
cargo test --locked --manifest-path tools/native-system-tests/Cargo.toml live_display_read -- --ignored --nocapture
cargo test --locked --manifest-path tools/native-system-tests/Cargo.toml live_energy_read -- --ignored --nocapture
# Interactive/elevated acceptance; briefly changes and restores the internal panel:
cargo test --locked --manifest-path tools/native-system-tests/Cargo.toml live_display_roundtrip -- --ignored --nocapture
```
