import { useCallback, useEffect, useRef, useState } from 'react';
import { Activity, AppWindow, ClipboardList, Gauge, HardDrive, History, ListStart, ShieldCheck } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { ResourceOverview } from '../monitor/ResourceOverview';
import { bytes, bps, percent } from '../monitor/ResourceChart';
import type { Minute } from '../monitor/ResourceChart';
import { StorageView } from './StorageView';
import { dateTime, manager, textError } from './api';
import type { AwakeState, Action, AppInfo, Health, Peak, Preview, Receipt, Scan, ShellPage, Startup, UpdateInfo, WindowsEvent, WindowsInventory } from './api';

const NAV = [
  { id: 'health', title: '电脑体检', icon: ShieldCheck },
  { id: 'resources', title: '资源去向', icon: Gauge },
  { id: 'storage', title: '存储与清理', icon: HardDrive },
  { id: 'startup', title: '启动项', icon: ListStart },
  { id: 'apps', title: '软件管理', icon: AppWindow },
  { id: 'timeline', title: '故障时间线', icon: Activity },
  { id: 'receipts', title: '操作记录', icon: ClipboardList },
] as const;
type Page = typeof NAV[number]['id'];
type Query<T> = { data?: T; error?: string; loading: boolean };
const unavailable = '未取得';
const securityText: Record<string, string> = {
  good: 'Windows 报告正常', poor: 'Windows 报告需处理', snoozed: '已暂停',
  not_monitored: 'Windows 未监视', unavailable,
};
const sourceText: Record<string, string> = {
  'user-run': '当前用户 · 注册表', 'user-folder': '当前用户 · 启动文件夹',
  'user:RunOnce': '当前用户 · 一次性启动', 'global:Run': '全局 · 注册表', 'global:RunOnce': '全局 · 一次性启动',
  'global32:Run': '全局 · 32 位注册表', 'global32:RunOnce': '全局 · 32 位一次性启动',
  'common-folder': '公共启动文件夹', task: '启动 / 登录计划任务', service: '自动启动服务',
};
const startupState: Record<string, string> = {
  registered: '存在注册记录', conflict: '有同名项目 · 未覆盖', backup: '已保留恢复记录',
  Ready: '就绪', Disabled: '已禁用', Running: '运行中', Stopped: '已停止', Queued: '排队中',
};
const eventText: Record<string, string> = {
  display: '显示驱动重置', bugcheck: '系统错误检查', power: '非正常重启', shutdown: '意外关机',
  application: '应用程序错误', hang: '应用无响应', update: 'Windows 更新安装', update_error: 'Windows 更新错误',
};
const rowStatus: Record<string, string> = {
  done: '已核对', pending: '已请求 · 未完成', skipped: '已跳过', unverified: '失败或未核实',
};
const peakText: Record<Peak['metric'], string> = { memory: '物理内存', cpu: 'CPU', disk: '物理磁盘', network: '物理网络' };
const peakFormat = (p: Peak) => p.metric === 'cpu' ? percent(p.value) : p.metric === 'memory' ? bytes(p.value) : bps(p.value);
const isOneOf = (a: string, b: string[]) => b.includes(a);

function useRead<T>(load: () => Promise<T>, generation = 0): Query<T> {
  const [q, setQ] = useState<Query<T>>({ loading: true });
  useEffect(() => {
    let active = true;
    setQ({ loading: true });
    void load().then(data => { if (active) setQ({ data, loading: false }); },
      error => { if (active) setQ({ error: textError(error), loading: false }); });
    return () => { active = false; };
  }, [load, generation]);
  return q;
}
function Reading({ q, label }: { q: Query<unknown>; label: string }) {
  return q.loading ? <p role="status">正在读取{label}…</p> : q.error ? <p className="manager-error" role="alert">{label}：{q.error}</p> : null;
}
function Warnings({ items = [] }: { items?: string[] }) {
  return items.length ? <details className="manager-warnings"><summary>数据源提示（{items.length}）</summary>
    <ul>{items.map((w, i) => <li key={i}>{w}</li>)}</ul></details> : null;
}

function HealthView({ health, open, navigate }: { health: Query<Health>; open: (p: ShellPage) => void; navigate: (p: Page) => void }) {
  const [reload, setReload] = useState(0);
  const load = useCallback(() => manager.windows<{ updates: UpdateInfo[]; warnings: string[] }>('health'), []);
  const windows = useRead(load, reload);
  const h = health.data, m = h?.memory, update = windows.data?.updates[0];
  const low = (h?.volumes ?? []).filter(v => v.total && v.free != null && v.free / v.total < 0.10);
  return <div className="manager-stack">
    <section className="resource-panel manager-heading">
      <h2>看见资源，查清问题</h2>
      <p>体检只读。读数来自 Windows 提供方；需要改动时另行预览和确认。</p>
      <small>{h ? 'Windows 即时读数 · ' + dateTime(h.time_ms) : '等待 Windows 即时读数'}</small>
      <Reading q={health} label="系统体检" />
    </section>
    <div className="manager-card-grid">
      <section className="resource-panel manager-card">
        <h3><Gauge size={18} /> 内存压力</h3><strong>{m ? m.load + '%' : unavailable}</strong>
        <p>{m ? '可用 ' + bytes(m.available) + ' / Windows 可见 ' + bytes(m.visible) : '来源：Windows 物理内存状态'}</p>
        <p>{m && m.load >= 85 ? '此样本已用偏高；到资源去向核对进程、待机和系统占用。' : '待机缓存已经计入可用量，不是丢失的内存。'}</p>
        <button className="toggle-btn" onClick={() => navigate('resources')}>查看资源去向</button>
      </section>
      <section className="resource-panel manager-card">
        <h3><HardDrive size={18} /> 固定磁盘</h3><strong>{h ? (low.length ? low.length + ' 个卷低于 10% 可用' : h.volumes.length + ' 个卷') : unavailable}</strong>
        <ul className="manager-volumes">{h?.volumes.map(v => <li key={v.root}>
          <span>{v.root}</span><b>{bytes(v.free)} 可用 / {bytes(v.total)}</b>
          {v.total && v.free != null && <progress aria-label={v.root + ' 已用比例'} max={v.total} value={v.total - v.free} />}
        </li>)}</ul>
        <button className="toggle-btn" onClick={() => navigate('storage')}>查看存储与清理</button>
      </section>
      <section className="resource-panel manager-card">
        <h3><ShieldCheck size={18} /> Windows 防护</h3><strong>{securityText[h?.antivirus ?? 'unavailable']}</strong>
        <p>防火墙：{securityText[h?.firewall ?? 'unavailable']} · Windows 安全中心汇总</p>
        <button className="toggle-btn" onClick={() => open('security')}>打开 Windows 安全中心</button>
      </section>
      <section className="resource-panel manager-card">
        <h3><History size={18} /> Windows 更新</h3><strong>{update ? dateTime(update.time_ms) : windows.loading ? '正在读取' : unavailable}</strong>
        <p>{update ? (update.title || '读取到一次安装记录') + (update.source === 'WindowsUpdateClient' ? ' · Windows 系统事件' : ' · Windows 更新历史') : '仅显示本机已安装记录，不拿历史记录推断是否已是最新版。'}</p>
        <Reading q={windows} label="Windows 更新历史" /><Warnings items={windows.data?.warnings} />
        <div className="resource-actions">
          <button className="toggle-btn" onClick={() => open('update')}>打开 Windows 更新</button>
          <button className="toggle-btn" disabled={windows.loading} onClick={() => setReload(n => n + 1)}>重读历史</button>
        </div>
      </section>
    </div>
    <section className="resource-panel">
      <div className="resource-line"><h3>启动与故障诊断</h3><div className="resource-actions">
        <button className="toggle-btn" onClick={() => navigate('startup')}>查看启动项</button>
        <button className="toggle-btn" onClick={() => navigate('timeline')}>打开故障时间线</button></div></div>
      <p>分开查看同登录用户的可操作项目与系统只读项目；出现闪屏、卡顿或重启时按时间查 Windows 事件。</p>
    </section>
  </div>;
}

type InventoryStartup = Pick<Startup, 'name' | 'source' | 'detail' | 'state'>;
function StartupView({ open, preview, generation }: { open: (p: ShellPage) => void; preview: (a: Action) => void; generation: number }) {
  const [scope, setScope] = useState('user');
  const [query, setQuery] = useState('');
  const [limit, setLimit] = useState(60);
  const loadOther = useCallback(() => scope !== 'user' ? manager.windows<WindowsInventory<InventoryStartup>>('startup') : Promise.resolve(undefined), [scope]);
  const user = useRead(manager.startup, generation), other = useRead(loadOther);
  const rows = (scope === 'user' ? user.data?.rows ?? [] : other.data?.rows ?? [] as Startup[]).filter(r =>
    (r.name + ' ' + r.detail + ' ' + r.source).toLowerCase().includes(query.trim().toLowerCase()));
  return <section className="resource-panel">
    <div className="resource-line"><h3><ListStart size={18} /> 启动项</h3><button className="toggle-btn" onClick={() => open('startup')}>Windows 启动应用</button></div>
    <p>注册记录存在不代表实际随系统启动。仅当前用户的注册表 Run 项与启动快捷方式可操作；系统范围和一次性项目保持只读。</p>
    <div className="resource-actions">
      <label>范围 <select value={scope} onChange={e => { setScope(e.target.value); setLimit(60); }} aria-label="启动项范围">
        <option value="user">当前用户</option><option value="system">全局、一次性启动与服务</option></select></label>
      <input type="search" className="resource-search" placeholder="搜索启动项" aria-label="搜索启动项" value={query}
        onChange={e => { setQuery(e.target.value); setLimit(60); }} />
    </div>
    <Reading q={scope === 'user' ? user : other} label="启动项" />
    <Warnings items={scope === 'user' ? user.data?.warnings : other.data?.warnings} />
    {!!rows.length && <div className="resource-table-wrap"><table className="resource-table manager-table">
      <thead><tr><th scope="col">启动项</th><th scope="col">来源</th><th scope="col">状态</th><th scope="col">维护</th></tr></thead>
      <tbody>{rows.slice(0, limit).map((r: Startup, i) => <tr key={r.source + '/' + r.name + '/' + i}>
        <td className="manager-path" title={r.detail}>{r.name}<small>{r.detail}</small></td>
        <td>{sourceText[r.source] || r.source}</td><td>{startupState[r.state] || r.state}</td>
        <td>{r.id && r.action ? <button className="toggle-btn" onClick={() => preview({ kind: 'startup', id: r.id!, restore: r.action === 'restore' })}>
          {r.action === 'restore' ? '预览恢复' : '预览禁用'}</button> : '只读'}</td>
      </tr>)}</tbody>
    </table></div>}
    {rows.length > limit && <button className="toggle-btn" onClick={() => setLimit(l => l + 60)}>显示更多（{rows.length - limit}）</button>}
    {!rows.length && !(scope === 'user' ? user : other).loading && <p>没有匹配的可见条目。</p>}
  </section>;
}

function AppsView({ open }: { open: (p: ShellPage) => void }) {
  const [search, setSearch] = useState(''), [kind, setKind] = useState('all'), [limit, setLimit] = useState(60);
  const load = useCallback(() => manager.windows<WindowsInventory<AppInfo>>('apps'), []);
  const query = useRead(load);
  const rows = (query.data?.rows ?? []).filter(r => (kind === 'all' || r.kind === kind || (kind === 'msix' && r.kind === 'start_msix')) &&
    (r.name + ' ' + r.publisher).toLowerCase().includes(search.trim().toLowerCase()));
  return <section className="resource-panel">
    <div className="resource-line"><h3><AppWindow size={18} /> 软件管理</h3><button className="toggle-btn" onClick={() => open('apps')}>打开 Windows 卸载界面</button></div>
    <p>查看桌面程序和当前用户的 MSIX 应用。卸载在 Windows 中选择同名项目，不以管理员权限执行软件登记的命令。</p>
    <div className="resource-actions">
      <input className="resource-search" type="search" aria-label="搜索软件" placeholder="搜索名称或发布者" value={search}
        onChange={e => { setSearch(e.target.value); setLimit(60); }} />
      <label>类型 <select value={kind} onChange={e => { setKind(e.target.value); setLimit(60); }}>
        <option value="all">全部</option><option value="desktop">桌面应用</option><option value="msix">MSIX 应用</option>
      </select></label><b>{rows.length} 项</b>
    </div>
    <Reading q={query} label="软件清单" /><Warnings items={query.data?.warnings} />
    {!!rows.length && <div className="resource-table-wrap"><table className="resource-table manager-table">
      <thead><tr><th scope="col">应用名称</th><th scope="col">版本</th><th scope="col">来源</th><th scope="col">范围</th></tr></thead>
      <tbody>{rows.slice(0, limit).map((r, i) => <tr key={r.name + '/' + i}>
        <td>{r.name}<small>{r.publisher || '发布者未登记'}</small></td><td>{r.version || '未登记'}</td>
        <td>{r.kind === 'desktop' ? '桌面安装记录' : r.kind === 'start_msix' ? '启动菜单登记的包应用' : 'MSIX'}</td><td>{r.scope === 'user' ? '当前用户' : '整机'}</td>
      </tr>)}</tbody>
    </table></div>}
    {rows.length > limit && <button className="toggle-btn" onClick={() => setLimit(l => l + 60)}>显示更多（{rows.length - limit}）</button>}
    {!query.loading && !rows.length && <p>没有匹配的可见条目。</p>}
  </section>;
}

function TimelineView({ navigate }: { navigate: (p: Page) => void }) {
  const [hours, setHours] = useState(24), [generation, setGeneration] = useState(0), [type, setType] = useState('all');
  const [selected, select] = useState<string>();
  const load = useCallback(async () => {
    const [events, peaks, history] = await Promise.allSettled([
      manager.windows<WindowsInventory<WindowsEvent>>('events', hours),
      manager.peaks(),
      invoke<Minute[]>('get_resource_history', { hours }),
    ]);
    const value = events.status === 'fulfilled' ? events.value : { rows: [], warnings: [textError(events.reason)] };
    return { events: value.rows, peaks: peaks.status === 'fulfilled' ? peaks.value : [],
      history: history.status === 'fulfilled' ? history.value : [], warnings: value.warnings };
  }, [hours]);
  const q = useRead(load, generation);
  const rows = (q.data?.events ?? []).filter(r => type === 'all' || (type === 'incidents' ? isOneOf(r.kind, ['application', 'hang', 'bugcheck', 'display', 'shutdown', 'power']) : r.kind.startsWith('update')));
  const id = (r: WindowsEvent) => r.source + '/' + r.record;
  return <div className="manager-stack">
    <section className="resource-panel">
      <div className="resource-line"><h3><Activity size={18} /> 故障时间线</h3><button className="toggle-btn" onClick={() => navigate('resources')}>进入实时资源采样</button></div>
      <p>Windows 事件和已有采样按时间对齐。相关资源读数不等于根因；无事件记录也不代表没有发生异常。</p>
      <div className="resource-actions">
        <label>时段 <select aria-label="事件时段" value={hours} onChange={e => { setHours(Number(e.target.value)); select(undefined); }}>
          <option value="1">最近 1 小时</option><option value="24">最近 24 小时</option><option value="168">最近 7 天</option></select></label>
        <label>类别 <select aria-label="事件类别" value={type} onChange={e => setType(e.target.value)}>
          <option value="all">所有事件</option><option value="incidents">异常</option><option value="updates">更新</option></select></label>
        <button className="toggle-btn" disabled={q.loading} onClick={() => setGeneration(n => n + 1)}>刷新事件与采样</button>
      </div>
      <Reading q={q} label="Windows 事件" /><Warnings items={q.data?.warnings} />
      {!rows.length && !q.loading && <p>所选范围内没有读到这类事件。</p>}
      <ol className="manager-timeline">{rows.map(r => {
        const key = id(r);
        const visible = key === selected;
        const peaks = visible ? (q.data?.peaks ?? []).filter(p => Math.abs(p.time_ms - r.time_ms) <= 120_000) : [];
        const history = visible ? (q.data?.history ?? []).find(h => h.time_ms <= r.time_ms && h.time_ms + 60_000 > r.time_ms) : undefined;
        return <li key={key}>
          <button className="manager-event" type="button" aria-expanded={visible} onClick={() => select(visible ? undefined : key)}>
            <time>{dateTime(r.time_ms)}</time><b>{eventText[r.kind] || r.kind}</b><span>{r.source} · #{r.event_id}</span>
          </button>
          {visible && <div className="manager-event-detail">
            {r.detail && <p>{r.detail}</p>}
            {!!peaks.length && <><strong>事件前后两分钟的实测分钟峰值</strong>
              <ul>{peaks.map(p => <li key={p.metric + p.time_ms}>{dateTime(p.time_ms)} · {peakText[p.metric]} {peakFormat(p)}</li>)}</ul></>}
            {history && <p>同一分钟已保存汇总：内存峰值 {bytes(history.metrics[0]?.peak)} · CPU 峰值 {percent(history.metrics[3]?.peak)}。</p>}
            {!peaks.length && !history && <p>该时段没有可对齐的 OpenRevo 资源采样。</p>}
          </div>}
        </li>;
      })}</ol>
    </section>
    {!!q.data?.peaks.length && <section className="resource-panel">
      <h3>最近采样的峰值</h3><p>最近约十分钟，只显示存在的实测值。</p>
      <div className="manager-data-pills">{q.data.peaks.slice(0, 12).map(p => <span key={p.metric + p.time_ms}>
        {peakText[p.metric]} <b>{peakFormat(p)}</b><small>{dateTime(p.time_ms)}</small>
      </span>)}</div>
    </section>}
  </div>;
}

function ReceiptView({ receipt, compact }: { receipt: Receipt; compact?: boolean }) {
  const ok = receipt.rows.filter(r => r.status === 'done').length;
  const names: Record<string, string> = { cleanup: '回收', startup: '启动项', close: '应用关闭' };
  return <details className="manager-receipt" open={compact || undefined}>
    <summary>{dateTime(receipt.time_ms)} · {names[receipt.kind] || receipt.kind} · {ok} / {receipt.rows.length} 项已核对</summary>
    {receipt.warning && <p role="alert">{receipt.warning}</p>}
    {!!receipt.rows.length && <ul>{receipt.rows.map((r, i) => <li key={i}>
      <b>{rowStatus[r.status] || r.status}</b> · <span className="manager-path">{r.item}</span><small>{r.detail}</small>
    </li>)}</ul>}
    {receipt.volumes.map(v => {
      const d = v.free_after != null && v.free_before != null ? v.free_after - v.free_before : null;
      return <p key={v.root}>{v.root} 卷可用容量 {bytes(v.free_before)} → {bytes(v.free_after)}
        {d != null && '（观测变化 ' + (d >= 0 ? '+' : '−') + bytes(Math.abs(d)) + '；含同期其他磁盘活动）'}</p>;
    })}
  </details>;
}
function Receipts({ generation, open }: { generation: number; open: (p: ShellPage) => void }) {
  const q = useRead(manager.receipts, generation);
  return <section className="resource-panel">
    <div className="resource-line"><h3>本机操作记录</h3><button className="toggle-btn" onClick={() => open('recycle')}>打开 Windows 回收站</button></div>
    <p>保留最近 50 次操作。回收文件在 Windows 中恢复；自启备份在「启动项」中恢复。</p>
    <Reading q={q} label="操作记录" />{!q.loading && !q.data?.length && <p>还没有已保存的维护记录。</p>}
    {q.data?.map(r => <ReceiptView key={r.id + '/' + r.time_ms} receipt={r} />)}
  </section>;
}


function SessionControls({ open }: { open: (p: ShellPage) => void }) {
  const session = useRead(manager.session);
  const awake = useRead(manager.awakeStatus);
  const [readback, setReadback] = useState<AwakeState>();
  const [duration, setDuration] = useState(30);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const current = readback ?? awake.data;
  useEffect(() => {
    if (!current?.enabled) return;
    let active = true;
    const timer = window.setInterval(() => {
      if (!document.hidden) void manager.awakeStatus().then(value => { if (active) setReadback(value); }).catch(() => {});
    }, 4000);
    return () => { active = false; window.clearInterval(timer); };
  }, [current?.enabled]);
  const setAwake = async (enable: boolean) => {
    setBusy(true); setError('');
    try {
      await manager.setAwake(enable, enable && duration ? duration : undefined);
      setReadback(await manager.awakeStatus());
    } catch (cause) {
      setError(textError(cause));
    } finally { setBusy(false); }
  };
  return <section className="resource-panel" aria-label="会话权限与后台保活">
    <div className="resource-line">
      <h3>会话权限与后台保活</h3>
      <button className="toggle-btn" onClick={() => open('startup')}>打开 Windows 启动项</button>
    </div>
    {session.data?.hardware === false && <p>标准权限管理模式：未加载驱动、固件控制、插件或硬件配置。硬件调校需先退出此会话，再从 Windows 以管理员身份手动启动。</p>}
    {session.data?.hardware === true && <p>管理员硬件会话：仅手动启动，应用不登记或重建登录自启任务。</p>}
    <div className="resource-actions">
      <label>熄屏保活 <select aria-label="熄屏保活时长" value={duration} disabled={busy} onChange={e => setDuration(Number(e.target.value))}>
        <option value="30">30 分钟</option><option value="120">2 小时</option><option value="480">8 小时</option><option value="0">直到关闭</option>
      </select></label>
      <button className="toggle-btn" disabled={busy || !current} onClick={() => void setAwake(!current?.enabled)}>
        {current?.enabled ? '停止保活' : '开启保活'}
      </button>
      {current && <span role="status">{current.enabled ? current.remaining_ms == null ? '已开启 · 本次会话' :
        '已开启 · 约剩 ' + Math.ceil(current.remaining_ms / 60000) + ' 分钟' : '保活未开启'}</span>}
    </div>
    <p>允许屏幕熄灭，只申请阻止系统空闲睡眠；主动睡眠或退出结束本次请求。</p>
    {error && <p className="manager-error" role="alert">{error}</p>}
  </section>;
}
export function ManagerApp() {
  const [page, setPage] = useState<Page>('health');
  const [version, setVersion] = useState(0);
  const [healthGeneration, setHealthGeneration] = useState(0);
  const [job, setJob] = useState<Scan>();
  const [preview, setPreview] = useState<Preview>();
  const [last, setLast] = useState<Receipt>();
  const [notice, setNotice] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const health = useRead(manager.health, healthGeneration);
  const intent = useRef(false);

  const open = (p: ShellPage) => { setError(''); void manager.shell(p).catch(e => setError(textError(e))); };
  const prepare = async (action: Action) => {
    if (intent.current) return;
    intent.current = true;
    setBusy(true); setError(''); setNotice('');
    try { setPreview(await manager.preview(action)); }
    catch (e) { setError(textError(e)); }
    finally { intent.current = false; setBusy(false); }
  };
  const execute = async () => {
    if (!preview || intent.current) return;
    intent.current = true;
    setBusy(true); setError('');
    try {
      const result = await manager.apply(preview.id);
      setPreview(undefined);
      if (result.canceled) { setNotice('已取消，未执行维护。'); return; }
      setLast(result);
      setNotice('维护已结束。各项目的实际结果在操作回执中。');
      setVersion(n => n + 1);
      setHealthGeneration(n => n + 1);
    } catch (e) { setError(textError(e)); setPreview(undefined); }
    finally { intent.current = false; setBusy(false); }
  };
  const navigate = (p: Page) => { setPage(p); setError(''); setNotice(''); };
  useEffect(() => {
    if (!preview) return;
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !intent.current) setPreview(undefined);
    };
    window.addEventListener('keydown', escape);
    return () => window.removeEventListener('keydown', escape);
  }, [preview]);

  return <div className="manager-shell">
    <header className="manager-hero">
      <div><small>OPENREVO · {__APP_VERSION__}</small><h1>管理中心</h1><p>系统体检、空间分析与可核对的维护</p></div>
      <span className="manager-badge">本机处理 · 按需执行</span>
    </header>
    <nav aria-label="管理中心导航" className="manager-nav">{NAV.map(({ id, title, icon: Icon }) =>
      <button key={id} aria-current={page === id ? 'page' : undefined} type="button" className={'toggle-btn ' + (page === id ? 'active-indigo' : '')}
        onClick={() => navigate(id)}><Icon size={16} /> {title}</button>)}
    </nav>
    <main className="manager-content">
      {error && <p className="manager-error" role="alert">{error}</p>}
      {notice && <p className="manager-notice" role="status">{notice}</p>}
      {last && page !== 'receipts' && <ReceiptView receipt={last} compact />}
      {page === 'health' && <><div className="manager-top-action"><button className="toggle-btn" disabled={health.loading}
        onClick={() => setHealthGeneration(n => n + 1)}>刷新体检</button></div><HealthView health={health} open={open} navigate={navigate} /><SessionControls open={open} /></>}
      {page === 'storage' && <StorageView volumes={health.data?.volumes ?? []} job={job} setJob={setJob} open={open} preview={a => void prepare(a)} />}
      {page === 'resources' && <ResourceOverview onCloseProcess={process => void prepare({ kind: 'close', process })} />}
      {page === 'startup' && <StartupView open={open} generation={version} preview={a => void prepare(a)} />}
      {page === 'apps' && <AppsView open={open} />}
      {page === 'timeline' && <TimelineView navigate={navigate} />}
      {page === 'receipts' && <Receipts generation={version} open={open} />}
    </main>
    {preview && <div className="sys-modal-backdrop manager-modal" role="presentation">
      <section className="sys-modal-card" role="dialog" aria-modal="true" aria-labelledby="manager-preview-title">
        <header className="sys-modal-header"><h2 id="manager-preview-title">{preview.caption}</h2></header>
        <div className="sys-modal-body"><p>{preview.items.length} 个项目{preview.selected_bytes != null && ' · 已选文件占盘 ' + bytes(preview.selected_bytes)}</p>
          <ul className="manager-preview-list">{preview.items.map((item, i) => <li key={i}>{item}</li>)}</ul>
          <p>{preview.recovery}</p>
        </div>
        <footer className="sys-modal-actions"><button type="button" autoFocus className="toggle-btn" disabled={busy}
          onClick={() => setPreview(undefined)}>取消</button>
          <button type="button" className="toggle-btn danger" disabled={busy} onClick={() => void execute()}>{busy ? '等待 Windows 确认…' : '确认并执行'}</button></footer>
      </section>
    </div>}
  </div>;
}
