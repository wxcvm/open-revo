// Real production panels in a real Chromium; every hardware IPC call is mocked.
// Covers the Windows CPU policy panel (AC/DC fields, expected plan GUID, failed
// apply readback) and the AMD driver graphics panel (true/false/null readback,
// unsupported rows, FRTC gating, truthful reports, restore report).
const assert = require('node:assert/strict');
const { pathToFileURL } = require('node:url');
const path = require('node:path');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');

const PORT = 1432;
const PLAN = '11111111-2222-3333-4444-555555555555';
const AC = 'section.cpu-policy-side[aria-label="已接通电源 (AC)"]';
const DC = 'section.cpu-policy-side[aria-label="使用电池 (DC)"]';
const RESTORE_SECTION = 'section[aria-label="恢复本次会话改动"]';

/** Drive a controlled range input through React's onChange without user drag. */
const setRange = (page, selector, value) =>
  page.locator(selector).evaluate((el, v) => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    setter.call(el, String(v));
    el.dispatchEvent(new Event('input', { bubbles: true }));
  }, value);

/** Scope to one AMD driver row by its production label. */
const gpuRow = (page, label) => page.locator('.quick-row', { hasText: label });

(async () => {
  const root = path.resolve(__dirname, '..');
  const { createServer } = await import(pathToFileURL(path.join(root, 'node_modules/vite/dist/node/index.js')));
  const dev = await createServer({ root, optimizeDeps: { include: ['react', 'react-dom/client'] },
    server: { host: '127.0.0.1', port: PORT, strictPort: true, hmr: false } });
  let browser;
  try {
    await dev.listen();
    const html = await dev.transformIndexHtml('/fixture.html', `<!doctype html><html><body><div id="root"></div>
<script type="module">
import React from '/node_modules/.vite/deps/react.js';
import ReactDOM from '/node_modules/.vite/deps/react-dom_client.js';
import { CpuPolicyPanel } from '/src/components/tuning/CpuPolicyPanel.tsx';
import { NativeChargePanel } from '/src/components/tuning/NativeChargePanel.tsx';
import AmdDriverGraphicsPanel from '/src/components/gpu/AmdDriverGraphicsPanel.tsx';
const root = ReactDOM.createRoot(document.getElementById('root'));
const gpuPanel = () => React.createElement(AmdDriverGraphicsPanel);
window.mountCharge = () => root.render(React.createElement(NativeChargePanel));
window.mountCpu = () => root.render(React.createElement(CpuPolicyPanel));
window.mountGpu = strict => root.render(strict
  ? React.createElement(React.StrictMode, null, gpuPanel())
  : gpuPanel());
window.unmount = () => root.render(null);
</script></body></html>`);
    browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH });
    const page = await browser.newPage();
    page.setDefaultTimeout(20000);
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.route('**/fixture.html', route => route.fulfill({ contentType: 'text/html', body: html }));
    await page.addInitScript(() => {
      const f = window.fixture = {
        charge: { matched:true, available:true, reason:null, enabled:0, threshold1:0, threshold2:0 }, chargeCalls:[], chargeFail:false, calls: [], cpuLoads: 0, applyCalls: [], failApply: null,
        cpu: {
          plan_guid: '11111111-2222-3333-4444-555555555555',
          ac: { max_processor_state: 100, boost_mode: 1, max_mhz: 0 },
          dc: { max_processor_state: 60, boost_mode: 3, max_mhz: 2500 },
        },
        gpuLoads: 0, gpuLoadPlan: null, gpuReleases: [],
        toggleCalls: [], toggleReleases: [], deferToggle: false, rejectToggle: false,
        rejectMessage: '驱动读回仍为开启，未接受本次写入', restoreSkipped: false,
        gpu: {
          available: true, detail: null, adlx_version: '1.4.2', gpu_name: 'AMD Radeon 890M',
          anti_lag: { supported: true, enabled: true, read_error: null },
          chill: { supported: true, enabled: false, min_fps: 30, max_fps: 60,
            range: { min: 30, max: 300, step: 1 }, read_error: null },
          frame_rate_target: { supported: true, enabled: false, fps: 60,
            range: { min: 30, max: 300, step: 5 }, read_error: null },
          image_sharpening: { supported: false, enabled: null, read_error: null },
          enhanced_sync: { supported: true, enabled: null, read_error: 'ADLX 未返回 Enhanced Sync 状态' },
          tuning: { manual_gfx: true, manual_vram: false, manual_fan: null,
            manual_power: null, auto_tuning: true, preset_tuning: false },
          session: { changed: [], can_restore: false },
          warnings: ['驱动报告：持续风扇控制未就绪'],
        },
      };
      f.staleGpu = Object.assign(structuredClone(f.gpu), {
        adlx_version: '0.0.1-stale', gpu_name: 'STALE FIXTURE GPU',
        anti_lag: { supported: true, enabled: false, read_error: null },
      });
      f.releaseGpuLoad = () => { const next = f.gpuReleases.shift(); if (next) next(); };
      f.releaseToggle = () => { const next = f.toggleReleases.shift(); if (next) next(); };
      window.__TAURI_INTERNALS__ = {
        transformCallback() { return 1; },
        async invoke(name, args) {
          const f = window.fixture;
          f.calls.push(name);
          if (name === 'get_native_charge_state') return structuredClone(f.charge);
          if (name === 'apply_native_charge_policy') {
            f.chargeCalls.push(structuredClone(args));
            if (f.chargeFail) return {applied:false,failed:'echo rejected',rollback:'restored',state:{enabled:0,threshold1:0,threshold2:0}};
            Object.assign(f.charge, args.enabled ? {enabled:1,threshold1:args.threshold,threshold2:100} : {enabled:0,threshold1:0,threshold2:0});
            return {applied:true,failed:null,rollback:null,state:{enabled:f.charge.enabled,threshold1:f.charge.threshold1,threshold2:f.charge.threshold2}};
          }
          if (name === 'get_cpu_policy') { f.cpuLoads++; return structuredClone(f.cpu); }
          if (name === 'apply_cpu_policy') {
            f.applyCalls.push(structuredClone(args || {}));
            if (f.failApply) throw new Error(f.failApply);
            f.cpu.ac = structuredClone(args.request.ac);
            f.cpu.dc = structuredClone(args.request.dc);
            return { plan_guid: f.cpu.plan_guid, applied: ['ac.max_processor_state', 'dc.boost_mode'],
              unchanged: ['ac.boost_mode', 'dc.max_processor_state'], failed: [], restored: [],
              rollback_failed: [], relinquished: [], unverified: [], activated: true,
              activation_error: null, reactivated: true, overlay_before: null, overlay_after: null,
              overlay_preserved: true, overlay_error: null, reverted: false };
          }
          if (name === 'get_gpu_driver_graphics_state') {
            f.gpuLoads++;
            const step = f.gpuLoadPlan && f.gpuLoadPlan.shift();
            if (step && step.defer) {
              await new Promise(resolve => f.gpuReleases.push(resolve));
              return structuredClone(step.payload || f.gpu);
            }
            if (step && step.payload) f.gpu = structuredClone(step.payload);
            return structuredClone(f.gpu);
          }
          if (name === 'set_gpu_driver_toggle') {
            f.toggleCalls.push(structuredClone(args || {}));
            if (f.deferToggle) await new Promise(resolve => f.toggleReleases.push(resolve));
            if (f.rejectToggle) {
              return { applied: false, message: f.rejectMessage, state: structuredClone(f.gpu) };
            }
            f.gpu[args.feature] = Object.assign(structuredClone(f.gpu[args.feature]), { enabled: args.enabled });
            f.gpu.session = { changed: ['anti_lag'], can_restore: true };
            return { applied: true,
              message: `已回读确认：${args.feature} ${args.enabled ? '已开启' : '已关闭'}`,
              state: structuredClone(f.gpu) };
          }
          if (name === 'restore_gpu_driver_session') {
            const restored = f.restoreSkipped ? [] : ['anti_lag'];
            const skipped = f.restoreSkipped ? [{ field: 'anti_lag', reason: '驱动拒绝恢复' }] : [];
            f.gpu.session = { changed: ['anti_lag'], can_restore: true };
            return { restored, skipped, message: null, state: structuredClone(f.gpu) };
          }
          return null;
        },
      };
      window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    });
    await page.goto(`http://127.0.0.1:${PORT}/fixture.html`);

    /* ---------------------------- CPU policy panel ---------------------------- */
    assert.equal(await page.evaluate(() => window.fixture.calls.length), 0);
    await page.evaluate(() => window.mountCpu());
    await page.locator('.cpu-policy-summary').waitFor();
    assert.equal(await page.locator('.cpu-policy-plan code').textContent(), PLAN);
    assert.equal(await page.evaluate(() => window.fixture.calls.filter(x => x === 'get_cpu_policy').length), 1);
    const summary = await page.locator('.cpu-policy-summary').innerText();
    assert.ok(summary.includes('AC 100%') && summary.includes('boost 1'), summary);
    assert.ok(summary.includes('DC 60%') && summary.includes('boost 3'), summary);
    assert.ok(summary.includes('不限 MHz 策略') && summary.includes('2500 MHz 策略'), summary);
    console.log('PASS cpu panel: summary shows the Windows readback of both sides and the active plan GUID');

    await page.locator('details.cpu-policy-card > summary').click();
    await page.locator(`${AC} input[type=range]`).waitFor();
    assert.equal(await page.locator('.cpu-policy-plan code').isVisible(), true);
    assert.equal(await page.locator(`${AC} input[type=range]`).inputValue(), '100');
    assert.equal(await page.locator(`${DC} input[type=range]`).inputValue(), '60');
    assert.equal(await page.locator(`${AC} select`).inputValue(), '1');
    assert.equal(await page.locator(`${DC} select`).inputValue(), '3');
    assert.equal(await page.locator(`${AC} select option`).count(), 7);
    assert.equal(await page.locator(`${AC} input[type=number]`).inputValue(), '0');
    assert.equal(await page.locator(`${DC} input[type=number]`).inputValue(), '2500');
    await setRange(page, `${AC} input[type=range]`, 80);
    assert.equal(await page.locator(`${DC} input[type=range]`).inputValue(), '60');
    await page.locator(`${DC} select`).selectOption('5');
    assert.equal(await page.locator(`${AC} select`).inputValue(), '1');
    const acDesc = await page.locator(`${AC} .cpu-policy-field-desc`).first().innerText();
    assert.ok(acDesc.includes('会话快照 100%'), `draft must not rewrite the snapshot label: ${acDesc}`);
    console.log('PASS cpu AC/DC fields: independent drafts per side, snapshot baseline kept, no optimistic success');

    await page.getByRole('button', { name: '应用 Windows CPU 策略' }).click();
    const success = page.locator('.cpu-policy-result');
    await success.filter({ hasText: '已接受同一计划的重新应用' }).waitFor();
    assert.ok((await success.innerText()).includes('ac.max_processor_state'), await success.innerText());
    const applied = await page.evaluate(() => window.fixture.applyCalls.at(-1));
    assert.equal(applied.expectedPlanGuid, PLAN);
    assert.deepEqual(applied.request, {
      ac: { max_processor_state: 80, boost_mode: 1, max_mhz: 0 },
      dc: { max_processor_state: 60, boost_mode: 5, max_mhz: 2500 },
    });
    assert.equal(await page.evaluate(() => window.fixture.cpuLoads), 2);
    assert.equal(await page.locator(`${AC} input[type=range]`).inputValue(), '80');
    assert.equal(await page.locator(`${DC} select`).inputValue(), '5');
    console.log('PASS cpu apply: expectedPlanGuid IPC argument matches the active plan; response is re-queried, not trusted');

    await page.evaluate(() => {
      window.fixture.cpu.ac.max_processor_state = 45;
      window.fixture.failApply = 'CPU policy write rejected';
    });
    await setRange(page, `${AC} input[type=range]`, 90);
    await page.getByRole('button', { name: '应用 Windows CPU 策略' }).click();
    await page.locator('.cpu-policy-error[role=alert]').filter({ hasText: 'CPU policy write rejected' }).waitFor();
    await page.waitForFunction(() =>
      document.querySelector('section.cpu-policy-side input[type=range]').value === '45');
    const failedText = await page.locator('.cpu-policy-card').innerText();
    assert.equal(await page.locator('.cpu-policy-result').count(), 0);
    assert.equal(failedText.includes('已接受同一计划的重新应用'), false);
    assert.equal(failedText.includes('已回读确认'), false);
    assert.equal(await page.evaluate(() => window.fixture.applyCalls.length), 2);
    assert.equal(await page.evaluate(() => window.fixture.applyCalls.at(-1).expectedPlanGuid), PLAN);
    assert.equal(await page.evaluate(() => window.fixture.cpuLoads), 3);
    console.log('PASS cpu failed apply: error surfaced, Windows snapshot refreshed (draft 90 -> 45), no success state');

    await page.evaluate(() => { window.fixture.failApply = null; window.unmount(); });
    await page.waitForFunction(() => !document.querySelector('.cpu-policy-card'));

    /* ------------------- GPU panel: StrictMode stale first load ------------------- */
    await page.evaluate(() => {
      window.fixture.gpuLoadPlan = [{ defer: true, payload: window.fixture.staleGpu }, {}];
      window.mountGpu(true);
    });
    await page.waitForFunction(() => document.querySelector('.amdgpu-meta-line')?.textContent.includes('1.4.2'));
    assert.equal(await page.locator('.amdgpu-meta-line').nth(1).innerText(), 'AMD Radeon 890M');
    assert.equal(await gpuRow(page, 'Anti-Lag').locator('.amdgpu-chip').innerText(), '已开启');
    const loadsAfterStrict = await page.evaluate(() => window.fixture.gpuLoads);
    assert.ok(loadsAfterStrict >= 2, `StrictMode must issue the remount readback: ${loadsAfterStrict}`);
    await page.evaluate(() => window.fixture.releaseGpuLoad());
    await page.waitForTimeout(700);
    const strictText = await page.locator('.amdgpu-root').innerText();
    assert.equal(strictText.includes('STALE FIXTURE GPU'), false, 'stale first readback must never overwrite newer state');
    assert.equal(strictText.includes('0.0.1-stale'), false);
    assert.equal(await gpuRow(page, 'Anti-Lag').locator('.amdgpu-chip').innerText(), '已开启');
    assert.deepEqual(errors, []);
    console.log('PASS gpu StrictMode: a late first readback is discarded, the newer snapshot stays on screen');

    /* ----------------------------- GPU panel: readback ----------------------------- */
    await page.evaluate(() => { window.fixture.gpuLoadPlan = null; window.unmount(); });
    await page.waitForFunction(() => !document.querySelector('.amdgpu-root'));
    await page.evaluate(() => window.mountGpu(false));
    await gpuRow(page, 'Anti-Lag').locator('.amdgpu-chip').waitFor();

    const antiRow = gpuRow(page, 'Anti-Lag');
    const chillRow = gpuRow(page, 'Radeon Chill');
    const sharpRow = gpuRow(page, 'Image Sharpening');
    const frtcRow = gpuRow(page, 'Frame Rate Target Control');
    const syncRow = gpuRow(page, 'Enhanced Sync');

    assert.equal(await antiRow.locator('.amdgpu-chip').innerText(), '已开启');
    assert.equal(await antiRow.locator('button').innerText(), '关闭');
    assert.equal(await antiRow.locator('button').getAttribute('aria-pressed'), 'true');
    assert.equal(await chillRow.locator('.amdgpu-chip').innerText(), '已关闭');
    assert.equal(await chillRow.locator('button').innerText(), '开启');
    assert.equal(await sharpRow.locator('.amdgpu-chip').innerText(), '不支持');
    assert.equal(await sharpRow.locator('button').count(), 0);
    assert.ok((await sharpRow.innerText()).includes('驱动上报该功能不受支持，不提供开关。'));
    assert.ok(await sharpRow.evaluate(el => el.classList.contains('is-disabled')));
    assert.equal(await syncRow.locator('.amdgpu-chip').innerText(), '未知');
    assert.equal(await syncRow.locator('button').count(), 0);
    assert.ok((await syncRow.innerText()).includes('ADLX 未返回 Enhanced Sync 状态'));
    assert.equal(await page.getByText('驱动报告：持续风扇控制未就绪', { exact: true }).count(), 1);
    assert.equal(await page.getByLabel('Radeon Chill 最小帧率').inputValue(), '30');
    assert.equal(await page.getByLabel('Radeon Chill 最大帧率').inputValue(), '60');
    assert.equal(await page.getByLabel('Radeon Chill 最小帧率').isEnabled(), true);
    console.log('PASS gpu readback: true/false/null chips differ, unsupported rows carry no toggle and no guessed value');

    await page.locator('details[aria-label="调校支持度（驱动证据）"] > summary').click();
    assert.equal(await page.locator('.amdgpu-evidence-item', { hasText: '手动核心频率' }).locator('.amdgpu-chip').innerText(), '支持');
    assert.equal(await page.locator('.amdgpu-evidence-item', { hasText: '手动显存频率' }).locator('.amdgpu-chip').innerText(), '不支持');
    assert.equal(await page.locator('.amdgpu-evidence-item', { hasText: '手动风扇转速' }).locator('.amdgpu-chip').innerText(), '未知');
    console.log('PASS gpu evidence: driver tuning support keeps true/false/null distinct (unknown is not unsupported)');

    /* --------------------------- GPU panel: FRTC gating --------------------------- */
    const frtcInput = page.getByLabel('目标帧率上限');
    assert.equal(await frtcInput.inputValue(), '60');
    assert.equal(await frtcInput.isDisabled(), true);
    assert.equal(await frtcRow.locator('button').innerText(), '开启');
    await frtcRow.locator('button').click();
    await page.waitForFunction(() => !document.querySelector('[aria-label="目标帧率上限"]').disabled);
    assert.deepEqual(await page.evaluate(() => window.fixture.toggleCalls.at(-1)),
      { feature: 'frame_rate_target', enabled: true });
    assert.equal(await frtcRow.locator('.amdgpu-chip').innerText(), '已开启');
    const appliedMsg = page.locator('.amdgpu-message[role=status]');
    await appliedMsg.filter({ hasText: '已回读确认：frame_rate_target 已开启' }).waitFor();
    console.log('PASS gpu FRTC: the FPS input stays disabled until the driver confirms the limit is enabled');

    /* -------------------- GPU panel: rejected report stays truthful -------------------- */
    await page.evaluate(() => { window.fixture.rejectToggle = true; });
    await frtcRow.locator('button').click();
    const rejection = page.locator('.amdgpu-message.is-error[role=alert]');
    await rejection.filter({ hasText: '驱动读回仍为开启，未接受本次写入' }).waitFor();
    assert.equal(await frtcRow.locator('.amdgpu-chip').innerText(), '已开启');
    assert.equal(await frtcRow.locator('button').innerText(), '关闭');
    assert.equal(await frtcInput.isEnabled(), true);
    assert.equal(await page.getByText('已回读确认：frame_rate_target 已关闭').count(), 0);
    assert.equal(await page.locator('.amdgpu-message[role=status]').count(), 0);
    assert.deepEqual(await page.evaluate(() => window.fixture.toggleCalls.at(-1)),
      { feature: 'frame_rate_target', enabled: false });
    console.log('PASS gpu rejected toggle: requested state is not displayed, the driver readback and error are adopted');

    /* --------------------- GPU panel: rapid duplicate toggle click --------------------- */
    await page.evaluate(() => {
      window.fixture.rejectToggle = false;
      window.fixture.deferToggle = true;
    });
    const togglesBefore = await page.evaluate(() => window.fixture.toggleCalls.length);
    await page.evaluate(() => {
      const row = [...document.querySelectorAll('.quick-row')].find(el => el.textContent.includes('Anti-Lag'));
      const button = row.querySelector('button');
      button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await page.waitForTimeout(200);
    assert.equal(await page.evaluate(() => window.fixture.toggleCalls.length), togglesBefore + 1);
    await page.evaluate(() => window.fixture.releaseToggle());
    await page.waitForFunction(() => window.fixture.gpu.anti_lag.enabled === false);
    await page.waitForFunction(() => {
      const row = [...document.querySelectorAll('.quick-row')].find(el => el.textContent.includes('Anti-Lag'));
      return row.textContent.includes('已关闭');
    });
    assert.equal(await page.evaluate(() => window.fixture.toggleCalls.length), togglesBefore + 1);
    assert.equal(await page.evaluate(() => window.fixture.toggleCalls.at(-1).feature), 'anti_lag');
    await page.evaluate(() => { window.fixture.deferToggle = false; });
    console.log('PASS gpu duplicate click: a second click while the write is in flight invokes the driver once');

    /* ----------------------------- GPU panel: restore report ----------------------------- */
    const restoreSection = page.locator(RESTORE_SECTION);
    const restoreButton = page.getByRole('button', { name: '恢复会话改动' });
    assert.equal(await restoreButton.isEnabled(), true);
    await restoreButton.click();
    await restoreSection.getByText('已回读确认恢复：anti_lag', { exact: false }).waitFor();
    assert.equal(await restoreSection.locator('.amdgpu-skip-list').count(), 0);
    await page.evaluate(() => { window.fixture.restoreSkipped = true; });
    await restoreButton.click();
    const skipList = restoreSection.locator('.amdgpu-skip-list li');
    await skipList.filter({ hasText: '驱动拒绝恢复' }).waitFor();
    assert.equal(await skipList.count(), 1);
    assert.equal(await restoreSection.getByText('已回读确认恢复', { exact: false }).count(), 0);
    assert.ok((await skipList.innerText()).includes('anti_lag'));
    console.log('PASS gpu restore: confirmed fields are listed, skipped fields show the driver reason, no invented success');

    /* ------------------- GPU panel: null readback offers no usable toggle ------------------- */
    await page.evaluate(() => {
      window.fixture.gpu.chill.enabled = null;
      window.fixture.gpu.chill.min_fps = null;
      window.fixture.gpu.chill.max_fps = null;
      window.fixture.gpu.chill.read_error = 'ADLX 读回超时';
      window.fixture.gpu.session = { changed: [], can_restore: false };
      window.unmount();
    });
    await page.waitForFunction(() => !document.querySelector('.amdgpu-root'));
    await page.evaluate(() => window.mountGpu(false));
    await gpuRow(page, 'Radeon Chill').locator('.amdgpu-chip').waitFor();
    assert.equal(await chillRow.locator('.amdgpu-chip').innerText(), '未知');
    assert.equal(await chillRow.locator('button').innerText(), '未知');
    assert.equal(await chillRow.locator('button').isDisabled(), true);
    assert.ok((await chillRow.innerText()).includes('驱动回读未知：ADLX 读回超时'));
    assert.equal(await page.getByLabel('Radeon Chill 最小帧率').inputValue(), '');
    assert.equal(await page.getByRole('button', { name: '恢复会话改动' }).isDisabled(), true);
    await page.evaluate(() => window.mountCharge());
    const charge = page.getByRole('group', {name:'电池充电上限'});
    await charge.waitFor();
    await page.getByRole('button', {name:'工作站 60%'}).click();
    assert.equal(await page.evaluate(() => window.fixture.chargeCalls.length), 0);
    await page.getByRole('button', {name:'启用 60% / 100%'}).click();
    await charge.getByRole('status').filter({hasText:'已写入并独立回读确认'}).waitFor();
    const chargeCall = await page.evaluate(() => window.fixture.chargeCalls[0]);
    assert.deepEqual(chargeCall, {enabled:true,threshold:60,expected:{enabled:0,threshold1:0,threshold2:0}});
    await page.getByRole('button', {name:'关闭上限（满充）'}).click();
    await page.waitForFunction(() => window.fixture.chargeCalls.length === 2);
    await page.waitForFunction(() => document.querySelector('[aria-label="电池充电上限"]').innerText.includes('enabled=0'));
    await page.evaluate(() => { window.fixture.chargeFail = true; });
    await page.getByRole('button', {name:'启用 60% / 100%'}).click();
    await charge.getByRole('status').filter({hasText:'未生效'}).waitFor();
    assert.ok((await charge.innerText()).includes('echo rejected'));
    console.log('PASS charge: preset selection is draft-only, exact snapshot sent, disable resets, failed apply stays truthful');
    await page.evaluate(() => { window.unmount(); window.fixture.charge.matched=false; });
    await charge.waitFor({state:'detached'});
    await page.evaluate(() => window.mountCharge());
    await page.waitForTimeout(200);
    assert.equal(await page.getByRole('group', {name:'电池充电上限'}).count(),0);
    console.log('PASS charge: unsupported machine has no controls');
    assert.deepEqual(errors, []);
    console.log('PASS gpu unknown readback: no on/off claim, toggle unusable, draft empty, restore unavailable');
  } finally {
    await browser?.close();
    await dev.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
