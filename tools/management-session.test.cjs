const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const read = name => fs.readFileSync(path.resolve(__dirname, '..', name), 'utf8');

test('Windows manifests inherit the shell token and have no auto-start plugin', () => {
  const manifest = read('src-tauri/build.rs');
  assert.match(manifest, /requestedExecutionLevel level="asInvoker" uiAccess="false"/);
  assert.doesNotMatch(manifest, /level="(?:requireAdministrator|highestAvailable)"/);
  assert.doesNotMatch(read('src-tauri/Cargo.toml'), /tauri-plugin-autostart/);
  assert.doesNotMatch(read('src-tauri/capabilities/default.json'), /autostart:/);
});

test('the standard-user entry registers no firmware, extension, or startup mutation IPC', () => {
  const standard = read('src-tauri/src/system/desktop_session.rs');
  const handlers = standard.slice(standard.indexOf('.invoke_handler('), standard.indexOf('.setup('));
  assert.match(handlers, /manager::manager_session/);
  assert.match(handlers, /manager::manager_save_volume_snapshot/);
  assert.match(handlers, /background_awake::set_background_awake/);
  assert.doesNotMatch(handlers, /hardware_cmd|raw_ec_cmd|market_cmd|updater|widget_manager|autostart/);
  assert.doesNotMatch(standard, /spawn_hardware_state_restore|init_hooks\(|Auto-load|set_autostart/);
  const launch = read('src-tauri/src/lib.rs');
  assert.match(launch, /Mode::Hardware if crate::system::manager::verify_interactive_account\(\)\.is_ok\(\) => run_hardware\(\)/);
  assert.match(standard, /\(Some\(true\), true\) => Mode::RefuseBackgroundElevation/);
  assert.match(standard, /\(None, _\) => Mode::RefuseUnverifiedToken/);
  assert.match(launch, /Mode::Management => desktop_session::run\(background\)/);
  const hardware = launch.slice(launch.indexOf('fn run_hardware()'));
  assert.ok(hardware.indexOf('.setup(') < hardware.indexOf('EcBridge::auto_load()'));
  assert.ok(hardware.indexOf('.plugin(single_instance_plugin)') < hardware.indexOf('.setup('));
});

test('boot, logging, and power restore contain no code to regenerate a removed startup task', () => {
  const settings = read('src-tauri/src/system/autostart.rs');
  assert.match(settings, /ms-settings:startupapps/);
  assert.doesNotMatch(settings, /schtasks|HighestAvailable|RegSetValue|std::process::Command/);
  for (const name of ['lib.rs', 'commands/system_cmd.rs', 'system/power/mod.rs']) {
    assert.doesNotMatch(read('src-tauri/src/' + name), /sync_autostart_target_path|set_autostart_enabled/);
  }
});
