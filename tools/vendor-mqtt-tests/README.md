# Vendor control regression tests

These tests never connect to the OEM broker or change hardware settings.

## Transport

Run `npm run test:vendor-mqtt` (or the Cargo command in `package.json`).
The tiny independent crate imports the **production** MQTT module, starts local
mock brokers on ephemeral ports, and does not load Tauri or Windows GUI DLLs.
It tests verified writes without replay, missing replies versus broken framing,
bounded packet lengths, fragmented TCP, subscription refusal and clean disconnect.
Timing-sensitive socket cases run serially, with the same deadlines as production.

## UI lifetime

Run `npm run test:vendor-ui` with Playwright available to Node and its Chromium
installed. If using a shared installation, set `PLAYWRIGHT_MODULE` to that module's
absolute path; `CHROMIUM_PATH` optionally selects a browser executable.
The test launches a local Vite server and the production React hook under
StrictMode with mocked IPC. It checks rejection, duplicate clicks, unmount during
connect/readiness retry and remount isolation.

These are protocol/UI regressions, **not** confirmation that firmware applied a
fan request. Actual fan effectiveness needs RPM observation on the target model;
independent control needs a verified native hardware mapping.
