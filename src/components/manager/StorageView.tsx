import { useEffect, useState } from 'react';
import { FolderOpen, Search, HardDrive, Trash2 } from 'lucide-react';
import { bytes } from '../monitor/ResourceChart';
import { manager, basename, dateTime, textError } from './api';
import type { Action, Scan, ScanRow, ScanTarget, Section, ShellPage, Volume, VolumeOverview } from './api';

const SECTIONS: { id: Section; title: string }[] = [
  { id: 'tree', title: '占用分布' }, { id: 'largest', title: '大文件' },
  { id: 'duplicates', title: '重复文件' }, { id: 'cleanup', title: '可回收项目' },
];
const running = (s: Scan) => s.state === 'running' || s.state === 'canceling';
const complete = (s: Scan) => s.state === 'complete' || s.state === 'partial';

function SpaceMap({ rows, total, reveal }: { rows: ScanRow[]; total: number; reveal: (row: ScanRow) => void }) {
  if (!total) return null;
  let x = 0;
  return <section aria-label="扫描范围占用图" className="manager-map">
    <svg viewBox="0 0 1000 52" preserveAspectRatio="none" role="img" aria-label="文件夹占盘比例图">
      {rows.filter(r => (r.allocated ?? 0) > 0).slice(0, 16).map((r, i) => {
        const w = (r.allocated ?? 0) / total * 1000, at = x;
        x += w;
        return <rect key={r.id} className={'resource-tone-' + (i % 6)} x={at} width={w} height="52">
          <title>{basename(r.path)} · {bytes(r.allocated)}</title>
        </rect>;
      })}
      {x < 1000 && <rect className="manager-map-rest" x={x} width={1000-x} height="52"><title>其余已计入项目</title></rect>}
    </svg>
    <div className="resource-legend">{rows.filter(r => (r.allocated ?? 0) > 0).slice(0, 8).map((r, i) =>
      <button key={r.id} type="button" className={'resource-tone-' + (i % 6)} onClick={() => reveal(r)} title={r.path}>
        <span className="resource-dot" />{basename(r.path)} <strong>{bytes(r.allocated)}</strong>
      </button>)}</div>
  </section>;
}


const VOLUME_CATEGORIES = ['用户目录', '应用目录', 'Windows 系统路径', '多路径硬链接', '其他已读'] as const;
const change = (current: number | null | undefined, previous: number | null | undefined) => {
  if (current == null || previous == null) return '—';
  const value = current - previous;
  return value === 0 ? bytes(0) : (value > 0 ? '+' : '−') + bytes(Math.abs(value));
};

function VolumePanel({ overview, busy, save }: { overview: VolumeOverview; busy: boolean; save: () => void }) {
  const { current, previous } = overview;
  const totals = current.totals;
  const capacityGap = totals ? totals.total - totals.used - totals.free : 0;
  const entries = VOLUME_CATEGORIES.map((label, index) => ({
    label, current: current.classified[index], previous: previous?.classified[index], tone: index,
  }));
  const all = [...entries, {
    label: '未对账占用', current: current.unresolved, previous: previous?.unresolved, tone: 5,
  }];
  let x = 0;
  return <section className="resource-panel manager-volume-panel" aria-label="整盘空间对账">
    <div className="resource-line">
      <h3>整盘空间对账</h3>
      <button className="toggle-btn" disabled={busy || !overview.save_available || overview.saved} onClick={save}>
        {overview.saved ? '本次快照已保存' : busy ? '正在保存…' : '保存本次卷快照'}
      </button>
    </div>
    <p>Windows 卷级读数 · {dateTime(current.captured_ms)}{previous ? ' · 上次保存：' + dateTime(previous.captured_ms) : ' · 尚无此卷的历史快照'}</p>
    <div className="manager-data-pills">
      <span>总容量 <b>{bytes(current.totals?.total)}</b></span>
      <span>系统已用 <b>{bytes(current.totals?.used)}</b></span>
      <span>系统可用 <b>{bytes(current.totals?.free)}</b></span>
      {!!capacityGap && totals && <span>卷容量余差 <b>{change(totals.total, totals.used + totals.free)}</b></span>}
      {!!totals?.reserved && <span>系统报告的保留空间 <b>{bytes(totals.reserved)}</b></span>}
      {!!totals?.pool_unavailable && <span>存储池不可用 <b>{bytes(totals.pool_unavailable)}</b></span>}
      {previous && <span>系统已用变化 <b>{change(current.totals?.used, previous.totals?.used)}</b></span>}
      {previous && current.totals && previous.totals && current.totals.total !== previous.totals.total &&
        <span>卷容量变化 <b>{change(current.totals.total, previous.totals.total)}</b></span>}
    </div>
    {!!capacityGap && <p>卷容量余差＝总容量－系统已用－系统可用。Windows 单独报告的保留与池不可用空间不重复相加；下方分类图只分解系统已用。</p>}
    {current.totals && current.totals.used > 0 && current.unresolved != null && <div className="manager-map">
      <svg viewBox="0 0 1000 40" preserveAspectRatio="none" role="img" aria-label="整盘已用空间分类图">
        {all.filter(item => item.current != null && item.current > 0).map(item => {
          const width = (item.current ?? 0) / current.totals!.used * 1000, at = x;
          x += width;
          return <rect key={item.label} className={'resource-tone-' + item.tone} x={at} width={width} height="40">
            <title>{item.label} · {bytes(item.current)}</title>
          </rect>;
        })}
      </svg>
    </div>}
    <table className="resource-table manager-table">
      <thead><tr><th scope="col">按可读路径分组</th><th scope="col">本次已分配</th>{previous && <th scope="col">较上次保存</th>}</tr></thead>
      <tbody>{all.map(item => <tr key={item.label}>
        <td><span className={'resource-tone-' + item.tone}><span className="resource-dot" /></span>{item.label}</td>
        <td>{bytes(item.current)}</td>{previous && <td>{change(item.current, item.previous)}</td>}
      </tr>)}</tbody>
    </table>
    <p>同一物理文件只计一次；硬链接另列。未对账占用含文件系统元数据、不可读或未扫描项目，不视作可释放空间。目录分类变化也可能来自读取覆盖变化。</p>
    {current.unresolved == null && <p>卷级读数缺失，或卷级已用与逐文件读数未对齐；本次不计算差额。</p>}
    {current.recycle && <p>回收站：{bytes(current.recycle.bytes)} · {current.recycle.items} 项（Windows Shell 另行报告，不叠加进上述分类）。</p>}
    {overview.notice && <p role="status">{overview.notice}</p>}
    <p>仅点击保存时记录卷标识、容量和分类汇总，不保存文件名或路径。</p>
  </section>;
}
export function StorageView({ volumes, job, setJob, open, preview }: {
  volumes: Volume[]; job?: Scan; setJob: (next: Scan) => void;
  open: (p: ShellPage) => void; preview: (a: Action) => void;
}) {
  const [root, setRoot] = useState('');
  const [error, setError] = useState('');
  const [section, setSection] = useState<Section>(job?.kind === 'cleanup' ? 'cleanup' : 'tree');
  const [offset, setOffset] = useState(0);
  const [rows, setRows] = useState<ScanRow[]>([]);
  const [chosen, setChosen] = useState<number[]>([]);
  const [working, setWorking] = useState(false);
  const [saving, setSaving] = useState(false);
  const [verifyDuplicates, setVerifyDuplicates] = useState(false);
  const [paging, setPaging] = useState(false);
  const jobId = job?.id, done = job && complete(job), inProgress = job && running(job);
  const drive = root || volumes[0]?.root || '';
  const effectiveSection = job?.kind === 'cleanup' ? 'cleanup' : (section === 'cleanup' ? 'tree' : section);

  useEffect(() => {
    if (!jobId || !inProgress) return;
    let stop = false, timer = 0;
    const poll = async () => {
      try {
        const r = await manager.status(jobId);
        if (!stop) setJob(r);
      } catch (e) {
        if (!stop && !document.hidden) setError(textError(e));
      } finally {
        if (!stop) timer = window.setTimeout(poll, document.hidden ? 1500 : 550);
      }
    };
    timer = window.setTimeout(poll, 300);
    return () => { stop = true; window.clearTimeout(timer); };
  }, [jobId, inProgress, setJob]);

  useEffect(() => {
    let active = true;
    setRows([]);
    if (!jobId || !done) { setPaging(false); return; }
    setPaging(true);
    void manager.page(jobId, effectiveSection, offset).then(r => { if (active) setRows(r); },
      e => { if (active) setError(textError(e)); }).finally(() => { if (active) setPaging(false); });
    return () => { active = false; };
  }, [jobId, done, effectiveSection, offset]);

  const start = async (target: ScanTarget) => {
    setWorking(true); setError('');
    try {
      const r = await manager.scan(target);
      if (r) { setOffset(0); setSection(r.kind === 'cleanup' ? 'cleanup' : 'tree'); setChosen([]); setJob(r); }
    } catch (e) { setError(textError(e)); }
    finally { setWorking(false); }
  };
  const saveVolume = async () => {
    if (!jobId || saving) return;
    setSaving(true); setError('');
    try { setJob(await manager.saveVolume(jobId)); }
    catch (e) { setError(textError(e)); }
    finally { setSaving(false); }
  };
  const cancel = () => jobId && void manager.cancel(jobId).catch(e => setError(textError(e)));
  const reveal = (r: ScanRow) => jobId && void manager.reveal(jobId, r.id).catch(e => setError(textError(e)));
  const total = job?.total[SECTIONS.findIndex(s => s.id === effectiveSection)] ?? 0;
  const stats = job?.stats;
  const ready = !working && !inProgress && !saving;
  const toggle = (id: number) => setChosen(curr => curr.includes(id) ? curr.filter(i => i !== id) : curr.length < 200 ? [...curr, id] : curr);

  return <div className="manager-stack">
    <section className="resource-panel">
      <div className="resource-line"><h3><HardDrive size={18} /> 空间分析</h3><button className="toggle-btn" onClick={() => open('storage')}>Windows 存储设置</button></div>
      <p>整盘扫描默认只读取元数据；按需勾选后才校验重复文件内容。大文件和重复项只分析、定位，不下载云端占位文件。</p>
      <div className="resource-actions">
        <label>选择本地卷 <select aria-label="选择本地卷" value={drive} onChange={e => setRoot(e.target.value)}>
          {!volumes.length && <option value="">暂无可用本地卷</option>}
          {volumes.map(v => <option key={v.root} value={v.root}>{v.root} · 可用 {bytes(v.free)}</option>)}
        </select></label>
        <button className="toggle-btn active-indigo" disabled={!ready || !drive} onClick={() => void start({ kind: 'volume', root: drive, verify_duplicates: verifyDuplicates })}><Search size={15} /> 扫描选中卷</button>
        <button className="toggle-btn" disabled={!ready} onClick={() => void start({ kind: 'folder', verify_duplicates: verifyDuplicates })}><FolderOpen size={15} /> 选择目录并扫描</button>
        <label><input type="checkbox" checked={verifyDuplicates} disabled={!ready}
          onChange={e => setVerifyDuplicates(e.target.checked)} /> 同时校验重复文件内容</label>
      </div>
    </section>
    <section className="resource-panel">
      <div className="resource-line"><h3><Trash2 size={18} /> 清理预览</h3><button className="toggle-btn" onClick={() => open('recycle')}>打开回收站</button></div>
      <p>候选范围：超过七天的临时文件类型，以及已关闭的 Edge、Chrome、Brave 已知缓存。选中后移入回收站，不会直接释放磁盘容量。</p>
      <button className="toggle-btn" disabled={!ready} onClick={() => void start({ kind: 'cleanup' })}>扫描可回收项目</button>
    </section>
    {error && <p className="manager-error" role="alert">{error}</p>}
    {job && <section className="resource-panel">
      <div className="resource-line"><h3>扫描结果</h3>{inProgress && <button className="toggle-btn" onClick={cancel} disabled={job.state === 'canceling'}>取消扫描</button>}</div>
      <p className="manager-path">{job.scope}</p>
      <p role="status">{({
        running: '正在扫描…', canceling: '正在等待当前文件读取结束…', canceled: '已取消；以下不作为完整统计',
        failed: '扫描失败', complete: '扫描完成', partial: '扫描完成 · 有未读取或限额项',
      })[job.state]} · {job.progress.files.toLocaleString()} 个计入文件 · {job.progress.skipped.toLocaleString()} 个跳过条目</p>
      {job.error && <p className="manager-error" role="alert">{job.error}</p>}
      {job.notes.map(n => <p key={n}>{n}</p>)}
      {done && stats && <>
        <div className="manager-data-pills">
          <span>已读文件大小 <b>{bytes(stats.logical)}</b></span><span>已知占盘 <b>{bytes(stats.allocated)}</b></span>
          <span>跳过的硬链接别名 <b>{stats.aliases}</b></span>
        </div>
        {job.overview && <VolumePanel overview={job.overview} busy={saving} save={() => void saveVolume()} />}
        {!!stats.unknown_allocation && <p>{stats.unknown_allocation} 个文件未取得分配大小；未当成零占用。</p>}
        {!!stats.uncategorized && <p>{stats.uncategorized} 个文件已计入总量，所属一级目录未单独展示。</p>}
        {stats.cleanup_limited && <p>候选列表最多显示 10,000 项，其他项目不进入此次操作。</p>}
        {stats.duplicates_limited && <p>重复检测存在访问限制、文件变化或读取预算限制；只列出已验证的匹配。</p>}
        {job.kind === 'space' && <div className="resource-tabs" aria-label="扫描结果分类">
          {SECTIONS.slice(0, 3).map(s => <button key={s.id} aria-pressed={effectiveSection === s.id}
            className={'toggle-btn ' + (effectiveSection === s.id ? 'active-indigo' : '')} onClick={() => { setSection(s.id); setOffset(0); }}>{s.title}</button>)}
        </div>}
        {effectiveSection === 'tree' && offset === 0 && <SpaceMap rows={rows} total={stats.allocated} reveal={reveal} />}
        {effectiveSection === 'duplicates' && <p>{job.duplicates_requested === false ?
          '本轮只读元数据。勾选「同时校验重复文件内容」再扫描，即可验证相同尺寸的候选文件。' :
          '同组条目同长度、完整 SHA‑256 一致；同文件的多个硬链接不组成重复组。'}</p>}
        {effectiveSection === 'cleanup' && <div className="resource-actions">
          <b>已选 {chosen.length} 项（每次最多 200 项）</b>
          <button className="toggle-btn" disabled={!rows.length || paging}
            onClick={() => setChosen(prev => [...new Set([...prev, ...rows.map(r => r.id)])].slice(0, 200))}>选择本页</button>
          <button className="toggle-btn" disabled={!chosen.length} onClick={() => setChosen([])}>取消选择</button>
          <button className="toggle-btn active-indigo" disabled={!chosen.length} onClick={() => jobId && preview({ kind: 'cleanup', job_id: jobId, ids: chosen })}>预览回收</button>
        </div>}
        {paging ? <p role="status">正在读取本页…</p> : !rows.length ? <p>此分类没有可显示的项目。</p> :
          <div className="resource-table-wrap"><table className="resource-table manager-table"><thead><tr>
            <th scope="col">项目</th><th scope="col">文件大小</th><th scope="col">已分配空间</th><th scope="col">定位</th>
          </tr></thead><tbody>{rows.map(r => <tr key={r.id}>
            <td className="manager-path" title={r.path}>{effectiveSection === 'cleanup' ?
              <label><input type="checkbox" aria-label={'选择 ' + basename(r.path)} checked={chosen.includes(r.id)} onChange={() => toggle(r.id)} /> {basename(r.path)}</label> : basename(r.path)}
              <small>{r.group ? '重复组 ' + r.group + ' · ' : r.count > 1 ? r.count + ' 个文件 · ' : ''}{r.path}</small></td>
            <td>{bytes(r.logical)}</td><td>{bytes(r.allocated)}</td>
            <td><button type="button" onClick={() => reveal(r)}>在资源管理器定位</button></td>
          </tr>)}</tbody></table></div>}
        {total > 40 && <div className="resource-actions">
          <button className="toggle-btn" disabled={offset === 0 || paging} onClick={() => setOffset(o => o - 40)}>上一页</button>
          <span>{offset + 1}–{Math.min(offset + 40, total)} / {total}</span>
          <button className="toggle-btn" disabled={offset + 40 >= total || paging} onClick={() => setOffset(o => o + 40)}>下一页</button>
        </div>}
      </>}
    </section>}
  </div>;
}
