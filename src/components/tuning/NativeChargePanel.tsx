import React, { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { BatteryCharging, RefreshCw } from 'lucide-react';

interface NativeChargeSnapshot {
  enabled: number;
  threshold1: number;
  threshold2: number;
}

interface NativeChargeState {
  matched: boolean;
  available: boolean;
  reason: string | null;
  enabled: number | null;
  threshold1: number | null;
  threshold2: number | null;
}

interface NativeChargeOutcome {
  applied: boolean;
  failed: string | null;
  rollback: string | null;
  state: NativeChargeSnapshot | null;
}

const WORKSTATION_T1 = 60;
const LONG_LIFE_T1 = 80;
const T2_FULL = 100;

const clampThreshold = (value: number) =>
  Number.isFinite(value) ? Math.min(99, Math.max(1, Math.floor(value))) : 1;

const describeSnapshot = (snapshot: NativeChargeSnapshot | null) =>
  snapshot
    ? `enabled=${snapshot.enabled} · T1=${snapshot.threshold1}% · T2=${snapshot.threshold2}%`
    : '回读不可用';

const describeOutcome = (outcome: NativeChargeOutcome) => {
  if (outcome.applied) {
    return `已写入并独立回读确认：${describeSnapshot(outcome.state)}`;
  }
  const details = [outcome.failed, outcome.rollback].filter(Boolean).join('；');
  return `未生效。${details || '写入结果未确认'}。已重新读取实际状态。`;
};

export const NativeChargePanel: React.FC = () => {
  const [state, setState] = useState<NativeChargeState | null>(null);
  const [threshold, setThreshold] = useState(LONG_LIFE_T1);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const pending = useRef(false);
  const generation = useRef(0);

  const refresh = useCallback(async () => {
    const current = ++generation.current;
    const next = await invoke<NativeChargeState>('get_native_charge_state');
    if (current !== generation.current) return next;
    setState(next);
    if (
      next.enabled === 1 &&
      next.threshold1 !== null &&
      next.threshold1 >= 1 &&
      next.threshold1 <= 99
    ) {
      setThreshold(next.threshold1);
    }
    return next;
  }, []);

  useEffect(() => {
    refresh().catch((cause) => setError(String(cause)));
    return () => { generation.current++; };
  }, [refresh]);

  if (!state || !state.matched) return null;

  const known =
    state.available &&
    state.enabled !== null &&
    state.threshold1 !== null &&
    state.threshold2 !== null;

  const statusText = known
    ? state.enabled === 1
      ? `已启用 · T1 ${state.threshold1}% · T2 ${state.threshold2}%`
      : `未启用（满充） · 字段 ${state.threshold1}/${state.threshold2}`
    : '状态未知';

  const run = async (enabled: boolean, nextThreshold: number) => {
    if (!known || pending.current || state.enabled === null || state.threshold1 === null || state.threshold2 === null) {
      return;
    }

    const expected: NativeChargeSnapshot = {
      enabled: state.enabled,
      threshold1: state.threshold1,
      threshold2: state.threshold2,
    };

    pending.current = true;
    generation.current++;
    setBusy(true);
    setError('');
    setMessage('');

    try {
      const outcome = await invoke<NativeChargeOutcome>('apply_native_charge_policy', {
        enabled,
        threshold: nextThreshold,
        expected,
      });
      setMessage(describeOutcome(outcome));
    } catch (cause) {
      setError(`${String(cause)}。已重新读取实际状态，不会乐观显示成功。`);
    } finally {
      try {
        await refresh();
      } catch (cause) {
        setError(String(cause));
      }
      setBusy(false);
      pending.current = false;
    }
  };

  if (!state.available) {
    return (
      <div className="sys-rows" role="group" aria-label="电池充电上限">
        <div className="quick-row">
          <div>
            <div className="sys-row-label">
              <BatteryCharging className="w-2.5 h-2.5" />
              电池充电上限
            </div>
            <div className="sys-row-desc">
              P916F-STX / BIOS 1.15 已匹配，但 PMC2 当前不可用。{state.reason || '未提供原因。'}
            </div>
            <div className="sys-row-desc">
              仅打开已存在的 signed inpoutx64 设备；不会安装、启动、删除或下载驱动。
            </div>
          </div>
          <div className="sys-row-controls">
            <button
              type="button"
              className="toggle-btn"
              disabled={busy}
              onClick={() => {
                setBusy(true);
                setError('');
                refresh()
                  .catch((cause) => setError(String(cause)))
                  .finally(() => setBusy(false));
              }}
            >
              <RefreshCw className="w-3 h-3" />
              重新读取
            </button>
          </div>
        </div>
        {error && (
          <div className="quick-row" role="alert">
            {error}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="sys-rows" role="group" aria-label="电池充电上限">
      <div className="quick-row">
        <div>
          <div className="sys-row-label">
            <BatteryCharging className="w-2.5 h-2.5" />
            电池充电上限
          </div>
          <div className="sys-row-desc">
            硬件当前状态：{statusText}。不依赖原厂控制中心。
          </div>
          <div className="sys-row-desc">
            设置后会独立回读确认；保留满电放电边界为 100%，不主动把电池放到目标电量。完整充电周期尚未验收。
          </div>
        </div>
        <div className="sys-row-controls">
          <button
            type="button"
            className="toggle-btn"
            disabled={busy}
            onClick={() => {
              setBusy(true);
              setError('');
              refresh()
                .catch((cause) => setError(String(cause)))
                .finally(() => setBusy(false));
            }}
          >
            <RefreshCw className="w-3 h-3" />
            刷新
          </button>
        </div>
      </div>

      <div className="quick-row">
        <div>
          <div className="sys-row-label">充电上限预设</div>
          <div className="sys-row-desc">
            预设只写入 T1 数值；T2 始终 100%。当前 T1 草稿：{threshold}%。
          </div>
        </div>
        <div className="sys-row-controls">
          <button
            type="button"
            className={`toggle-btn${threshold === WORKSTATION_T1 ? ' active-indigo' : ''}`}
            disabled={busy}
            onClick={() => setThreshold(WORKSTATION_T1)}
          >
            工作站 60%
          </button>
          <button
            type="button"
            className={`toggle-btn${threshold === LONG_LIFE_T1 ? ' active-indigo' : ''}`}
            disabled={busy}
            onClick={() => setThreshold(LONG_LIFE_T1)}
          >
            电池长效 80%
          </button>
        </div>
      </div>

      <div className="quick-row">
        <div>
          <div className="sys-row-label">自定义 T1</div>
          <div className="sys-row-desc">
            可设 1–99%。需要充满时，请使用“关闭上限（满充）”。
          </div>
          <div className="sys-row-desc">
            禁用/重置会清除 EC 中的 T1/T2 为 0/0/0；不会自动恢复旧阈值，恢复原策略需要重新显式应用。
          </div>
        </div>
        <div className="sys-row-controls">
          <input
            className="tuning-input-number"
            type="number"
            inputMode="numeric"
            aria-label="自定义 T1 充电上限"
            min={1}
            max={99}
            step={1}
            value={threshold}
            disabled={busy}
            onChange={(event) => setThreshold(clampThreshold(Number(event.target.value)))}
          />
          <button
            type="button"
            className="toggle-btn active-indigo"
            disabled={busy || !known}
            onClick={() => void run(true, threshold)}
          >
            {busy ? '写入并回读中…' : `启用 ${threshold}% / ${T2_FULL}%`}
          </button>
          <button
            type="button"
            className="toggle-btn"
            disabled={busy || !known || state.enabled === 0}
            onClick={() => void run(false, T2_FULL)}
          >
            关闭上限（满充）
          </button>
        </div>
      </div>

      {error && (
        <div className="quick-row" role="alert">
          {error}
        </div>
      )}
      {message && (
        <div className="quick-row" role="status">
          {message}
        </div>
      )}
    </div>
  );
};

export default NativeChargePanel;
