// Real React entry point with deterministic Tauri IPC responses.
const assert = require('node:assert/strict');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');

(async () => {
  const root = path.resolve(__dirname, '..');
  const { createServer } = await import(pathToFileURL(path.join(root, 'node_modules/vite/dist/node/index.js')));
  const server = await createServer({ root, server: { host: '127.0.0.1', port: 1442, strictPort: true, hmr: false } });
  let browser;
  try {
    await server.listen();
    browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH });
    const p = await browser.newPage({ viewport: { width: 1050, height: 760 } });
    const errors = [];
    p.on('pageerror', e => errors.push(e.message));
    await p.addInitScript(() => {
      const time = Date.now(), receipts = [];
      const drive = { root: 'D:\\', total: 128 * 2 ** 30, free: 40 * 2 ** 30 };
      const row = { id: 17, path: 'D:\\scratch\\Fixture.tmp', category: 'fixture', logical: 2048000,
        allocated: 2097152, count: 1, group: null };
      let disabled = false, polls = 0, cancel = false, pending;
      window.calls = [];
      const status = (kind, state) => ({ id: kind === 'cleanup' ? 45 : 43, kind, state,
        scope: kind === 'cleanup' ? '当前用户' : 'D:\\scratch', volume: drive, total: [1, 1, 2, 1], notes: [],
        progress: { files: 82, skipped: 1, logical: 2048000, allocated: 2097152 },
        stats: { logical: 2048000, allocated: 2097152, aliases: 2, unknown_allocation: 1 },
      });
      window.__TAURI_INTERNALS__ = { invoke: async (cmd, args) => {
        window.calls.push([cmd, args]);
        switch (cmd) {
          case 'manager_health': return { time_ms: time, memory: { visible: 32 * 2 ** 30, available: 7 * 2 ** 30, load: 78 },
            antivirus: 'good', firewall: 'unavailable', volumes: [drive] };
          case 'manager_inventory':
            if (args.kind === 'health') return { updates: [{ time_ms: time, title: 'Fixture Update', result: 2 }], warnings: [] };
            if (args.kind === 'events') return { rows: [{ time_ms: time, kind: 'display', source: 'Display', event_id: 4101, record: 314, detail: 'Fixture driver' }], warnings: [] };
            if (args.kind === 'startup') return { rows: [{ name: 'Fixture Service', source: 'service', detail: 'FixtureSvc', state: 'Running' }], warnings: [] };
            return { rows: [{ name: 'Fixture Game', publisher: 'Example', version: '1.8', kind: 'desktop', scope: 'user' }], warnings: [] };
          case 'manager_startup': return { rows: [{ name: 'Fixture Startup', id: 'id', source: 'user-run', detail: 'Demo',
            state: disabled ? 'backup' : 'registered', action: disabled ? 'restore' : 'disable' }], warnings: [] };
          case 'manager_peaks': return [{ time_ms: time - 1, metric: 'cpu', value: 53 }];
          case 'get_resource_history': return [];
          case 'manager_scan': polls = 0; cancel = false; return status(args.target.kind === 'cleanup' ? 'cleanup' : 'space', 'running');
          case 'manager_scan_cancel': cancel = true; return;
          case 'manager_scan_status': return args.id === 45 ? status('cleanup', 'complete') : status('space', cancel ? 'canceled' : ++polls >= 2 ? 'partial' : 'running');
          case 'manager_scan_page': return args.section === 'duplicates' ?
            [{ ...row, group: 1 }, { ...row, id: 18, path: 'D:\\scratch\\Duplicate.tmp', group: 1 }] : [row];
          case 'manager_preview':
            pending = args.action;
            return { id: 77, kind: pending.kind,
              caption: pending.kind === 'cleanup' ? '移入 Windows 回收站' : pending.kind === 'close' ? '请求应用正常关闭' : '启动项预览',
              items: [pending.kind === 'startup' ? 'Fixture Startup' : 'Fixture.tmp'], selected_bytes: null, recovery: 'Fixture recovery path' };
          case 'manager_apply': {
            if (pending.kind === 'startup') disabled = !pending.restore;
            const r = { id: receipts.length, time_ms: time, kind: pending.kind, canceled: false,
              rows: [{ item: 'Fixture', status: 'done', detail: 'Fixture readback' }], volumes: [] };
            receipts.push(r); return r;
          }
          case 'manager_receipts': return receipts;
          case 'manager_shell':
          case 'manager_reveal':
          case 'release_resource_view': return;
          case 'get_resource_snapshot':
            return { history: { enabled: false, error: null }, recent: [], mode: 'foreground',
              snapshot: { time_ms: time, summary: Array(10).fill(null), memory: {}, processes: [
                { key: 'proc:10:123', pid: 10, name: 'Fixture.exe', group: 'same-bin', private_resident: 1024, private_commit: 2048, cpu_percent: 14 },
                { key: 'proc:12:456', pid: 12, name: 'Fixture.exe', group: 'same-bin', private_resident: null, private_commit: 2048, cpu_percent: null },
              ], disks: [], interfaces: [], process_source: 'Windows fixture', logical_cpus: 8, sampled_in_ms: 0 } };
          default: throw new Error('Unexpected command: ' + cmd);
        }
      }};
    });
    await p.goto('http://127.0.0.1:1442/manager.html');
    await p.getByText('Fixture Update').waitFor();
    assert.equal(await p.evaluate(() => window.calls.some(([c]) => c === 'manager_scan')), false);
    const b = (name, parent = p) => parent.getByRole('button', { name, exact: typeof name === 'string' });
    const tab = name => b(name, p.getByRole('navigation', { name: '管理中心导航' })).click();
    await tab('资源去向');
    await p.getByText(/≥ 1(?:\.0)? KiB/).waitFor();
    await b(/Fixture.exe（2 个进程）/).click();
    await b('预览关闭 Fixture.exe 10').click();
    const close = p.getByRole('dialog', { name: '请求应用正常关闭' });
    await b('取消', close).click();
    assert.equal(await p.evaluate(() => window.calls.filter(([c]) => c === 'manager_apply').length), 0);
    await tab('存储与清理');
    await b('选择目录并扫描').click();
    await b('取消扫描').click();
    await p.getByText(/已取消；以下不作为完整统计/).waitFor();
    await b('选择目录并扫描').click();
    await p.getByRole('img', { name: '文件夹占盘比例图' }).waitFor();
    await b('重复文件').click();
    await p.getByText('Duplicate.tmp', { exact: false }).first().waitFor();
    await b('扫描可回收项目').click();
    await p.getByRole('checkbox', { name: '选择 Fixture.tmp' }).check();
    await b('预览回收').click();
    await p.getByRole('dialog', { name: '移入 Windows 回收站' }).waitFor();
    await b('确认并执行').click();
    await p.getByText('Fixture readback', { exact: true }).waitFor();
    await tab('启动项');
    await b('预览禁用').click();
    await b('确认并执行').click();
    await b('预览恢复').click();
    await b('确认并执行').click();
    await b('预览禁用').waitFor();
    await p.getByLabel('启动项范围').selectOption('system');
    await p.getByText('Fixture Service', { exact: false }).waitFor();
    assert.equal(await b('预览禁用').count(), 0);
    await tab('软件管理');
    await p.getByText('Fixture Game').waitFor();
    await p.getByRole('searchbox', { name: '搜索软件' }).fill('not-found');
    await p.getByText('没有匹配的可见条目。').waitFor();
    await b('打开 Windows 卸载界面').click();
    await tab('故障时间线');
    await b(/显示驱动重置/).click();
    await p.getByText('事件前后两分钟的实测分钟峰值').waitFor();
    await tab('操作记录');
    await p.locator('.manager-receipt').first().waitFor();
    assert.equal(await p.locator('.manager-receipt').count(), 3);
    await p.setViewportSize({ width: 430, height: 640 });
    assert.equal(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true);
    assert.deepEqual(errors, []);
    console.log('PASS management center: health, grouped telemetry, scan cancel/duplicates, confirmed cleanup, startup restore, inventory, timeline, receipts, narrow layout');
  } finally {
    if (browser) await browser.close();
    await server.close();
  }
})().catch(e => { console.error(e); process.exitCode = 1; });
