// Production power page + production stylesheet/shell, with read-only mocked IPC.
const assert = require('node:assert/strict');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');

(async () => {
  const root = path.resolve(__dirname, '..');
  const { createServer } = await import(pathToFileURL(path.join(root, 'node_modules/vite/dist/node/index.js')));
  const dev = await createServer({ root, optimizeDeps: { include: ['react', 'react-dom/client'] },
    server: { host: '127.0.0.1', port: 1433, strictPort: true, hmr: false } });
  let browser;
  try {
    await dev.listen();
    const html = await dev.transformIndexHtml('/scroll-fixture.html', `<!doctype html><html data-theme="mono"><head>
<style>.fixture-header{height:40px;flex-shrink:0}.fixture-nav{height:48px;flex-shrink:0}</style>
</head><body><div id="root"></div><script type="module">
import React from '/node_modules/.vite/deps/react.js';
import ReactDOM from '/node_modules/.vite/deps/react-dom_client.js';
import '/src/index.css';
import { PowerTuningView } from '/src/components/PowerTuningView.tsx';
const root = ReactDOM.createRoot(document.getElementById('root'));
root.render(React.createElement('div', {className:'openrevo-shell', 'data-layout':'grid'},
 React.createElement('header',{className:'shell-titlebar fixture-header'},'OpenRevo'),
 React.createElement('nav',{className:'shell-navbar fixture-nav'},'电源管理'),
 React.createElement('main',{className:'shell-viewport'},React.createElement(PowerTuningView,{sleepModeName:'均衡'}))));
</script></body></html>`);
    browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH });
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    await page.route('**/scroll-fixture.html', r => r.fulfill({ contentType: 'text/html', body: html }));
    await page.addInitScript(() => {
      window.calls = [];
      window.__TAURI_INTERNALS__ = { transformCallback(){return 1;}, async invoke(name) {
        window.calls.push(name);
        if(name === 'get_cpu_policy') return {plan_guid:'11111111-2222-3333-4444-555555555555',
          ac:{max_processor_state:100,boost_mode:2,max_mhz:0},dc:{max_processor_state:100,boost_mode:2,max_mhz:2900}};
        if(name === 'get_native_charge_state') return {matched:true,available:true,reason:null,enabled:0,threshold1:0,threshold2:0};
        if(name === 'get_power_settings') return {supports_battery_limit:false,supports_ec_power_options:false,
          battery_percent:98,is_ac_online:true,high_perf_scheme:false,sleep_auto_office:false,sleep_guard_enabled:false};
        if(name === 'get_display_policy') return {policy:'follow_system',paths:[]};
        if(name === 'get_battery_health' || name.includes('plugin')) return [];
        return null;
      }};
      window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {unregisterListener(){}};
    });
    for (const size of [{width:1280,height:800},{width:960,height:640}]) {
      await page.setViewportSize(size);
      await page.goto('http://127.0.0.1:1433/scroll-fixture.html');
      await page.locator('.cpu-policy-summary').waitFor();
      const viewport = page.locator('.shell-viewport');
      const navTop = (await page.locator('nav').boundingBox()).y;
      for (const expanded of [false, true]) {
        if(expanded) await page.locator('.cpu-policy-summary').click();
        await viewport.evaluate(el => {el.scrollTop=0;});
        const box = await viewport.boundingBox();
        await page.mouse.move(box.x+12,box.y+box.height/2);
        await page.mouse.wheel(0,10000);
        await page.waitForTimeout(350);
        const layout = await viewport.evaluate(el => {
          const view=el.getBoundingClientRect();
          const power=el.querySelector('.power-view-container');
          const last=power.lastElementChild.getBoundingClientRect();
          const children=[...power.children].filter(e=>e.getBoundingClientRect().height>0).map(e=>e.getBoundingClientRect());
          return {top:el.scrollTop,client:el.clientHeight,total:el.scrollHeight,lastBottom:last.bottom,viewBottom:view.bottom,
            overlap:children.some((r,i)=>i>0 && r.top < children[i-1].bottom-1)};
        });
        assert.ok(layout.top>0, `wheel must scroll ${JSON.stringify({size,expanded,layout})}`);
        assert.ok(layout.lastBottom<=layout.viewBottom+2,'last battery section must be reachable');
        assert.equal(layout.overlap,false,'sections must not shrink/overlap');
        assert.equal((await page.locator('nav').boundingBox()).y,navTop,'navigation stays fixed');
        await page.mouse.wheel(0,-10000);
        await page.waitForTimeout(350);
        assert.equal(await viewport.evaluate(el=>el.scrollTop),0,'wheel must return to top');
        console.log(`PASS power wheel ${size.width}x${size.height}, CPU ${expanded?'expanded':'collapsed'}: ${JSON.stringify(layout)}`);
      }
    }
    assert.deepEqual(errors,[]);
    assert.equal(await page.evaluate(()=>window.calls.some(n=>/^(set_|apply_|reset_)/.test(n))),false,'layout test must not write hardware');
  } finally {
    if(browser) await browser.close();
    await dev.close();
  }
})().catch(e=>{console.error(e);process.exitCode=1;});
