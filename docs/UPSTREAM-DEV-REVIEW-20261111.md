# Upstream dev review and native UI cleanup

Reviewed 2026-11-11. Fork baseline: `wxcvm/open-revo@656dab3`.
Upstream: `faintonce/open-revo`, `dev@f2303d0879e024ab6879dbf2d19be19a88ba839f`.
Git reports 32 fork-only / 127 upstream-only commits, 430 changed files from
merge base `b7d2b526e60cd04965eadab21e606bdb1e1aa56e`.

## Selectively adopted

- `146ca47bbc4d252527a5eb42e0d7c73f7ee8cc97`: imported the pure HAL power-option
  helper and its failure/bit-mask tests. Adapted read-only status queries and
  checked option writes to this fork; added native capability guards rather than
  replacing the fork's verified P916F-STX WMI implementation.
- `463a4a0d630609a9a800d6093cb57fd86f7d6ee1`: dark native form-control scheme,
  applied to the host document/CSS. No sample-plugin styling copied wholesale.
- `c606c62b6725d2c71735974f17bde6bb64c7095b`: corrected all five core manifest
  fixture include paths, required to run the imported HAL tests.

## Deliberately not merged

Slint/Wry host replacement, LLM hardware gateway, pet widgets and new hybrid
plugin surfaces add substantial architecture/dependencies and do not address
native control on this machine. Existing other-model drivers, plugin support,
verified native modes/timer/fan telemetry and GPU shared-memory telemetry remain.
No version bump to upstream 0.9.4 and no claim of full upstream synchronization.

## Native cleanup

- Power page uses backend-advertised support; STX no longer exposes charge
  limits, shutdown USB power or AC-recovery controls. All corresponding setter
  entrypoints reject unsupported hardware before saving configuration.
- Remove the irrelevant battery power-gating toggle on exempt thin/iGPU models.
- Native settings/monitor pages neither probe nor recover OEM services. The
  other-model vendor implementation remains available and unchanged in purpose.
- Monitor failures no longer substitute invented RAM/disk samples; unknown fan
  maximums no longer produce estimated percentages or a made-up 6000-RPM ceiling.
- Keep real Windows power/sleep settings; propagate failed policy commands and
  avoid optimistic enabled states. This is not reboot/suspend acceptance.
- Keep both verified native fan channels; a read-only model review must not
  override actual two-channel WMI evidence with an inference from an old manifest.

DeepSeek Harness provided a read-only review; its claims about one fan channel
and unverified keyboard writes were rejected based on production typed protocol
and prior hardware readbacks. No external-model write access was used.
