const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '..');
const read = file => fs.readFileSync(path.join(root, file), 'utf8');
const commands = read('src-tauri/src/commands/power_cmd.rs');

test('native cooling exclusion precedes stored overrides and paired-device probing', () => {
  const config=read('src-tauri/src/system/config.rs');
  const body=config.slice(config.indexOf('pub fn is_water_cooler_enabled'), config.indexOf('pub fn get_module_toggles'));
  assert.ok(body.indexOf('xingyao::is_active') < body.indexOf('load_config'));
  const cooler=read('src-tauri/src/commands/cooler_cmd.rs');
  const start=cooler.slice(cooler.indexOf('pub fn start_water_cooler'),cooler.indexOf('pub fn get_water_cooler_status'));
  assert.ok(start.indexOf('xingyao::is_active') < start.indexOf('get_global_cooler'));
});

test('power queries cannot write hardware or commit preferences', () => {
  const query = commands.slice(commands.indexOf('pub fn get_power_settings'), commands.indexOf('fn get_active_power_scheme_guid'));
  assert.doesNotMatch(query, /wmi_write_ec|save_config|apply_ec_option/);
  assert.equal((query.match(/supports_ec_power_options &&/g) || []).length, 2);
});

test('all battery setter entrypoints reject unsupported hardware before mutation', () => {
  for (const name of ['set_battery_limit', 'set_battery_mode', 'set_battery_hardware_limit']) {
    const start = commands.indexOf(`pub fn ${name}(`);
    const next = commands.indexOf('#[tauri::command', start + 1);
    const body = commands.slice(start, next);
    const guard = body.indexOf('get_hardware_support_flags().has_battery_limit');
    assert.ok(guard >= 0, name);
    assert.ok(guard < body.indexOf('save_config'), name);
  }
});

test('hardware options cannot blindly overwrite a register after a failed read', () => {
  const start = commands.indexOf('pub fn set_power_option(');
  const end = commands.indexOf('pub fn decode_ansi_or_utf8', start);
  const body = commands.slice(start, end);
  assert.match(body, /needs_acpi_kernel_driver/);
  assert.match(body, /apply_ec_option/);
  assert.doesNotMatch(body, /wmi_write_ec/);
  assert.ok(body.indexOf('apply_ec_option') < body.indexOf('cfg.usb_charge_enabled'));
});

test('Windows policy failure is propagated rather than saved as success', () => {
  const policy = read('src-tauri/src/system/power/ppm.rs');
  const body = policy.slice(policy.indexOf('pub fn apply_sleep_guard_option'), policy.indexOf('pub fn apply_all_sleep_guard'));
  assert.doesNotMatch(body, /let _ = run_powercfg/);
  assert.match(body, /run_powercfg_checked/);
  const single = commands.slice(commands.indexOf('pub fn set_sleep_guard_option'), commands.indexOf('pub fn set_master_sleep_guard'));
  assert.ok(single.indexOf('apply_sleep_guard_option(&option, enable)?') < single.indexOf('save_config'));
});


test('power transitions never set fixed refresh rates or announce unconfirmed success', () => {
  const watcher=read('src-tauri/src/system/switches/watcher.rs');
  assert.doesNotMatch(watcher, /set_monitor_refresh_rate|auto_min_refresh_on_battery|refresh-rate-changed/);
  assert.match(watcher, /previous != is_ac/);
});

test('display changes are explicit, temporary and restored only while still owned', () => {
  const policy=read('src-tauri/src/system/display_policy.rs');
  assert.match(policy, /snapshot\(\)\?\.signature != \*previous/);
  assert.match(policy, /restore\(original\)\?/);
  const display=read('src-tauri/src/system/display.rs');
  assert.doesNotMatch(display, /CDS_UPDATEREGISTRY/);
  assert.match(display, /Display validation rejected/);
});

test('battery health only issues query IOCTLs and preserves unknown capacity and rate', () => {
  const battery=read('src-tauri/src/system/battery.rs');
  assert.doesNotMatch(battery, /0x294048|IOCTL_BATTERY_SET_INFORMATION|powershell/);
  assert.match(battery, /i32::MIN/);
  assert.match(battery, /value == u32::MAX/);
});
