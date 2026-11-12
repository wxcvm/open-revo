# Harness reliability follow-up - 2026-11-12

## Delegation and integration

DeepSeek Harness implemented the transaction engine, native adapter, failure UI
and deterministic tests in a detached worktree. A second Harness session reviewed
power/display ownership and retained firmware evidence; a third reviewed the
Windows unit-test loader failure. The main checkout only received reviewed files,
not generated schemas, global configuration or hardware writes from Harness.

The main agent additionally fixed snapshot/mutex ordering, strict internal-panel
selection, read-only ownership reconciliation and automatic/manual preference
separation. Automated scene selection no longer rewrites manual AC/DC defaults.

## Behavior and API

- Applying a scene preflights all requested values and stops on the first error.
  A setter can mutate and still return an error, so that field participates in
  conditional rollback too. Unknown readback is not reported as restored.
- `ApplyResult` retains `applied`, `failed`, `rollback_failed` and adds `restored`,
  `relinquished`, `unverified`, `reverted`. The UI describes these outcomes instead
  of treating an empty rollback error list as proof of recovery.
- Display rollback uses an opaque checkpoint including pre-scene ownership, not
  an integer Hz value or a global reset. It preserves an earlier owned override
  and refuses to overwrite an externally changed display state.
- Manual energy changes, scene transactions and legacy energy linkage share an
  energy mutex. Legacy linkage checks its enable flag after acquiring that lock.
  An ambiguous manual write releases legacy linkage rather than overwriting an
  already changed OS setting later.
- Scene refresh control requires an internal display and never falls back to an
  external primary monitor. External state changes revoke display ownership.

## Verified evidence

- 12 deterministic transaction tests: early/preflight failure, partial setter
  effects, unknown readback, ownership loss, partial rollback, checkpoint callback,
  exact success and manual energy classification. Two Windows ABI/GUID unit tests
  also pass in the isolated production-module test crate.
- Production display checkpoint roundtrip on the internal panel:
  `30 -> 60 -> 120 -> 60 -> original`, with exact snapshot and ownership checks.
- Production native scene engine/adapter roundtrip: firmware `3 -> 2 -> 3`,
  Windows energy changed to balanced and restored to the prior GUID. Independent
  reads passed; the live configuration SHA-256 was unchanged. This is adapter
  acceptance, not a claim that UI persistence or AC cable transitions were tested.
- Root lib usage-profile tests now execute and pass. The former `0xc0000139`
  loader failure was traced to the missing Common-Controls v6 activation manifest
  in unit-test executables, which import `TaskDialogIndirect`. MSVC link arguments
  supply the dependency; an integration test checks that export under activation.
  `rustc-link-arg-tests` alone was tested and does not fix lib unit-test targets.
  Generic manifest generation is explicitly disabled again for bin targets,
  which retain Tauri's existing administrator resource manifest; otherwise MSVC
  reports duplicate manifest resource #1.
- Targeted browser and source-contract checks remain separate from hardware
  acceptance. We do not run unrelated tests that purge live AppData.

## Review boundaries

The known WMAA mailbox has no fan-duty or battery-threshold write handler in the
retained DSDT. This is not proof that all firmware/driver transports lack them.
The old raw mode documentation was corrected: raw 1 = balance, raw 2 = performance;
raw 3 is not an Office mode on this board. No speculative EC writes were added.

Not every review suggestion was accepted: an opt-in profile on the next real
power transition may intentionally replace a manual selection; that follows the
agreed behavior. A transient display restore failure must not simply report
"follow system" while the app's fixed state remains applied. The dormant arbiter
was not wired globally as part of this focused change.

DRR-enabled restore, physical AC unplug/replug, sleep/resume endurance and crash
recovery journaling remain separate acceptance items. Firmware fan/charging
capabilities remain gated off until demonstrated on this exact board.
