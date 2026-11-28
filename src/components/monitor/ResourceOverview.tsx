import { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ResourceChart, bytes, bps, clock, iops, percent, useHistoryChart } from './ResourceChart';
import type { ChartRow, Line, Maybe, Minute } from './ResourceChart';

const TABS = ['内存', 'CPU', '磁盘', '网络'] as const;
type Tab = typeof TABS[number];
type Mode = 'foreground' | 'background' | 'paused' | 'idle';
type HistoryState = { enabled: boolean; error: string | null };
type Point = { time_ms: number; summary: Maybe[]; selected: Maybe[]; gap_before: boolean };
type Item = { key: string; name: string };
type Process = { key: string | null; pid: number; name: string; group?: string | null; private_resident: Maybe; private_commit: Maybe; cpu_percent: Maybe };
type Disk = Item & { read: Maybe; write: Maybe; read_iops: Maybe; write_iops: Maybe };
type Interface = Item & { physical: boolean; up: boolean; rx: Maybe; tx: Maybe };
type Memory = Record<'installed' | 'visible' | 'reserved' | 'available' | 'used' | 'standby' | 'free_zero' |
  'available_unclassified' | 'modified' | 'nonpaged_pool' | 'paged_pool_resident' | 'cache_resident' |
  'commit' | 'commit_limit' | 'private_resident_sum' | 'unattributed_used', Maybe>
  & { available_split_error: string | null; private_readable: number; private_seen: number };
type Snapshot = {
  time_ms: number; sampled_in_ms: number; summary: Maybe[]; memory: Memory; processes: Process[]; disks: Disk[];
  interfaces: Interface[]; process_source: string | null; logical_cpus: number; disk_detail_available: boolean; net_available: boolean;
};
type Live = { snapshot: Snapshot | null; recent: Point[]; history: HistoryState; mode: Mode };

const METRICS: Line[] = [
  { label: '已用物理内存', format: n => bytes(n) }, { label: '可用物理内存', format: n => bytes(n) },
  { label: '系统提交量', format: n => bytes(n) }, { label: '系统 CPU 利用率', format: percent },
  { label: '读取', format: bps }, { label: '写入', format: bps },
  { label: '读操作', format: iops }, { label: '写操作', format: iops },
  { label: '接收', format: bps }, { label: '发送', format: bps },
];
const HISTORY_METRICS: Record<Tab, number[]> = { 内存: [0, 1, 2], CPU: [3], 磁盘: [4, 5, 6, 7], 网络: [8, 9] };
const errorText = (error: unknown) => String(error instanceof Error ? error.message : error);
const fmtBytes = (n: Maybe) => bytes(n);
const modeText: Record<Mode, string> = { foreground: '实时 · 约 2 秒', background: '低频汇总 · 约 10 秒', paused: '锁屏或休眠 · 已暂停', idle: '未采样' };

function FieldList({ fields }: { fields: Array<[string, string]> }) {
  return <dl className="resource-fields">{fields.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl>;
}

function MemoryBreakdown({ memory: m }: { memory?: Memory }) {
  const total = m?.installed && m.reserved != null ? m.installed : m?.visible;
  const availableParts = m?.standby != null && m.free_zero != null
    ? [{ label: '待机 · 可回收', n: m.standby, tone: 2 }, { label: '空闲 / 零页', n: m.free_zero, tone: 3 },
       ...(m.available_unclassified ? [{ label: '其余可用量', n: m.available_unclassified, tone: 5 }] : [])]
    : [{ label: '可用 · 未拆分', n: m?.available, tone: 2 }];
  const usedParts = m?.unattributed_used != null && m.private_resident_sum != null
    ? [{ label: '进程私有驻留（已读到）', n: m.private_resident_sum, tone: 0 },
       { label: '已用差额（未逐项归因）', n: m.unattributed_used, tone: 1 }]
    : [{ label: '已用 · 未拆分', n: m?.used, tone: 1 }];
  const parts = [...usedParts, ...availableParts, { label: '系统不可见', n: m?.reserved, tone: 4 }];
  let x = 0;
  const segments = total && m?.used != null && m.available != null ? parts.flatMap(p => {
    if (!p.n) return [];
    const width = 600 * p.n / total;
    const left = x;
    x += width;
    return <rect key={p.label} className={`resource-tone-${p.tone}`} x={left} y="0" width={width} height="18">
      <title>{p.label}：{bytes(p.n)}</title>
    </rect>;
  }) : [];
  return <section className="resource-panel" aria-label="物理内存对账">
    <h4>物理内存对账</h4>
    <FieldList fields={[
      ['物理安装', bytes(m?.installed)], ['Windows 可见', bytes(m?.visible)], ['系统不可见 / 预留差额', bytes(m?.reserved)],
      ['已用', bytes(m?.used)], ['可用（含待机）', bytes(m?.available)], ['提交量 / 上限（非物理内存）', `${bytes(m?.commit)} / ${bytes(m?.commit_limit)}`],
    ]} />
    <svg className="resource-memory-bar" viewBox="0 0 600 18" preserveAspectRatio="none" role="img" aria-label="按物理内存容量比例显示已用、可用与预留">
      {segments}
    </svg>
    <div className="resource-legend">
      {parts.map(p => <span className={`resource-tone-${p.tone}`} key={p.label}><span className="resource-dot" aria-hidden="true" />{p.label} <strong>{bytes(p.n)}</strong></span>)}
    </div>
    {m?.available_split_error && <p>{m.available_split_error}，不猜测拆分。</p>}
    {m?.private_resident_sum != null && m.unattributed_used == null && <p>私有驻留合计与系统已用无法对账，未计算差额。</p>}
    {!!m?.private_seen && <p>本轮读到 {m.private_readable} / {m.private_seen} 个进程的私有驻留；未取得的项目不当作 0。</p>}
    <details>
      <summary>查看系统侧指标和对账口径</summary>
      <FieldList fields={[
        ['改写页', bytes(m?.modified)], ['非分页池', bytes(m?.nonpaged_pool)],
        ['分页池（驻留）', bytes(m?.paged_pool_resident)], ['系统缓存（驻留）', bytes(m?.cache_resident)],
        ['私有驻留合计', bytes(m?.private_resident_sum)],
      ]} />
      <p>差额不是某个进程的占用，可能包含内核、驱动、共享映射、压缩页或没有读到的进程。系统侧指标口径会重叠，不再从差额中逐项扣减。待机页已经算进可用量；提交量是虚拟内存承诺，不是额外占用的 RAM。系统不可见的差额也不能全部算给核显。</p>
    </details>
  </section>;
}

type Sort = 'private_resident' | 'private_commit' | 'cpu_percent';
export function ProcessList({ processes, source, sort, onSort, pick, picked, onClose }:
{ processes: Process[]; source?: string | null; sort: Sort; onSort: (key: Sort) => void; pick: (p: Process) => void; picked?: string; onClose?: (key: string) => void }) {
  const [query, setQuery] = useState('');
  const [limit, setLimit] = useState(30);
  const [grouped, setGrouped] = useState(true);
  const [expanded, setExpanded] = useState<string[]>([]);
  const rows = useMemo(() => {
    const q = query.trim().toLocaleLowerCase();
    return processes.filter(p => p.name.toLocaleLowerCase().includes(q) || String(p.pid).includes(q))
      .sort((a, b) => (b[sort] ?? -1) - (a[sort] ?? -1) || a.pid - b.pid);
  }, [processes, query, sort]);
  const total = (members: Process[], metric: Sort): Maybe => {
    const values = members.map(p => p[metric]).filter((p): p is number => p != null);
    return values.length ? values.reduce((a, b) => a + b, 0) : null;
  };
  const shown = useMemo(() => {
    const groups = new Map<string, Process[]>();
    rows.forEach(p => {
      const key = grouped && p.group ? 'app:' + p.group : 'pid:' + p.pid;
      const members = groups.get(key) ?? [];
      members.push(p);
      groups.set(key, members);
    });
    return Array.from(groups.entries()).sort(([, a], [, b]) =>
      (total(b, sort) ?? -1) - (total(a, sort) ?? -1) || a[0].pid - b[0].pid);
  }, [rows, grouped, sort]);
  const heads: Array<[Sort, string]> = [['private_resident', '私有驻留'], ['private_commit', '私有提交'], ['cpu_percent', '全机 CPU']];
  const cell = (members: Process[], metric: Sort) => {
    const n = total(members, metric);
    if (n == null) return '未取得';
    return (members.some(p => p[metric] == null) ? '≥ ' : '') + (metric === 'cpu_percent' ? percent(n) : bytes(n));
  };
  const row = (p: Process, child = false) => <tr key={p.key ?? p.pid}
    className={(p.key != null && p.key === picked ? 'is-picked ' : '') + (child ? 'resource-process-child' : '')}>
    <td>{p.key ? <button type="button" aria-pressed={p.key === picked} title={p.name} onClick={() => pick(p)}>{p.name}</button> : p.name}</td>
    <td>{p.pid}</td><td>{bytes(p.private_resident)}</td><td>{bytes(p.private_commit)}</td><td>{percent(p.cpu_percent)}</td>
    {onClose && <td>{p.key && <button type="button" aria-label={'预览关闭 ' + p.name + ' ' + p.pid} onClick={() => onClose(p.key!)}>预览关闭</button>}</td>}
  </tr>;
  return <section className="resource-panel" aria-label="进程用量排名">
    <div className="resource-line">
      <h4>进程用量排名</h4>
      <input className="resource-search" type="search" aria-label="搜索名称或 PID" placeholder="搜索名称或 PID"
        value={query} onChange={e => { setQuery(e.currentTarget.value); setLimit(30); }} />
    </div>
    <div className="resource-actions"><button className="toggle-btn" aria-pressed={grouped}
      onClick={() => { setGrouped(g => !g); setLimit(30); }}>{grouped ? '按程序分组' : '逐进程显示'}</button>
      <span>仅按已读到的同一可执行文件路径分组；未知路径不合并。符号 ≥ 表示部分成员未取得读数。</span>
    </div>
    <p>{source ? source + ' · ' + rows.length + ' / ' + processes.length + ' 个进程 · 选择名称查看近期变化' : '进程计数器尚未就绪或不可用。'} 私有提交不计入物理内存对账。</p>
    <div className="resource-table-wrap"><table className="resource-table">
      <thead><tr><th scope="col">名称</th><th scope="col">PID</th>{heads.map(([key, label]) =>
        <th key={key} scope="col" aria-sort={sort === key ? 'descending' : 'none'}>
          <button type="button" onClick={() => { onSort(key); setLimit(30); }}>{label}{sort === key ? ' ↓' : ''}</button>
        </th>)}{onClose && <th scope="col">应用操作</th>}</tr></thead>
      <tbody>{shown.slice(0, limit).flatMap(([key, members]) => members.length === 1 ? [row(members[0])] : [
        <tr key={key} className="resource-process-group">
          <td><button type="button" aria-expanded={expanded.includes(key)} onClick={() =>
            setExpanded(list => list.includes(key) ? list.filter(k => k !== key) : [...list, key])}>{members[0].name}（{members.length} 个进程）</button></td>
          <td>分组</td>{heads.map(([metric]) => <td key={metric}>{cell(members, metric)}</td>)}{onClose && <td>展开后操作</td>}
        </tr>,
        ...(expanded.includes(key) ? members.map(p => row(p, true)) : []),
      ])}</tbody>
    </table></div>
    {rows.length === 0 && <p>{query ? '没有匹配项' : '还没有进程样本'}</p>}
    {shown.length > limit && <button className="toggle-btn resource-button" onClick={() => setLimit(limit + 50)}>显示更多（{shown.length - limit}）</button>}
  </section>;
}

function DeviceList({ devices, network, pick, picked }: { devices: Array<Disk | Interface>; network: boolean; pick: (item: Item) => void; picked?: string }) {
  return <div className="resource-table-wrap"><table className="resource-table">
    <thead><tr>
      <th scope="col">{network ? '接口（单独选择）' : 'Windows 物理磁盘实例'}</th>
      {network ? <th scope="col">类型 / 状态</th> : null}
      <th scope="col">{network ? '接收' : '读取'}</th><th scope="col">{network ? '发送' : '写入'}</th>
      {!network && <><th scope="col">读 IOPS</th><th scope="col">写 IOPS</th></>}
    </tr></thead>
    <tbody>{devices.map(d => <tr key={d.key} className={d.key === picked ? 'is-picked' : ''}>
      <td><button type="button" aria-pressed={d.key === picked} title={d.name} onClick={() => pick(d)}>{d.name}</button></td>
      {'physical' in d ? <><td>{d.physical ? '物理' : '虚拟 / 软件'} · {d.up ? '连接' : '未连接'}</td><td>{bps(d.rx)}</td><td>{bps(d.tx)}</td></>
        : <><td>{bps(d.read)}</td><td>{bps(d.write)}</td><td>{iops(d.read_iops)}</td><td>{iops(d.write_iops)}</td></>}
    </tr>)}</tbody>
  </table></div>;
}

type Range = 1 | 24 | 168;
function ResourceHistoryPanel({ tab, state, onStatus }: { tab: Tab; state?: HistoryState; onStatus: (s: HistoryState) => void }) {
  const [range, setRange] = useState<Range>(1);
  const [metricByTab, setMetricByTab] = useState<Partial<Record<Tab, number>>>({});
  const [series, setSeries] = useState<{ range: Range; rows: Minute[]; time: number }>();
  const [refresh, setRefresh] = useState(0);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [notice, setNotice] = useState('');
  const [historyError, setHistoryError] = useState('');
  useEffect(() => {
    let alive = true;
    let timer = 0;
    const load = async () => {
      try {
        const rows = await invoke<Minute[]>('get_resource_history', { hours: range });
        if (alive) { setSeries({ range, rows, time: Date.now() }); setHistoryError(''); }
      } catch (e) {
        if (alive) setHistoryError(errorText(e));
      } finally {
        if (alive) timer = window.setTimeout(load, 60_000);
      }
    };
    void load();
    return () => { alive = false; window.clearTimeout(timer); };
  }, [range, refresh]);
  const metric = metricByTab[tab] ?? HISTORY_METRICS[tab][0];
  const current = series?.range === range ? series : undefined;
  const chart = useHistoryChart(current?.rows ?? [], metric, range, current?.time ?? Date.now());
  const format = METRICS[metric].format;
  const change = async (action: 'toggle' | 'clear' | 'export') => {
    setBusy(true);
    setNotice('');
    setHistoryError('');
    try {
      if (action === 'export') {
        const path = await invoke<string | null>('export_resource_history_csv', { hours: range });
        setNotice(path ? `已保存：${path}` : '已取消导出');
      } else {
        const updated = action === 'clear' ? await invoke<HistoryState>('clear_resource_history')
          : await invoke<HistoryState>('set_resource_history', { enabled: !state?.enabled });
        onStatus(updated);
        if (action === 'clear') { setConfirm(false); setSeries(undefined); }
        setNotice(action === 'clear' ? '已清除已保存的资源汇总' : updated.enabled ? '已启用七天本机历史' : '已停止记录；已有记录仍可查看或清除');
        setRefresh(n => n + 1);
      }
    } catch (e) { setHistoryError(errorText(e)); }
    finally { setBusy(false); }
  };
  return <section className="resource-panel resource-history" aria-label="本机长期汇总">
    <div className="resource-line"><h4>本机长期汇总</h4>
      <button className="toggle-btn resource-button" role="switch" aria-label="保存七天资源汇总" aria-checked={state?.enabled ?? false}
        disabled={!state || busy} onClick={() => void change('toggle')}>{state?.enabled ? '已开启记录' : '开启七天记录'}</button>
    </div>
    <p>默认不保存。开启后仅在本机保存系统总量的每分钟平均、峰值和有效覆盖，不保存进程名、PID、接口名、路径或连接信息。托盘或离开本页只低频采总量，锁屏、休眠及退出后不采集。</p>
    <div className="resource-actions">
      <label>查看时段 <select value={range} onChange={e => setRange(Number(e.currentTarget.value) as Range)}>
        <option value="1">最近 1 小时</option><option value="24">最近 24 小时</option><option value="168">最近 7 天</option>
      </select></label>
      <label>指标 <select value={metric} onChange={e => setMetricByTab(prev => ({ ...prev, [tab]: Number(e.currentTarget.value) }))}>
        {HISTORY_METRICS[tab].map(i => <option key={i} value={i}>{METRICS[i].label}</option>)}
      </select></label>
      <button className="toggle-btn resource-button" type="button" disabled={busy} onClick={() => setRefresh(n => n + 1)}>刷新汇总</button>
      <button className="toggle-btn resource-button" type="button" disabled={busy || !current?.rows.length} onClick={() => void change('export')}>导出汇总 CSV</button>
      <button className="toggle-btn resource-button" type="button" disabled={busy || !state} onClick={() => setConfirm(true)}>清除汇总</button>
    </div>
    {confirm && <div className="resource-confirm"><span>清除本机已保存的资源汇总？不会改动记录开关。</span>
      <button className="toggle-btn danger resource-button" disabled={busy} onClick={() => void change('clear')}>确认清除</button>
      <button className="toggle-btn resource-button" disabled={busy} onClick={() => setConfirm(false)}>取消</button></div>}
    {(historyError || state?.error) && <p role="alert">{historyError || state?.error}</p>}
    {notice && <p role="status">{notice}</p>}
    {!current?.rows.length ? <p>{state?.enabled ? '开启后需要连续的有效样本，暂停时段保持空白。' : '该时段没有已保存的汇总。'}</p>
      : <><ResourceChart key={`${range}:${metric}`} history title={`${METRICS[metric].label} · 历史汇总`}
        rows={chart.rows} from={chart.from} to={current.time} ceiling={metric === 3 ? 100 : undefined}
        lines={[{ label: '平均', format }, { label: '峰值', format }]} />
        <p>此指标有效覆盖约 {Math.round(chart.covered / 60_000)} 分钟。图表每 {chart.stepMinutes} 分钟一段，覆盖不足断线；CSV 导出所选时段内每分钟的所有指标，缺失留空。</p></>}
  </section>;
}

type Selection = Item & { pid?: number };
const modeHelp: Record<Tab, string> = {
  内存: '从物理总量拆到可用、私有驻留和未归因差额；排序可切换到独立的私有提交口径。',
  CPU: '系统利用率和进程排名均以整机为 100%；新进程或缺少上一样本时不会显示为 0%。',
  磁盘: '来自 Windows 物理磁盘性能计数器，显示吞吐和操作次数；不是文件、卷容量或进程 I/O。卷容量仍可在监控概览查看。',
  网络: '物理网卡总量不含虚拟接口，避免同一流量叠加；虚拟接口可单独查看。不归因到进程或连接。',
};
export function ResourceOverview({ onCloseProcess }: { onCloseProcess?: (key: string) => void } = {}) {
  const [tab, setTab] = useState<Tab>('内存');
  const [showHistory, setShowHistory] = useState(false);
  const [live, setLive] = useState<Live>();
  const [state, setState] = useState<HistoryState>();
  const [error, setError] = useState('');
  const [picked, setPicked] = useState<Partial<Record<Tab, Selection>>>({});
  const [sort, setSort] = useState<Sort>('private_resident');
  const selection = picked[tab];
  const selectedKey = selection?.key ?? null;
  const selected = useRef<string | null>(null);
  const loadedFor = useRef<string | null>(null);
  const kick = useRef<() => void>(() => {});
  const historyVersion = useRef(0);
  useEffect(() => {
    selected.current = selectedKey;
    kick.current();
  }, [selectedKey]);
  useEffect(() => {
    let alive = true;
    let pending = false;
    let timer = 0;
    const release = () => { void invoke('release_resource_view').catch(() => {}); };
    const load = async () => {
      if (!alive || document.hidden || pending) return;
      window.clearTimeout(timer);
      pending = true;
      const key = selected.current;
      const version = historyVersion.current;
      try {
        const next = await invoke<Live>('get_resource_snapshot', { selected: key });
        if (!alive || document.hidden) return;
        loadedFor.current = key;
        setLive(next);
        if (version === historyVersion.current) setState(next.history);
        setError('');
      } catch (e) {
        if (alive && !document.hidden) { setLive(l => l && { ...l, snapshot: null, mode: 'idle' }); setError(errorText(e)); }
      } finally {
        pending = false;
        if (alive && !document.hidden) timer = window.setTimeout(load, selected.current === key ? 2_000 : 0);
      }
    };
    kick.current = () => void load();
    const visibility = () => {
      if (document.hidden) {
        window.clearTimeout(timer);
        setLive(l => l && { ...l, snapshot: null, mode: l.history.enabled ? 'background' : 'idle' });
        release();
      } else { void load(); }
    };
    document.addEventListener('visibilitychange', visibility);
    void load();
    return () => {
      alive = false;
      kick.current = () => {};
      window.clearTimeout(timer);
      document.removeEventListener('visibilitychange', visibility);
      release();
    };
  }, []);
  const snapshot = live?.snapshot;
  const to = Math.max(snapshot?.time_ms ?? 0, live?.recent.at(-1)?.time_ms ?? 0, Date.now());
  const from = to - 600_000;
  const points = (live?.recent ?? []).filter(p => p.time_ms >= from);
  const global = (metrics: number[]): ChartRow[] => points.map(p => ({
    time: p.time_ms, gap: p.gap_before, values: metrics.map(i => p.summary[i]),
  }));
  const detail = (metrics: number[]): ChartRow[] => loadedFor.current === selectedKey ? points.map(p => ({
    time: p.time_ms, gap: p.gap_before, values: metrics.map(i => p.selected[i]),
  })) : [];
  const pick = (item: Selection) => setPicked(prev => ({ ...prev, [tab]: prev[tab]?.key === item.key ? undefined : item }));
  const pickProcess = (p: Process) => { if (p.key != null) pick({ key: p.key, name: p.name, pid: p.pid }); };
  const selectedRowExists = selection != null && (tab === '内存' || tab === 'CPU' ? snapshot?.processes.some(p => p.key === selection.key)
    : tab === '磁盘' ? snapshot?.disks.some(d => d.key === selection.key) : snapshot?.interfaces.some(i => i.key === selection.key));
  const renderGlobal = (title: string, metrics: number[], ceiling?: number) => <ResourceChart
    key={title} title={title} rows={global(metrics)} from={from} to={to} ceiling={ceiling} lines={metrics.map(i => METRICS[i])} />;
  const renderSelected = (title: string, indices: number[], lines: Line[], ceiling?: number) => selection && <ResourceChart
    key={`${selection.key}:${title}`} title={title} rows={detail(indices)} from={from} to={to} ceiling={ceiling} lines={lines} />;
  return <div className="resource-overview">
    <div className="resource-line resource-tabs">
      <div role="group" aria-label="资源类别">{TABS.map(name =>
        <button className="toggle-btn resource-button" key={name} type="button" aria-pressed={tab === name}
          onClick={() => { setTab(name); setSort(name === 'CPU' ? 'cpu_percent' : 'private_resident'); }}>{name}</button>)}</div>
      <div role="group" aria-label="资源视图">
        <button className="toggle-btn resource-button" type="button" aria-pressed={!showHistory} onClick={() => setShowHistory(false)}>近 10 分钟</button>
        <button className="toggle-btn resource-button" type="button" aria-pressed={showHistory} onClick={() => setShowHistory(true)}>本机历史</button>
      </div>
    </div>
    <p className="resource-description">{modeHelp[tab]}</p>
    {showHistory ? <ResourceHistoryPanel tab={tab} state={state} onStatus={s => { historyVersion.current++; setState(s); }} />
      : <>
        <div className="resource-line resource-source">
          <span>{live ? modeText[live.mode] : '连接 Windows 采样器…'}</span>
          <span>{snapshot ? `采样 ${clock(snapshot.time_ms)} · 耗时 ${snapshot.sampled_in_ms} ms` : '没有最新明细 · 不使用假数据'}</span>
        </div>
        {error && <p role="alert">{error}</p>}
        <div className="resource-plot-grid">
          {tab === '内存' && <>{renderGlobal('物理内存走势', [0, 1], snapshot?.memory.visible ?? undefined)}{renderGlobal('系统提交量走势', [2])}</>}
          {tab === 'CPU' && renderGlobal('系统 CPU 趋势', [3], 100)}
          {tab === '磁盘' && <>{renderGlobal('所有物理磁盘 · 吞吐', [4, 5])}{renderGlobal('所有物理磁盘 · IOPS', [6, 7])}</>}
          {tab === '网络' && renderGlobal('所有物理网卡 · 吞吐', [8, 9])}
        </div>
        {tab === '内存' && <MemoryBreakdown memory={snapshot?.memory} />}
        {tab === 'CPU' && <p>{snapshot?.logical_cpus ? `Windows 可用逻辑处理器：${snapshot.logical_cpus} 个` : '逻辑处理器数量未取得。'}</p>}
        {(tab === '内存' || tab === 'CPU') && <ProcessList key={tab}
          processes={snapshot?.processes ?? []} source={snapshot?.process_source} sort={sort} onSort={setSort} pick={pickProcess} picked={selectedKey ?? undefined} onClose={onCloseProcess} />}
        {(tab === '磁盘' || tab === '网络') && <section className="resource-panel">
          <h4>{tab === '磁盘' ? '按物理磁盘' : '按网络接口'}</h4>
          <DeviceList devices={tab === '磁盘' ? snapshot?.disks ?? [] : snapshot?.interfaces ?? []} network={tab === '网络'} pick={pick} picked={selectedKey ?? undefined} />
          {(tab === '磁盘' ? !snapshot?.disk_detail_available : !snapshot?.net_available) && <p>设备计数器尚未就绪或不可用。</p>}
        </section>}
        {selection && <section className="resource-panel" aria-label="所选项目趋势">
          <div className="resource-line">
            <h4>{selection.name}{selection.pid != null ? ` · PID ${selection.pid}` : ''}</h4>
            <button className="toggle-btn resource-button" onClick={() => pick(selection)}>取消选择</button>
          </div>
          {!selectedRowExists && <p>所选项目当前不在读数中，仅保留已有样本，不用新进程或离线数值补齐。</p>}
          <div className="resource-plot-grid">
            {tab === '内存' && renderSelected('所选进程 · 私有驻留', [1], [{ label: '私有驻留', format: fmtBytes }])}
            {tab === 'CPU' && renderSelected('所选进程 · 全机 CPU', [0], [{ label: '全机 CPU', format: percent }])}
            {tab === '磁盘' && <>{renderSelected('所选磁盘 · 吞吐', [0, 1], [METRICS[4], METRICS[5]])}
              {renderSelected('所选磁盘 · IOPS', [2, 3], [METRICS[6], METRICS[7]])}</>}
            {tab === '网络' && renderSelected('所选接口 · 吞吐', [0, 1], [METRICS[8], METRICS[9]])}
          </div>
        </section>}
      </>}
  </div>;
}
