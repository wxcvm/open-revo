import assert from 'node:assert/strict';
import test from 'node:test';
import { traceSample, appendTrace, tracePath, traceCsv, TRACE_LIMIT } from '../src/state/telemetryTrace.ts';

const state = { cpu_usage: 0, cpu_temp: 51, gpu_usage: 12, gpu_name: 'Radeon',
  gpu_temp: 0, cpu_freq: 2300, gpu_core_freq: 820, cpu_fan_rpm: 4000, gpu_fan_rpm: 0 };
const sample = (time, overrides = {}) => traceSample({ ...state, ...overrides }, time);

test('demo excluded; real zero usage retained; missing readings stay null', () => {
  assert.equal(sample(0, {is_mock:true}), null);
  const row = sample(0, { cpu_freq:NaN });
  assert.equal(row.cpuUsage, 0);
  assert.equal(row.cpuClock, null);
  assert.equal(row.gpuTemp, null);
  assert.equal(row.mainFan, 4000);
  assert.equal(row.auxFan, null);
  assert.equal(sample(0, {gpu_name:undefined}).gpuUsage, null);
});

test('bounded ring: oldest is dropped, gap and backward clock reset the segment', () => {
  let rows = [];
  for (let i=0; i<TRACE_LIMIT+1; i++) rows = appendTrace(rows, sample(i*1500));
  assert.equal(rows.length, TRACE_LIMIT);
  assert.equal(rows[0].time, 1500);
  assert.equal(appendTrace(rows,sample(rows.at(-1).time+6000)).at(-1).gap, true);
  assert.equal(appendTrace(rows,sample(10)).at(-1).gap, true);
});

test('paths do not connect unknown or interrupted readings; CSV exports UTC and blanks', () => {
  const rows = [sample(0,{gpu_temp:52}),sample(1500),sample(3000,{gpu_temp:53}),sample(4500,{gpu_temp:54})];
  const path = tracePath(rows,'gpuTemp',120);
  assert.equal(path.match(/M/g).length,2);
  assert.equal(path.match(/L/g).length,1);
  assert.equal(tracePath([rows[0],{...rows[2],gap:true}],'gpuTemp',120).includes('L'),false);
  assert.equal(traceCsv(rows).split('\r\n')[2],'1970-01-01T00:00:01.500Z,0,0,12,51,,2300,820,4000,');
});
