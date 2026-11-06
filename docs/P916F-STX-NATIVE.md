# Xingyao P916F-STX native takeover

## Evidence and scope

Verified on `MECHREVO XINGYAO Series-P916F-STX`, AMD Ryzen AI / Radeon 880M,
2026-11-06. Do **not** apply these definitions to P916F-PTL/Intel or arbitrary
Tongfang EC hardware. A separate exact-board manifest takes precedence over the
older shared P916F manifest. No official binaries or firmware dumps are shipped.

Local DSDT (33599 bytes, SHA256
`7F975B7CB0966DCB984D523C14B68310E4AFC910E4067FA7AEA7AA793BEDC543`)
was disassembled read-only with ACPICA iASL 20250807. Its WMAA dispatch describes:

| Request | Firmware method | Meaning |
|---|---|---|
| `02 01 <index>` | GFNS | One fan per request, index 0 or 1; LE16 RPM at output 1 |
| `03 09` | GPFM | Raw mode 1 = balance, 2 = performance |
| `03 0A <raw>` | SPFM | Set mode; successful local output echoed ECMD 0x94/0x95 |
| `03 0B` | GKBT | LE16 keyboard idle timeout in seconds at output 1 |
| `03 0C <lo> <hi>` | SKBT | Store LE16 seconds; zero means always on |

Windows interface: `root\wmi:OemWMIMethod.OemWMIfun`, instance
`ACPI\PNP0C14\HWMI_0`, 64-byte input / 256-byte output. Read responses require
zero status, and writes require matching readback, never a cached preference.
The native path uses Windows ACPI/WMI, not GCUBridge, GCUService,
ACPIDriverDll.dll or legacy EC IOCTL writes.

## Controlled hardware test

OpenRevo, GCUBridge and GCUService were stopped for isolation; no service startup
settings were changed for this experiment. Tests passed:

- Timer `15 -> 1200 -> 0 -> 600` seconds, exact native readback after every write.
- Modes `1 -> 2 -> 2`, exact native readback after every write.
- Both indexed fans returned live RPM (4934 / 4749 in that sample).
- Finally restored original timer 600, mode 2, official backend and OpenRevo.

This confirms the firmware controls work without the official user-mode backend.
It does not establish reboot/suspend persistence or physical backlight observation.
The live deployment experiment keeps the official installation available for rollback.

## Unsupported features

The inspected WMAA fan group exposes RPM reads, **not** manual duty/strong-cooling
writes. Native fan boost/custom curves and battery-charge limits are not enabled.
Keyboard brightness remains on firmware hotkeys. Do not turn a vendor request flag
or an unsupported API return into a claim of full fan speed or feature parity.

The legacy shared P916F manifest had incorrect assumptions for STX: three power
codes, a second fan packed into the first response, and a boolean keyboard timeout.
The exact STX manifest and typed, verified transactions replace those assumptions.
