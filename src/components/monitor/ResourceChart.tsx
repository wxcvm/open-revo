import { useMemo, useState } from 'react';

export type Maybe = number | null | undefined;
export type ChartRow = { time: number; values: Maybe[]; gap?: boolean; coverageMs?: number; spanMs?: number };
export type Line = { label: string; format: (value: Maybe) => string; tone?: number };
const valid = (n: Maybe): n is number => n != null && Number.isFinite(n) && n >= 0;
export const unavailable = '未取得';
const number = (n: number, digits = 1) => n.toLocaleString('zh-CN', { maximumFractionDigits: digits });
export function bytes(value: Maybe, suffix = '') {
  if (!valid(value)) return unavailable;
  const size = value >= 2 ** 30 ? 3 : value >= 2 ** 20 ? 2 : value >= 2 ** 10 ? 1 : 0;
  return `${number(value / 1024 ** size, size === 3 ? 2 : 1)} ${['B', 'KiB', 'MiB', 'GiB'][size]}${suffix}`;
}
export const bps = (n: Maybe) => bytes(n, '/s');
export const percent = (n: Maybe) => valid(n) ? `${number(n, 2)}%` : unavailable;
export const iops = (n: Maybe) => valid(n) ? `${number(n)} 次/s` : unavailable;
export const clock = (time: number, full = false) => new Date(time).toLocaleString('zh-CN', full
  ? { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' }
  : { hour: '2-digit', minute: '2-digit', second: '2-digit' });

const WIDTH = 600, HEIGHT = 116;
function paths(rows: ChartRow[], index: number, x: (n: number) => number, maximum: number) {
  let d = '', pen = false;
  for (const row of rows) {
    const y = row.values[index];
    if (!valid(y)) { pen = false; continue; }
    const start = !pen || row.gap;
    const px = x(row.time).toFixed(1);
    const py = (HEIGHT - (y / maximum) * (HEIGHT - 8) - 3).toFixed(1);
    d += start ? `M${px},${py}l0.05,0` : `L${px},${py}`;
    pen = true;
  }
  return d;
}

export function ResourceChart({ title, rows, lines, from, to, ceiling, history = false }:
{ title: string; rows: ChartRow[]; lines: Line[]; from: number; to: number; ceiling?: number; history?: boolean }) {
  const [selectedTime, setSelectedTime] = useState<number | null>(null);
  const x = (time: number) => Math.max(0, Math.min(WIDTH, (time - from) / Math.max(1, to - from) * WIDTH));
  const maximum = ceiling ?? Math.max(1, ...rows.flatMap(r => r.values.filter(valid)));
  const nearest = (time: number) => {
    let lo = 0, hi = rows.length - 1;
    while (lo < hi) {
      const middle = (lo + hi) >>> 1;
      if (rows[middle].time < time) lo = middle + 1; else hi = middle;
    }
    return lo > 0 && time - rows[lo - 1].time < rows[lo].time - time ? lo - 1 : lo;
  };
  const index = selectedTime == null ? rows.length - 1 : nearest(selectedTime);
  const current = rows[index];
  const plotted = rows.some(r => r.values.some(valid));
  return <section className="resource-chart" aria-label={title}>
    <div className="resource-line"><h4>{title}</h4><span>{current ? clock(current.time, history) : '无采样'}</span></div>
    <div className="resource-legend">
      {lines.map((line, i) => <span className={`resource-tone-${line.tone ?? i}`} key={line.label}>
        <span className="resource-dot" aria-hidden="true" />{line.label} <strong>{line.format(current?.values[i])}</strong>
      </span>)}
      {current?.coverageMs != null && current.spanMs != null && <span>覆盖 {number(100 * current.coverageMs / current.spanMs)}%</span>}
    </div>
    <div className="resource-chart-plot">
      <svg viewBox={`0 0 ${WIDTH} ${HEIGHT}`} preserveAspectRatio="none" aria-hidden="true"
        onPointerMove={event => {
          const rect = event.currentTarget.getBoundingClientRect();
          if (rows.length && rect.width) setSelectedTime(rows[nearest(from + (event.clientX - rect.left) / rect.width * (to - from))].time);
        }} onPointerLeave={() => setSelectedTime(null)}>
        {[20, 50, 80, 113].map(y => <line className="resource-grid-line" key={y} x1="0" x2={WIDTH} y1={y} y2={y} />)}
        {lines.map((line, i) => <path key={line.label} className={`resource-trace resource-tone-${line.tone ?? i}`}
          d={paths(rows, i, x, maximum)} />)}
        {current && <line className="resource-cursor" x1={x(current.time)} x2={x(current.time)} y1="0" y2={HEIGHT} />}
      </svg>
      {!plotted && <span className="resource-empty-chart">暂无可绘制的采样</span>}
    </div>
    <div className="resource-axis"><span>{clock(from, history)}</span><span>{clock(to, history)}</span></div>
    {rows.length > 1 && <input className="resource-scrub" type="range" min="0" max={rows.length - 1} value={index}
      aria-label={`${title}时间游标`} aria-valuetext={current && clock(current.time, history)}
      onChange={event => setSelectedTime(rows[Number(event.currentTarget.value)]?.time ?? null)}
      onBlur={() => setSelectedTime(null)} />}
  </section>;
}

export type Aggregate = { average: number; peak: number; covered_ms: number };
export type Minute = { time_ms: number; metrics: Array<Aggregate | null> };
export function useHistoryChart(minutes: Minute[], metric: number, hours: number, at: number) {
  return useMemo(() => {
    const step = (hours === 1 ? 1 : hours === 24 ? 6 : 42) * 60_000;
    const from = at - hours * 3_600_000;
    const byBucket = new Map<number, { weighted: number; covered: number; peak: number }>();
    for (const row of minutes) {
      const value = row.metrics[metric];
      if (!value || row.time_ms < from || row.time_ms > at) continue;
      const key = Math.floor(row.time_ms / step) * step;
      const prev = byBucket.get(key) ?? { weighted: 0, covered: 0, peak: 0 };
      prev.weighted += value.average * value.covered_ms;
      prev.covered += value.covered_ms;
      prev.peak = Math.max(prev.peak, value.peak);
      byBucket.set(key, prev);
    }
    const rows: ChartRow[] = [];
    let covered = 0;
    for (let start = Math.floor(from / step) * step; start <= at; start += step) {
      const value = byBucket.get(start);
      const span = Math.max(1, Math.min(start + step, at) - Math.max(start, from));
      covered += value?.covered ?? 0;
      rows.push({ time: Math.max(from, start), values: value ? [value.weighted / value.covered, value.peak] : [null, null],
        coverageMs: value?.covered ?? 0, spanMs: span, gap: value != null && value.covered < span });
    }
    return { from, rows, covered, stepMinutes: step / 60_000 };
  }, [minutes, metric, hours, at]);
}
