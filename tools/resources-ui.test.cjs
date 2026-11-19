// Production React views and styles, with deterministic native IPC fixtures.
const assert = require('node:assert/strict');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');

(async () => {
  const root = path.resolve(__dirname, '..');
  const { createServer } = await import(pathToFileURL(path.join(root, 'node_modules/vite/dist/node/index.js')));
  const dev = await createServer({ root, optimizeDeps: { include: ['react', 'react-dom/client'] },
    server: { host: '127.0.0.1', port: 1441, strictPort: true, hmr: false } });
  let browser;
  try {
    await dev.listen();
    const html = await dev.transformIndexHtml('/resource-fixture.html', `<!doctype html><html data-theme="mono"><head></head>
<body><div id="root"></div><script type="module">
import React from '/node_modules/.vite/deps/react.js';
import ReactDOM from '/node_modules/.vite/deps/react-dom_client.js';
import '/src/index.css';
import { ResourceOverview } from '/src/components/monitor/ResourceOverview.tsx';
const root = ReactDOM.createRoot(document.getElementById('root'));
root.render(React.createElement(ResourceOverview));
window.detach = () => root.unmount();
</script></body></html>`);
    browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH });
    const page = await browser.newPage({ viewport: { width: 920, height: 720 } });
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    await page.route('**/resource-fixture.html', r => r.fulfill({ contentType: 'text/html', body: html }));
    await page.addInitScript(() => {
      const M = 2 ** 20;
      const at = Date.now();
      let enabled = false, cleared = false, cancel = true;
      const metrics = [15000 * M, 7000 * M, 28000 * M, 22, 7400, 10000, 5, 7, 56000, 2000];
      window.calls = [];
      window.__TAURI_INTERNALS__ = { async invoke(name, args) {
        window.calls.push([name, args]);
        switch (name) {
          case 'get_resource_snapshot': {
            const memory = { installed: 23000 * M, visible: 22000 * M, reserved: 1000 * M, available: 7000 * M,
              used: 15000 * M, standby: 6600 * M, free_zero: 400 * M, modified: 10 * M,
              private_resident_sum: 9900 * M, unattributed_used: 5100 * M, commit: 28000 * M, commit_limit: 41000 * M,
              private_readable: 2, private_seen: 2 };
            const processes = [{ key: 'proc:101:1', name: 'Browser.exe', pid: 101, private_resident: 7000*M, private_commit: 9000*M, cpu_percent: 0 },
              { key: 'proc:102:1', name: 'Worker.exe', pid: 102, private_resident: 2900*M, private_commit: 9900*M, cpu_percent: null }];
            const disks = [{ key: 'disk:0', name: '0 C: D:', read: 7400, write: 10000, read_iops: 5, write_iops: 7 }];
            const interfaces = [{ key: 'net:0', name: 'Wi-Fi', physical: true, up: true, rx: 56000, tx: 2000 },
              { key: 'net:1', name: 'WSL adapter', physical: false, up: true, rx: 13000, tx: 12000 }];
            const recent = [10000, 8000, 2000, 0].map((delta, i) => ({
              time_ms: at-delta, summary: i === 1 ? [metrics[0], null, metrics[2], null, ...metrics.slice(4)] : metrics,
              gap_before: i === 2, selected: args.selected ? [30000, 7000 * M, 3, 6] : [],
            }));
            return { snapshot: { memory, processes, disks, interfaces, time_ms: at, sampled_in_ms: 9, summary: metrics,
              process_source: 'PSAPI', logical_cpus: 20, disk_detail_available: true, net_available: true },
              history: { enabled, error: null }, recent, mode: 'foreground' };
          }
          case 'get_resource_history':
            return enabled && !cleared ? [{ time_ms: Math.floor(at / 60000) * 60000 - 180000,
              metrics: metrics.map(v => ({ average: v, peak: v + 1, covered_ms: 20000 })) }] : [];
          case 'set_resource_history': enabled = args.enabled; return { enabled, error: null };
          case 'export_resource_history_csv':
            if (cancel) { cancel = false; return null; }
            return 'D:\\summary.csv';
          case 'clear_resource_history': cleared = true; return { enabled, error: null };
          default: return null;
        }
      } };
    });
    await page.goto('http://127.0.0.1:1441/resource-fixture.html');
    await page.getByRole('region', { name: '物理内存对账' }).locator('.resource-legend').getByText('已用差额', { exact: false }).waitFor();
    assert.equal(await page.locator('.resource-memory-bar rect').count(), 5);
    const physical = page.getByRole('region', { name: '物理内存走势' });
    const timeline = physical.getByRole('slider');
    const lastTime = await timeline.getAttribute('aria-valuetext');
    await timeline.press('ArrowLeft');
    assert.notEqual(await timeline.getAttribute('aria-valuetext'), lastTime);
    const legends = await physical.locator('path').evaluateAll(paths => paths.map(p => p.getAttribute('d').match(/M/g).length));
    assert.deepEqual(legends, [2, 2]);
    const ranking = page.getByRole('region', { name:'进程用量排名' });
    assert.match(await ranking.locator('tbody tr').first().innerText(), /Browser\.exe/);
    await ranking.getByRole('button', { name: '私有提交' }).click();
    assert.match(await ranking.locator('tbody tr').first().innerText(), /Worker\.exe/);
    await ranking.getByRole('searchbox').fill('101');
    assert.equal(await ranking.locator('tbody tr').count(), 1);
    await ranking.getByRole('button', { name: 'Browser.exe' }).click();
    await page.getByRole('region', { name: '所选进程 · 私有驻留' }).getByText('6.84 GiB').waitFor();
    await page.getByRole('button', { name: 'CPU', exact: true }).click();
    await page.getByRole('region', { name: '系统 CPU 趋势' }).waitFor();
    assert.match(await ranking.locator('tbody tr').nth(1).innerText(), /未取得/);
    await page.getByRole('button', { name: '磁盘', exact: true }).click();
    await page.getByRole('button', { name: '0 C: D:' }).click();
    await page.getByRole('region', { name: '所选磁盘 · IOPS' }).waitFor();
    await page.getByRole('button', { name: '网络', exact: true }).click();
    await page.getByRole('button', { name: 'WSL adapter' }).click();
    await page.getByRole('region', { name: '所选接口 · 吞吐' }).waitFor();
    await page.getByRole('button', { name: '本机历史', exact: true }).click();
    const preference = page.getByRole('switch', { name: '保存七天资源汇总' });
    assert.equal(await preference.getAttribute('aria-checked'), 'false');
    await preference.click();
    await page.getByRole('region', { name: '接收 · 历史汇总' }).waitFor();
    await page.getByLabel('查看时段').selectOption('168');
    const exportAction = page.getByRole('button', { name: '导出汇总 CSV' });
    await exportAction.click();
    await page.getByText('已取消导出', { exact: true }).waitFor();
    await exportAction.click();
    await page.getByText('已保存：D:\\summary.csv', { exact: true }).waitFor();
    await page.getByRole('button', { name: '清除汇总', exact: true }).click();
    await page.getByRole('button', { name: '确认清除', exact: true }).click();
    await page.getByText('已清除已保存的资源汇总', { exact: true }).waitFor();
    await page.setViewportSize({ width: 390, height: 640 });
    await page.getByRole('button', { name: '近 10 分钟', exact: true }).click();
    await page.getByRole('button', { name: '内存', exact: true }).click();
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1), true);
    await page.evaluate(() => window.detach());
    assert.equal(await page.evaluate(() => window.calls.some(([name]) => name === 'release_resource_view')), true);
    assert.deepEqual(errors, []);
    console.log('PASS resource chart gaps/keyboard, memory reconciliation, sorting/filter/selection, disk/network tabs, history controls/CSV and narrow layout');
  } finally {
    if (browser) await browser.close();
    await dev.close();
  }
})().catch(e => { console.error(e); process.exitCode = 1; });
