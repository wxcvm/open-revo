const assert = require('node:assert/strict');
const path = require('node:path');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');

(async () => {
  const { createServer } = await import('vite');
  const dev = await createServer({ root: path.resolve(__dirname, '..'),
    optimizeDeps: { include: ['react', 'react-dom/client'] },
    server: { host: '127.0.0.1', port: 1443, strictPort: true, hmr: false } });
  let browser;
  try {
    await dev.listen();
    const html = await dev.transformIndexHtml('/awake-fixture.html', `<!doctype html><div id="root"></div><script type="module">
import React from '/node_modules/.vite/deps/react.js';
import ReactDOM from '/node_modules/.vite/deps/react-dom_client.js';
import { BackgroundAwakeSwitch } from '/src/components/overview/BackgroundAwakeSwitch.tsx';
ReactDOM.createRoot(document.getElementById('root')).render(React.createElement(BackgroundAwakeSwitch));</script>`);
    browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH });
    const page = await browser.newPage();
    await page.route('**/awake-fixture.html', route => route.fulfill({ contentType: 'text/html', body: html }));
    await page.addInitScript(() => {
      const callbacks = new Map();
      let listener, next = 0, active = false, attempts = 0;
      window.__TAURI_INTERNALS__ = {
        transformCallback(callback) { callbacks.set(++next, callback); return next; },
        unregisterCallback(id) { callbacks.delete(id); },
        async invoke(name, args) {
          if (name === 'plugin:event|listen') { listener = args.handler; return 1; }
          if (name === 'plugin:event|unlisten') return;
          if (name === 'get_background_awake') return active;
          if (name !== 'set_background_awake') throw new Error(name);
          if (attempts++ === 0) throw 'Windows 请求失败';
          await new Promise(resolve => { window.finishRequest = resolve; });
          active = args.enable;
          return active;
        },
      };
      window.emitFromTray = value => {
        active = value;
        callbacks.get(listener)({ payload: value });
      };
    });
    await page.goto('http://127.0.0.1:1443/awake-fixture.html');
    const button = page.getByRole('button', { name: '后台保活' });
    await page.waitForFunction(() => document.querySelector('button')?.getAttribute('aria-pressed') === 'false');
    await button.click();
    assert.equal(await page.getByRole('alert').textContent(), 'Windows 请求失败');
    assert.equal(await button.getAttribute('aria-pressed'), 'false');
    await button.click();
    await page.waitForFunction(() => typeof window.finishRequest === 'function');
    assert.equal(await button.isDisabled(), true);
    assert.equal(await button.getAttribute('aria-pressed'), 'false');
    await page.evaluate(() => window.finishRequest());
    await page.waitForFunction(() => document.querySelector('button')?.getAttribute('aria-pressed') === 'true');
    await page.evaluate(() => window.emitFromTray(false));
    await page.waitForFunction(() => document.querySelector('button')?.getAttribute('aria-pressed') === 'false');
    console.log('PASS background awake: default off, rejection visible, no false success, tray state synchronized');
  } finally {
    if (browser) await browser.close();
    await dev.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
