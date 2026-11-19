import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import type { HardwareState } from '../../types/hardware';
import { appendTrace, traceCsv, tracePath, traceSample, TRACE_LIMIT } from '../../state/telemetryTrace';
import type { TraceMetric, TraceSample } from '../../state/telemetryTrace';

const charts: { metric: TraceMetric; label: string; unit: string; ceiling: number }[] = [
  { metric: 'cpuUsage', label: 'CPU 使用率', unit: '%', ceiling: 100 },
  { metric: 'gpuUsage', label: 'GPU 使用率', unit: '%', ceiling: 100 },
  { metric: 'cpuTemp', label: 'CPU 温度', unit: '°C', ceiling: 120 },
  { metric: 'gpuTemp', label: 'GPU 温度', unit: '°C', ceiling: 120 },
];

export function TelemetryTracePanel() {
  const [recording, setRecording] = useState(false);
  const [samples, setSamples] = useState<TraceSample[]>([]);
  const [error, setError] = useState('');
  const [exportStatus, setExportStatus] = useState('');
  const [saving, setSaving] = useState(false);
  const exportPending = useRef(false);
  const active = useRef(false);
  const gap = useRef(false);

  useEffect(() => {
    if (!recording) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen<HardwareState>('hardware-state-updated', ({ payload }) => {
      if (disposed || !active.current) return;
      const sample = traceSample(payload, Date.now());
      if (sample) {
        sample.gap = gap.current;
        gap.current = false;
        setSamples(previous => appendTrace(previous, sample));
      } else {
        gap.current = true;
      }
    }).then(unlisten => {
      if (disposed) unlisten();
      else stop = unlisten;
    }).catch(() => {
      if (disposed) return;
      active.current = false;
      setRecording(false);
      setError('无法订阅硬件遥测，请重试。');
    });
    return () => { disposed = true; stop?.(); };
  }, [recording]);

  const toggle = () => {
    active.current = !active.current;
    if (active.current) gap.current = samples.length > 0;
    setError('');
    setRecording(active.current);
  };
  const download = async () => {
    if (exportPending.current || !samples.length) return;
    exportPending.current = true;
    setSaving(true);
    setExportStatus('');
    try {
      const path = await invoke<string | null>('export_telemetry_csv', { csv: traceCsv(samples) });
      setExportStatus(path ? `已保存：${path}` : '已取消保存，记录仍保留。');
    } catch (e) {
      setExportStatus(`保存失败，记录仍保留：${String(e)}`);
    } finally {
      exportPending.current = false;
      setSaving(false);
    }
  };
  const latest = samples[samples.length - 1];

  return (
    <section className="sys-sub-panel telemetry-trace" aria-label="性能记录">
      <div className="quick-row">
        <div>
          <div className="sys-row-label">性能记录</div>
          <div className="sys-row-desc">
            {recording ? '正在接收硬件采样' : '未记录'} · {samples.length}/{TRACE_LIMIT} 个采样。
            仅在本页保存在内存，离开前可导出 CSV。不采集 FPS，不运行第三方代理。
          </div>
        </div>
        <div className="sys-row-controls">
          <button type="button" className="toggle-btn active-cyan" onClick={toggle}>
            {recording ? '停止记录' : samples.length ? '继续记录' : '开始记录'}
          </button>
          <button type="button" className="toggle-btn" onClick={download} disabled={!latest || saving}>{saving ? '正在保存…' : '导出 CSV'}</button>
          <button type="button" className="toggle-btn" disabled={!latest || saving} onClick={() => setSamples([])}>清空记录</button>
        </div>
      </div>
      {error && <p className="sys-row-desc" role="alert">{error}</p>}
      {exportStatus && <p className="sys-row-desc usage-notice" role="status">{exportStatus}</p>}
      {latest && (
        <div className="telemetry-trace-grid">
          {charts.map(({ metric, label, unit, ceiling }) => (
            <div className="telemetry-trace-chart" key={metric}>
              <div className="mon-row-head"><span>{label}</span><strong>{latest[metric] ?? '—'} {unit}</strong></div>
              <svg viewBox="0 0 300 60" preserveAspectRatio="none" aria-hidden="true">
                <path d={tracePath(samples, metric, ceiling)} />
              </svg>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
