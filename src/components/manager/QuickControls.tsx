import { useEffect, useRef, useState } from 'react';
import { BatteryCharging, SlidersHorizontal, Volume2 } from 'lucide-react';
import { manager, textError } from './api';
import type { ControlAction, ControlSnapshot, PowerMode } from './api';

const MODES: Record<PowerMode, string> = {
  efficiency: '最佳能效',
  balanced: '平衡',
  performance: '最佳性能',
};

export function QuickControls() {
  const [snapshot, setSnapshot] = useState<ControlSnapshot>();
  const [level, setLevel] = useState<number>();
  const [mode, setMode] = useState<PowerMode>();
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const pending = useRef(false);
  const bind = (value: ControlSnapshot) => {
    setSnapshot(value);
    setLevel(value.audio.value?.volume);
    setMode(value.power.value ?? undefined);
  };

  useEffect(() => {
    let live = true;
    void manager.quickControls().then(value => { if (live) bind(value); },
      cause => { if (live) setError(textError(cause)); }).finally(() => { if (live) setBusy(false); });
    return () => { live = false; };
  }, []);

  const readOrApply = async (action?: ControlAction) => {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setError('');
    setNotice('');
    try {
      const value = action ? await manager.setQuickControl(action) : await manager.quickControls();
      bind(value);
      if (action?.kind === 'volume' && value.audio.value) {
        setNotice('Windows 回读音量：' + value.audio.value.volume + '%');
      } else if (action?.kind === 'mute' && value.audio.value) {
        setNotice(value.audio.value.muted ? 'Windows 回读：已静音' : 'Windows 回读：已取消静音');
      } else if (action?.kind === 'energy' && value.power.value) {
        setNotice('Windows 回读能源模式：' + MODES[value.power.value]);
      }
    } catch (cause) {
      setError(textError(cause));
      if (action) {
        try { bind(await manager.quickControls()); }
        catch { setSnapshot(undefined); setLevel(undefined); setMode(undefined); }
      }
    } finally {
      setBusy(false);
      pending.current = false;
    }
  };
  const audio = snapshot?.audio.value;
  const power = snapshot?.power.value;
  return <section className="resource-panel manager-quick" aria-label="本机快捷控制">
    <div className="resource-line">
      <h3><SlidersHorizontal size={18} /> 本机快捷控制</h3>
      <button type="button" className="toggle-btn" disabled={busy} onClick={() => void readOrApply()}>重新读取状态</button>
    </div>
    <p>从这里直接改变 Windows 状态，按系统实际回读显示结果，不跳设置页面。</p>
    <div className="manager-card-grid">
      <div className="resource-panel manager-quick-card">
        <h4><Volume2 size={16} /> 默认播放设备</h4>
        {audio && <div className="resource-actions">
          <label className="manager-quick-level">输出音量 <b>{level}%</b>
            <input type="range" min={0} max={100} step={1} aria-label="输出音量" disabled={busy} value={level ?? audio.volume}
              onChange={event => setLevel(Number(event.target.value))} />
          </label>
          <button type="button" className="toggle-btn active-indigo" disabled={busy || level == null || level === audio.volume}
            onClick={() => level != null && void readOrApply({ kind: 'volume', device: audio.device, expected: audio.volume, level })}>应用音量</button>
          <button type="button" className="toggle-btn" disabled={busy} aria-pressed={audio.muted}
            onClick={() => void readOrApply({ kind: 'mute', device: audio.device, expected: audio.muted, muted: !audio.muted })}>
            {audio.muted ? '取消静音' : '静音'}
          </button>
        </div>}
        <small>{audio ? '当前：' + audio.volume + '% · ' + (audio.muted ? '已静音' : '未静音') :
          snapshot?.audio.problem ?? (busy ? '读取播放设备…' : '未读到播放设备')}</small>
      </div>
      <div className="resource-panel manager-quick-card">
        <h4><BatteryCharging size={16} /> Windows 能源模式</h4>
        {power && <div className="resource-actions">
          <select aria-label="Windows 能源模式" disabled={busy} value={mode ?? power}
            onChange={event => setMode(event.target.value as PowerMode)}>
            {Object.entries(MODES).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
          </select>
          <button type="button" className="toggle-btn active-indigo" disabled={busy || !mode || mode === power}
            onClick={() => mode && void readOrApply({ kind: 'energy', expected: power, mode })}>应用能源模式</button>
        </div>}
        <small>{power ? '当前：' + MODES[power] + ' · 不改 OpenRevo 的风扇、限充或刷新率策略' :
          snapshot?.power.problem ?? (busy ? '读取 Windows 能源模式…' : '未读到 Windows 能源模式')}</small>
      </div>
    </div>
    {error && <p className="manager-error" role="alert">{error}</p>}
    {notice && <p className="manager-notice" role="status">{notice}</p>}
  </section>;
}
