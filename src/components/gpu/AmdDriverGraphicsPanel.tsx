import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AlertTriangle, RotateCcw, Zap } from 'lucide-react';

/* --------------------------------------------------------------------------
 * AMD driver graphics features (ADLX readback).
 *
 * The Rust commands are authored in parallel with this panel; these types
 * mirror the agreed Tauri contract and live here so the component stays
 * self-contained:
 *   get_gpu_driver_graphics_state()            -> DriverGraphicsState
 *   set_gpu_driver_toggle({ feature, enabled }) -> GpuUpdateReport
 *   set_gpu_driver_chill_fps({ minFps, maxFps }) -> GpuUpdateReport
 *   set_gpu_driver_frtc_fps({ fps })            -> GpuUpdateReport
 *   restore_gpu_driver_session()                -> GpuRestoreReport
 *
 * Honesty rules enforced below:
 *   - enabled === null  -> unknown readback: never shown as on/off, toggle off.
 *   - supported === false -> the row is visibly disabled with the reason.
 *   - A report that is not applied never looks applied: the driver message is
 *     shown with error styling and report.state is always adopted.
 * -------------------------------------------------------------------------- */

export type ToggleState = {
  supported: boolean;
  enabled: boolean | null;
  read_error: string | null;
};

export type IntRange = { min: number; max: number; step: number };

export type DriverGraphicsToggleFeature =
  | 'anti_lag'
  | 'chill'
  | 'image_sharpening'
  | 'frame_rate_target';

export type DriverGraphicsState = {
  available: boolean;
  detail: string | null;
  adlx_version: string | null;
  gpu_name: string | null;
  anti_lag: ToggleState;
  chill: ToggleState & { min_fps: number | null; max_fps: number | null; range: IntRange | null };
  frame_rate_target: ToggleState & { fps: number | null; range: IntRange | null };
  image_sharpening: ToggleState;
  enhanced_sync: ToggleState;
  tuning: {
    manual_gfx: boolean | null;
    manual_vram: boolean | null;
    manual_fan: boolean | null;
    manual_power: boolean | null;
    auto_tuning: boolean | null;
    preset_tuning: boolean | null;
  };
  session: { changed: string[]; can_restore: boolean };
  warnings: string[];
};

export type GpuUpdateReport = {
  applied: boolean;
  message: string | null;
  state: DriverGraphicsState;
};

export type GpuRestoreReport = {
  restored: string[];
  skipped: { field: string; reason: string }[];
  message: string | null;
  state: DriverGraphicsState;
};

export interface AmdDriverGraphicsPanelProps {
  className?: string;
}

type ControlKey =
  | 'anti_lag'
  | 'chill'
  | 'chill_fps'
  | 'image_sharpening'
  | 'frame_rate_target'
  | 'frtc_fps'
  | 'restore';

type PanelMessage = { tone: 'info' | 'error'; text: string };

const EVIDENCE_FIELDS: { key: keyof DriverGraphicsState['tuning']; label: string }[] = [
  { key: 'manual_gfx', label: '手动核心频率' },
  { key: 'manual_vram', label: '手动显存频率' },
  { key: 'manual_fan', label: '手动风扇转速' },
  { key: 'manual_power', label: '手动功率上限' },
  { key: 'auto_tuning', label: '自动调校' },
  { key: 'preset_tuning', label: '预设调校' },
];

const errorText = (error: unknown): string => {
  if (error instanceof Error) return error.message;
  if (typeof error === 'string') return error;
  return String(error);
};

/** Parse a draft input; empty or non-numeric text is not a value. */
const parseFps = (raw: string): number | null => {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? parsed : null;
};

/** Snap to the driver-reported step grid, then clamp into the driver range. */
const alignToRange = (value: number, range: IntRange): number => {
  const step = range.step > 0 ? range.step : 1;
  const snapped = range.min + Math.round((value - range.min) / step) * step;
  const clamped = Math.min(range.max, Math.max(range.min, snapped));
  return Math.round(clamped * 1000) / 1000;
};

const reportMessage = (report: { applied: boolean; message: string | null }): PanelMessage | null => {
  if (!report.applied) {
    return { tone: 'error', text: report.message ?? '驱动未应用本次变更，且未说明原因。' };
  }
  return report.message ? { tone: 'info', text: report.message } : null;
};

export default function AmdDriverGraphicsPanel({ className }: AmdDriverGraphicsPanelProps) {
  const [state, setState] = useState<DriverGraphicsState | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [pending, setPending] = useState<ControlKey[]>([]);
  const [message, setMessage] = useState<PanelMessage | null>(null);
  const [restoreReport, setRestoreReport] = useState<GpuRestoreReport | null>(null);
  const [chillMinDraft, setChillMinDraft] = useState('');
  const [chillMaxDraft, setChillMaxDraft] = useState('');
  const [frtcDraft, setFrtcDraft] = useState('');
  const [chillError, setChillError] = useState<string | null>(null);
  const [frtcError, setFrtcError] = useState<string | null>(null);
  const mounted = useRef(true);
  const inFlight = useRef(false);
  const loadGeneration = useRef(0);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      loadGeneration.current++;
    };
  }, []);

  const isPending = (key: ControlKey) => pending.includes(key);

  /** Draft inputs always follow the newest driver readback. */
  const syncDrafts = (next: DriverGraphicsState) => {
    setChillMinDraft(next.chill.min_fps === null ? '' : String(next.chill.min_fps));
    setChillMaxDraft(next.chill.max_fps === null ? '' : String(next.chill.max_fps));
    setFrtcDraft(next.frame_rate_target.fps === null ? '' : String(next.frame_rate_target.fps));
  };

  const adopt = (next: DriverGraphicsState) => {
    if (!mounted.current) return;
    setState(next);
    setLoading(false);
    setLoadError(null);
    syncDrafts(next);
  };

  const load = async () => {
    const generation = ++loadGeneration.current;
    setLoading(true);
    setLoadError(null);
    try {
      const next = await invoke<DriverGraphicsState>('get_gpu_driver_graphics_state');
      if (generation !== loadGeneration.current) return;
      adopt(next);
    } catch (error) {
      if (!mounted.current || generation !== loadGeneration.current) return;
      setState(null);
      setLoading(false);
      setLoadError(errorText(error));
    }
  };

  useEffect(() => {
    void load();
    // Load once on mount; retry is driven by the error row button.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const run = async (key: ControlKey, action: () => Promise<void>) => {
    if (inFlight.current) return;
    inFlight.current = true;
    loadGeneration.current++;
    setRestoreReport(null);
    setPending((prev) => (prev.includes(key) ? prev : [...prev, key]));
    setMessage(null);
    try {
      await action();
    } catch (error) {
      if (mounted.current) setMessage({ tone: 'error', text: errorText(error) });
    } finally {
      inFlight.current = false;
      if (mounted.current) setPending((prev) => prev.filter((item) => item !== key));
    }
  };

  const handleToggle = async (feature: DriverGraphicsToggleFeature, enabled: boolean) => {
    await run(feature, async () => {
      const report = await invoke<GpuUpdateReport>('set_gpu_driver_toggle', { feature, enabled });
      adopt(report.state);
      setMessage(reportMessage(report));
    });
  };

  const handleApplyChill = async (st: DriverGraphicsState) => {
    const range = st.chill.range;
    if (!range) {
      setChillError('驱动未提供帧率范围，暂不能写入。');
      return;
    }
    const min = parseFps(chillMinDraft);
    const max = parseFps(chillMaxDraft);
    if (min === null || max === null) {
      setChillError('请填写最小与最大帧率（数值）。');
      return;
    }
    const nextMin = alignToRange(min, range);
    const nextMax = alignToRange(max, range);
    if (nextMin > nextMax) {
      setChillError(`最小帧率不能高于最大帧率（${nextMin} > ${nextMax}），未写入驱动。`);
      return;
    }
    setChillError(null);
    setChillMinDraft(String(nextMin));
    setChillMaxDraft(String(nextMax));
    const clamped: string[] = [];
    if (nextMin !== min) clamped.push(`最小帧率 ${min} → ${nextMin}`);
    if (nextMax !== max) clamped.push(`最大帧率 ${max} → ${nextMax}`);
    const clampNote: PanelMessage | null =
      clamped.length > 0 ? { tone: 'info', text: `已按驱动范围收敛：${clamped.join('，')}` } : null;
    await run('chill_fps', async () => {
      const report = await invoke<GpuUpdateReport>('set_gpu_driver_chill_fps', { minFps: nextMin, maxFps: nextMax });
      adopt(report.state);
      setMessage(reportMessage(report) ?? clampNote);
    });
  };

  const handleApplyFrtc = async (st: DriverGraphicsState) => {
    const range = st.frame_rate_target.range;
    if (!range) {
      setFrtcError('驱动未提供帧率范围，暂不能写入。');
      return;
    }
    const fps = parseFps(frtcDraft);
    if (fps === null) {
      setFrtcError('请填写目标帧率（数值）。');
      return;
    }
    const nextFps = alignToRange(fps, range);
    setFrtcError(null);
    setFrtcDraft(String(nextFps));
    const clampNote: PanelMessage | null =
      nextFps !== fps ? { tone: 'info', text: `已按驱动范围收敛：目标帧率 ${fps} → ${nextFps}` } : null;
    await run('frtc_fps', async () => {
      const report = await invoke<GpuUpdateReport>('set_gpu_driver_frtc_fps', { fps: nextFps });
      adopt(report.state);
      setMessage(reportMessage(report) ?? clampNote);
    });
  };

  const handleRestore = async () => {
    await run('restore', async () => {
      const report = await invoke<GpuRestoreReport>('restore_gpu_driver_session');
      adopt(report.state);
      setRestoreReport(report);
      const note: PanelMessage = report.skipped.length === 0
        ? { tone: 'info', text: report.message ?? `已回读确认恢复：${report.restored.join('、')}` }
        : { tone: 'error', text: report.message ?? '驱动未恢复任何设置，详见下方跳过原因。' };
      setMessage(note);
    });
  };

  /** Unknown readback is never rendered as on or off. */
  const renderToggleChip = (toggle: ToggleState) => {
    if (!toggle.supported) return <span className="amdgpu-chip amdgpu-chip--muted">不支持</span>;
    if (toggle.enabled === null) return <span className="amdgpu-chip amdgpu-chip--unknown">未知</span>;
    return toggle.enabled ? (
      <span className="amdgpu-chip amdgpu-chip--ok">已开启</span>
    ) : (
      <span className="amdgpu-chip amdgpu-chip--muted">已关闭</span>
    );
  };

  const renderToggleRow = (
    feature: DriverGraphicsToggleFeature,
    label: string,
    description: string,
    toggle: ToggleState,
    st: DriverGraphicsState,
  ) => {
    const unknown = toggle.supported && toggle.enabled === null;
    const rowDisabled = !st.available || !toggle.supported || unknown;
    const busy = isPending(feature);
    return (
      <div className={`quick-row amdgpu-row${rowDisabled ? ' is-disabled' : ''}`} key={feature}>
        <div className="amdgpu-row-main">
          <div className="sys-row-label">{label}</div>
          <div className="sys-row-desc">{description}</div>
          {!toggle.supported && <div className="sys-row-desc">驱动上报该功能不受支持，不提供开关。</div>}
          {unknown && (
            <div className="sys-row-desc">
              驱动回读未知{toggle.read_error ? `：${toggle.read_error}` : '，因此不显示为开或关。'}
            </div>
          )}
          {!st.available && <div className="sys-row-desc">驱动接口当前不可用，仅展示上次读取的状态。</div>}
        </div>
        <div className="sys-row-controls">
          <div className="amdgpu-chips">{renderToggleChip(toggle)}</div>
          {toggle.supported && (
            <button
              type="button"
              className={`toggle-btn${toggle.enabled === true ? ' active-emerald' : ''}`}
              aria-pressed={toggle.enabled === true}
              disabled={rowDisabled || pending.length > 0}
              onClick={() => void handleToggle(feature, toggle.enabled !== true)}
            >
              {busy ? '写入中…' : unknown ? '未知' : toggle.enabled ? '关闭' : '开启'}
            </button>
          )}
        </div>
      </div>
    );
  };

  const renderBody = (st: DriverGraphicsState) => {
    const chillRange = st.chill.range;
    const frtcRange = st.frame_rate_target.range;
    const chillLocked = !st.available || !chillRange || pending.length > 0;
    const frtcLocked = !st.available || !frtcRange || st.frame_rate_target.enabled !== true || pending.length > 0;
    return (
      <>
        <div className={st.available ? 'amdgpu-status is-ok' : 'amdgpu-status is-offline'}>
          <span className="amdgpu-dot" />
          <span>{st.available ? '驱动接口可用' : st.detail ?? '驱动接口不可用'}</span>
        </div>

        {st.warnings.length > 0 && (
          <div className="amdgpu-warnings" role="status">
            <div className="amdgpu-warnings-title">
              <AlertTriangle size={12} /> 驱动上报的告警
            </div>
            <ul className="amdgpu-warnings-list">
              {st.warnings.map((warning, index) => (
                <li key={`${index}-${warning}`}>{warning}</li>
              ))}
            </ul>
          </div>
        )}

        {message && (
          <div
            className={`amdgpu-message${message.tone === 'error' ? ' is-error' : ''}`}
            role={message.tone === 'error' ? 'alert' : 'status'}
          >
            {message.text}
          </div>
        )}

        <div className="sys-rows">
          {renderToggleRow('anti_lag', 'Anti-Lag', '降低输入延迟；状态以驱动回读为准。', st.anti_lag, st)}

          {renderToggleRow('chill', 'Radeon Chill', '按帧率区间限制渲染节奏，降低功耗与发热。', st.chill, st)}
          {st.chill.supported && (
            <div className="sys-sub-panel">
              <div className="amdgpu-fps-group">
                <span className="amdgpu-fps-label">最小</span>
                <input
                  className="amdgpu-fps"
                  type="number"
                  inputMode="numeric"
                  aria-label="Radeon Chill 最小帧率"
                  min={chillRange?.min}
                  max={chillRange?.max}
                  step={chillRange?.step}
                  value={chillMinDraft}
                  disabled={chillLocked}
                  onChange={(event) => setChillMinDraft(event.target.value)}
                />
                <span className="amdgpu-fps-unit">FPS</span>
                <span className="amdgpu-fps-label">最大</span>
                <input
                  className="amdgpu-fps"
                  type="number"
                  inputMode="numeric"
                  aria-label="Radeon Chill 最大帧率"
                  min={chillRange?.min}
                  max={chillRange?.max}
                  step={chillRange?.step}
                  value={chillMaxDraft}
                  disabled={chillLocked}
                  onChange={(event) => setChillMaxDraft(event.target.value)}
                />
                <span className="amdgpu-fps-unit">FPS</span>
                <button
                  type="button"
                  className="toggle-btn"
                  disabled={chillLocked}
                  onClick={() => void handleApplyChill(st)}
                >
                  {isPending('chill_fps') ? '写入中…' : '应用'}
                </button>
              </div>
              <p className="amdgpu-note">
                {chillRange
                  ? `驱动范围 ${chillRange.min}–${chillRange.max} FPS，步进 ${chillRange.step}；超出范围会在应用时收敛，最小帧率高于最大帧率会被拦截。`
                  : '驱动未提供帧率范围，暂不能写入。'}
              </p>
              {chillError && (
                <div className="amdgpu-error" role="alert">
                  {chillError}
                </div>
              )}
            </div>
          )}

          {renderToggleRow(
            'image_sharpening',
            'Image Sharpening',
            '驱动侧锐化；强度不在本面板调节。',
            st.image_sharpening,
            st,
          )}

          {renderToggleRow(
            'frame_rate_target',
            'Frame Rate Target Control',
            '限制渲染帧率上限以降低功耗。',
            st.frame_rate_target,
            st,
          )}
          {st.frame_rate_target.supported && (
            <div className="sys-sub-panel">
              <div className="amdgpu-fps-group">
                <span className="amdgpu-fps-label">目标</span>
                <input
                  className="amdgpu-fps"
                  type="number"
                  inputMode="numeric"
                  aria-label="目标帧率上限"
                  min={frtcRange?.min}
                  max={frtcRange?.max}
                  step={frtcRange?.step}
                  value={frtcDraft}
                  disabled={frtcLocked}
                  onChange={(event) => setFrtcDraft(event.target.value)}
                />
                <span className="amdgpu-fps-unit">FPS</span>
                <button
                  type="button"
                  className="toggle-btn"
                  disabled={frtcLocked}
                  onClick={() => void handleApplyFrtc(st)}
                >
                  {isPending('frtc_fps') ? '写入中…' : '应用'}
                </button>
              </div>
              <p className="amdgpu-note">
                {frtcRange
                  ? `先开启帧率限制再设置目标；驱动范围 ${frtcRange.min}–${frtcRange.max} FPS，步进 ${frtcRange.step}。`
                  : '驱动未提供帧率范围，暂不能写入。'}
              </p>
              {frtcError && (
                <div className="amdgpu-error" role="alert">
                  {frtcError}
                </div>
              )}
            </div>
          )}

          <div className="quick-row amdgpu-row is-disabled">
            <div className="amdgpu-row-main">
              <div className="sys-row-label">Enhanced Sync</div>
              <div className="sys-row-desc">
                {st.enhanced_sync.supported
                  ? '驱动报告该功能受支持，但本面板不提供切换，仅展示驱动回读状态。'
                  : '驱动上报该功能不受支持；此处仅作信息展示，不提供开关。'}
              </div>
              {st.enhanced_sync.read_error && (
                <div className="sys-row-desc">读取详情：{st.enhanced_sync.read_error}</div>
              )}
            </div>
            <div className="sys-row-controls">
              <div className="amdgpu-chips">{renderToggleChip(st.enhanced_sync)}</div>
            </div>
          </div>
        </div>

        <details className="sys-sub-panel" aria-label="调校支持度（驱动证据）">
          <summary className="amdgpu-section-title">调校支持度（驱动证据）</summary>
          <div className="amdgpu-evidence">
            {EVIDENCE_FIELDS.map((field) => {
              const value = st.tuning[field.key];
              return (
                <div className="amdgpu-evidence-item" key={field.key}>
                  <span className="amdgpu-evidence-name">{field.label}</span>
                  {value === true ? (
                    <span className="amdgpu-chip amdgpu-chip--ok">支持</span>
                  ) : value === false ? (
                    <span className="amdgpu-chip amdgpu-chip--muted">不支持</span>
                  ) : (
                    <span className="amdgpu-chip amdgpu-chip--unknown">未知</span>
                  )}
                </div>
              );
            })}
          </div>
          <p className="amdgpu-note">
            此处展示驱动实际返回的支持度；未知不等于不支持。本版本未接入核心／显存／风扇／功率写入，不提供未验证的滑块。
          </p>
        </details>

        <section className="sys-sub-panel" aria-label="恢复本次会话改动">
          <div className="amdgpu-session-head">
            <div className="amdgpu-row-main">
              <div className="sys-row-label">恢复本次会话改动</div>
              <div className="sys-row-desc">
                {st.session.can_restore
                  ? `可恢复：${st.session.changed.join('、') || '驱动未列出字段'}`
                  : st.session.changed.length > 0
                    ? `本次会话已改动：${st.session.changed.join('、')}（驱动报告不可恢复）`
                    : '本次会话尚未改动任何驱动设置。'}
              </div>
            </div>
            <button
              type="button"
              className="toggle-btn"
              disabled={!st.session.can_restore || pending.length > 0}
              onClick={() => void handleRestore()}
            >
              <RotateCcw size={12} /> {isPending('restore') ? '恢复中…' : '恢复会话改动'}
            </button>
          </div>
          {restoreReport && (
            <>
              {restoreReport.restored.length > 0 && (
                <p className="amdgpu-note">已回读确认恢复：{restoreReport.restored.join('、')}</p>
              )}
              {restoreReport.skipped.length > 0 && (
                <ul className="amdgpu-skip-list">
                  {restoreReport.skipped.map((item) => (
                    <li key={item.field}>
                      {item.field}：{item.reason}
                    </li>
                  ))}
                </ul>
              )}
              {restoreReport.restored.length === 0 && restoreReport.skipped.length === 0 && (
                <p className="amdgpu-note">驱动未返回已恢复或已跳过的字段。</p>
              )}
            </>
          )}
        </section>
      </>
    );
  };

  return (
    <section className={className ? `amdgpu-root ${className}` : 'amdgpu-root'} aria-label="AMD 驱动图形特性">
      <div className="amdgpu-head">
        <div className="amdgpu-title">
          <Zap size={14} /> AMD 驱动图形特性
        </div>
        <div className="amdgpu-meta">
          <span className="amdgpu-meta-line">ADLX {state?.adlx_version ?? '未提供'}</span>
          <span className="amdgpu-meta-line" title={state?.gpu_name ?? undefined}>
            {state?.gpu_name ?? 'GPU 型号未提供'}
          </span>
        </div>
      </div>

      {loadError !== null && (
        <div className="amdgpu-error-row" role="alert">
          <div>
            <div className="sys-row-desc">读取 AMD 驱动图形状态失败，未展示任何推测值。</div>
            <div className="amdgpu-error">{loadError}</div>
          </div>
          <button type="button" className="toggle-btn" onClick={() => void load()}>
            重试
          </button>
        </div>
      )}

      {state === null && loadError === null && (
        <div className="amdgpu-placeholder">{loading ? '正在读取 AMD 驱动图形状态…' : '暂无驱动状态。'}</div>
      )}

      {state !== null && renderBody(state)}
    </section>
  );
}
