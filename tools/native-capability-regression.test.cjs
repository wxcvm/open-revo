const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '..');
const read = file => fs.readFileSync(path.join(root, file), 'utf8');
const commands = read('src-tauri/src/commands/power_cmd.rs');

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
