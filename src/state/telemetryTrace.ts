import type { HardwareState } from '../types/hardware';

export const TRACE_LIMIT = 600;
export const TRACE_GAP_MS = 5000;
const columns = {
  cpuUsage: 'cpu_usage_pct',
  gpuUsage: 'gpu_usage_pct',
  cpuTemp: 'cpu_temp_c',
  gpuTemp: 'gpu_temp_c',
  cpuClock: 'cpu_clock_mhz',
  gpuClock: 'gpu_clock_mhz',
  mainFan: 'main_fan_rpm',
  auxFan: 'aux_fan_rpm',
} as const;
export type TraceMetric = keyof typeof columns;
export type TraceSample = { time: number; gap: boolean } & Record<TraceMetric, number | null>;
const valid = (value: number | undefined, low: number, high: number) =>
  typeof value === 'number' && Number.isFinite(value) && value >= low && value <= high ? value : null;

export function traceSample(status: HardwareState, time: number): TraceSample | null {
  if (status.is_mock || !Number.isFinite(time)) return null;
  return {
    time,
    gap: false,
    cpuUsage: valid(status.cpu_usage, 0, 100),
    gpuUsage: status.gpu_name ? valid(status.gpu_usage, 0, 100) : null,
    cpuTemp: valid(status.cpu_temp, 1, 150),
    gpuTemp: valid(status.gpu_temp, 1, 150),
    cpuClock: valid(status.cpu_freq, 1, 20000),
    gpuClock: valid(status.gpu_core_freq, 1, 20000),
    mainFan: valid(status.cpu_fan_rpm, 1, 50000),
    auxFan: valid(status.gpu_fan_rpm, 1, 50000),
  };
}

export function appendTrace(samples: TraceSample[], sample: TraceSample): TraceSample[] {
  const prev = samples[samples.length - 1];
  return [...samples.slice(-(TRACE_LIMIT - 1)), {
    ...sample,
    gap: sample.gap || !!prev && (sample.time <= prev.time || sample.time - prev.time > TRACE_GAP_MS),
  }];
}

export function tracePath(samples: TraceSample[], metric: TraceMetric, ceiling: number): string {
  let connected = false;
  const lastIndex = Math.max(1, samples.length - 1);
  return samples.map((sample, i) => {
    if (sample.gap) connected = false;
    const value = sample[metric];
    if (value === null) { connected = false; return ''; }
    const point = (connected ? 'L' : 'M') + (2 + i / lastIndex * 296).toFixed(1)
      + ',' + (58 - Math.min(1, value / ceiling) * 56).toFixed(1);
    connected = true;
    return point;
  }).join(' ');
}

export function traceCsv(samples: TraceSample[]): string {
  const keys = Object.keys(columns) as TraceMetric[];
  return ['timestamp_utc,gap_before,' + Object.values(columns).join(','),
    ...samples.map(s => [new Date(s.time).toISOString(), Number(s.gap), ...keys.map(k => s[k])].join(',')),
  ].join('\r\n') + '\r\n';
}
