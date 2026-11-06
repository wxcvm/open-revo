// Uses production hooks in a real browser; all hardware IPC is mocked.
const assert = require('node:assert/strict');
const { pathToFileURL } = require('node:url');
const path = require('node:path');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');

(async () => {
  const root = path.resolve(__dirname, '..');
  const { createServer } = await import(pathToFileURL(path.join(root, 'node_modules/vite/dist/node/index.js')));
  const dev = await createServer({ root, optimizeDeps: { include: ['react', 'react-dom/client'] },
    server: { host: '127.0.0.1', port: 1431, strictPort: true, hmr: false } });
  let browser;
  try {
    await dev.listen();
    const html = await dev.transformIndexHtml('/fixture.html', `<!doctype html><html><body><div id="root"></div>
<script type="module">
import React from '/node_modules/.vite/deps/react.js';
import ReactDOM from '/node_modules/.vite/deps/react-dom_client.js';
import { useVendorControl, parseFanBoost } from '/src/state/vendorControl.ts';
import { SystemSettingsView } from '/src/components/SystemSettingsView.tsx';
import { ModeSelector } from '/src/components/ModeSelector.tsx';
import { MiniDrawer } from '/src/components/MiniDrawer.tsx';
const root = ReactDOM.createRoot(document.getElementById('root'));
function Fixture() {
  const control = useVendorControl('vendor_probe_fan', 'Fan/Status', parseFanBoost);
  window.control = control;
  return React.createElement('pre', null, JSON.stringify(control));
}
window.mount = () => root.render(React.createElement(React.StrictMode, null, React.createElement(Fixture)));
window.unmount = () => root.render(null);
window.mountNative = () => root.render(React.createElement(React.Fragment, null,
  React.createElement(SystemSettingsView, {
    status: {vendor_bus_selected:false},
    capabilities: {power_modes:['balance','turbo'], rgb_keyboard: {
      sleep_timeout_supported:true, timeout_options_seconds:[0,10,15,20,30,45,60,120,300,600,1200,1800,3600,7200,10800]}},
    osdConfig:{enabled:false,theme:'fade',scale:'1',position:'top-left'}, onUpdateOsdConfig(){},onToggleAutostart(){}
  }),
  React.createElement(ModeSelector, {currentMode:3,onSelectMode(){}, capabilities:{power_modes:['balance','turbo']}})
));
window.mountMini = () => root.render(React.createElement(MiniDrawer, {
  status:{power_mode:3,gpu_name:'AMD Radeon 880M',gpu_vram_mb:16020,gpu_vram_used_mb:2000,cpu_fan_rpm:3000,
    vendor_bus_selected:false,support_flags:{has_battery_limit:false},available_refresh_rates:[60,120],monitors:[]},
  capabilities:{id:'mechrevo_xingyao_14_p916f_stx',form_factor:'thin_light',discrete_gpu:false,
    power_modes:['balance','turbo'],battery_charge_limit:false,rgb_keyboard:{supported:false,sleep_timeout_supported:true,
      timeout_options_seconds:[0,10,15,20,30,45,60,120,300,600,1200,1800,3600,7200,10800]}},
  onSetPowerMode(){},onToggleFanBoost(){},onToggleBatteryLimit(){},onSwitchRefreshRate(){},onToggleWinKey(){},
  onToggleAutostart(){},async onToggleOemService(){},onExpandDashboard(){},onOpenSponsor(){},onMinimize(){},onClose(){}
}));
window.mount();
</script></body></html>`);
    browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH });
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.route('**/fixture.html', route => route.fulfill({ contentType: 'text/html', body: html }));
    await page.addInitScript(() => {
      const f = window.fixture = { calls: [], running: true, failProbe: true, failWrite: false, boost: false,
        deferConnect: false, deferProbe: false, probeReleases: [], selected:true, timer:600 };
      window.__TAURI_INTERNALS__ = { transformCallback() {return 1;}, async invoke(name, args) {
        f.calls.push(name);
        const state = () => ({ present: true, selected: f.selected, running: f.running, auto_start: true });
        if (name === 'get_keyboard_sleep_timeout') return f.timer;
        if (name === 'set_keyboard_sleep_timeout') {
          if (f.failWrite) throw new Error('native readback mismatch');
          f.timer = args.seconds; return;
        }
        if (name === 'vendor_service_state') return state();
        if (name === 'vendor_connect_service') {
          if (f.deferConnect) await new Promise(resolve => { f.releaseConnect = resolve; });
          return state();
        }
        if (name === 'vendor_recover_service') { f.running = true; return; }
        if (name === 'vendor_probe_fan') {
          if (f.deferProbe) await new Promise(resolve => f.probeReleases.push(resolve));
          if (f.failProbe) throw new Error('no confirmed reply');
        }
        if (name === 'vendor_fan_boost') {
          if (f.failWrite) throw new Error('write outcome unconfirmed');
          f.boost = !f.boost;
        }
        if (name.startsWith('vendor_')) return { messages: [{ topic: 'Fan/Status', payload: JSON.stringify({ FanBoostEnable: Number(f.boost) }) }], elapsed_ms: 8 };
        return null;
      } };
      window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {unregisterListener(){}};
    });
    await page.goto('http://127.0.0.1:1431/fixture.html');
    await page.waitForFunction(() => window.control?.service && !window.control.checking);
    assert.equal(await page.evaluate(() => window.fixture.calls.includes('vendor_recover_service')), false);
    console.log('PASS running backend: a missing reply does not trigger service recovery');

    await page.evaluate(() => { window.fixture.failProbe = false; void window.control.connect(); });
    await page.waitForFunction(() => window.control.snapshot && !window.control.busy);
    await page.evaluate(() => { window.fixture.failWrite = true; void window.control.write('vendor_fan_boost', {}); });
    await page.waitForFunction(() => window.control.error.includes('unconfirmed'));
    assert.equal(await page.evaluate(() => window.control.snapshot.value), false);
    console.log('PASS rejected write: last confirmed snapshot is retained, no false enabled state');

    await page.evaluate(() => {
      window.fixture.deferConnect = true;
      void window.control.connect(); void window.control.connect();
    });
    const before = await page.evaluate(() => window.fixture.calls.filter(x => x === 'vendor_probe_fan').length);
    await page.evaluate(() => window.unmount());
    await page.waitForFunction(() => !document.querySelector('pre'));
    await page.evaluate(() => window.fixture.releaseConnect());
    await page.waitForTimeout(700);
    assert.equal(await page.evaluate(() => window.fixture.calls.filter(x => x === 'vendor_probe_fan').length), before);
    assert.equal(await page.evaluate(() => window.fixture.calls.filter(x => x === 'vendor_connect_service').length), 2);
    console.log('PASS unmount during connect: no late hardware probe; duplicate clicks submit once');

    await page.evaluate(() => {
      window.fixture.deferConnect = false; window.fixture.failProbe = true;
      window.mount();
    });
    await page.waitForFunction(() => window.control && !window.control.checking && document.querySelector('pre'));
    await page.evaluate(() => { void window.control.connect(); });
    await page.waitForFunction(() => window.control.busy);
    await page.waitForTimeout(100);
    await page.evaluate(() => window.unmount());
    await page.waitForFunction(() => !document.querySelector('pre'));
    const stoppedAt = await page.evaluate(() => window.fixture.calls.length);
    await page.waitForTimeout(900);
    assert.equal(await page.evaluate(() => window.fixture.calls.length), stoppedAt);
    console.log('PASS unmount during readiness retry: polling stops instead of running for 15 seconds');

    await page.evaluate(() => { window.fixture.running = false; window.fixture.failProbe = false; window.mount(); });
    await page.waitForFunction(() => window.control.snapshot && !window.control.checking && document.querySelector('pre'));
    assert.deepEqual(errors, []);
    console.log('PASS StrictMode remount: new lifetime receives its own snapshot and releases pending state');

    const probes = await page.evaluate(() => window.fixture.calls.filter(x => x === 'vendor_probe_keyboard').length);
    await page.evaluate(() => {
      window.fixture.selected=false; window.fixture.running=false; window.fixture.failWrite=false;
      window.mountNative();
    });
    const timer = page.getByLabel('原生背光休眠延时');
    await page.waitForFunction(() => document.querySelector('[aria-label="原生背光休眠延时"]')?.value === '600');
    assert.equal(await timer.locator('option').count(), 15);
    await timer.selectOption('1200');
    await page.getByText('通过原生 WMI 直接设置，无需官方后台；已回读 20 分钟').waitFor();
    await page.evaluate(() => {window.fixture.failWrite=true;});
    await timer.selectOption('15');
    await page.getByRole('alert').filter({hasText:'native readback mismatch'}).waitFor();
    assert.equal(await timer.inputValue(), '1200');
    assert.equal(await page.evaluate(() => window.fixture.calls.filter(x => x === 'vendor_probe_keyboard').length), probes);
    assert.equal(await page.getByRole('button', {name:/办公/}).count(), 0);
    console.log('PASS native UI: 15 timeout options, confirmed values only, two physical modes, no OEM probes');
    await page.evaluate(() => {window.fixture.timer=600; window.fixture.failWrite=false; window.mountMini();});
    const miniTimer = page.getByLabel('迷你面板原生背光延时');
    await page.waitForFunction(() => document.querySelector('[aria-label="迷你面板原生背光延时"]')?.value === '600');
    assert.equal(await page.locator('[data-slot="battery"]').count(), 0);
    assert.equal(await miniTimer.locator('option').count(), 15);
    await miniTimer.selectOption('1200');
    await page.waitForFunction(() => !document.querySelector('[aria-label="迷你面板原生背光延时"]').disabled);
    await page.evaluate(() => {window.fixture.failWrite=true;});
    await miniTimer.selectOption('15');
    await page.getByRole('alert').filter({hasText:'native readback mismatch'}).waitFor();
    assert.equal(await miniTimer.inputValue(),'1200');
    console.log('PASS mini UI: unsupported charging controls hidden; native timer has all presets and rejects false success');
    assert.deepEqual(errors, []);
  } finally {
    await browser?.close();
    await dev.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
