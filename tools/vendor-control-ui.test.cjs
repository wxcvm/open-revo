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
const root = ReactDOM.createRoot(document.getElementById('root'));
function Fixture() {
  const control = useVendorControl('vendor_probe_fan', 'Fan/Status', parseFanBoost);
  window.control = control;
  return React.createElement('pre', null, JSON.stringify(control));
}
window.mount = () => root.render(React.createElement(React.StrictMode, null, React.createElement(Fixture)));
window.unmount = () => root.render(null);
window.mount();
</script></body></html>`);
    browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH });
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.route('**/fixture.html', route => route.fulfill({ contentType: 'text/html', body: html }));
    await page.addInitScript(() => {
      const f = window.fixture = { calls: [], running: true, failProbe: true, failWrite: false, boost: false,
        deferConnect: false, deferProbe: false, probeReleases: [] };
      window.__TAURI_INTERNALS__ = { async invoke(name) {
        f.calls.push(name);
        const state = () => ({ present: true, selected: true, running: f.running, auto_start: true });
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
        return { messages: [{ topic: 'Fan/Status', payload: JSON.stringify({ FanBoostEnable: Number(f.boost) }) }], elapsed_ms: 8 };
      } };
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
  } finally {
    await browser?.close();
    await dev.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
