import React, { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Cpu, RotateCcw } from 'lucide-react';

interface CpuSideValues {
  max_processor_state: number;
  boost_mode: number;
  max_mhz: number | null;
}

interface CpuPolicySnapshot {
  plan_guid: string;
  ac: CpuSideValues;
  dc: CpuSideValues;
}

interface CpuSideUpdate {
  max_processor_state?: number;
  boost_mode?: number;
  max_mhz?: number;
}

interface CpuPolicyUpdate {
  ac: CpuSideUpdate;
  dc: CpuSideUpdate;
}

interface CpuPolicyOutcome {
  plan_guid: string;
  applied: string[];
  unchanged: string[];
  failed: string[];
  restored: string[];
  rollback_failed: string[];
  relinquished: string[];
  unverified: string[];
  activated: boolean;
  activation_error: string | null;
  reactivated: boolean;
  overlay_before: string | null;
  overlay_after: string | null;
  overlay_preserved: boolean;
  overlay_error: string | null;
  reverted: boolean;
}

const BOOST_MODES: Array<{ value: number; label: string }> = [
  { value: 0, label: 'Disabled (0)' },
  { value: 1, label: 'Enabled (1)' },
  { value: 2, label: 'Aggressive (2)' },
  { value: 3, label: 'Efficient Enabled (3)' },
  { value: 4, label: 'Efficient Aggressive (4)' },
  { value: 5, label: 'Aggressive at guaranteed (5)' },
  { value: 6, label: 'Efficient Aggressive at guaranteed (6)' },
];

const describeOutcome = (outcome: CpuPolicyOutcome): string => {
  const details = [
    outcome.activation_error ?? '',
    ...outcome.failed,
    ...outcome.rollback_failed,
    ...outcome.relinquished,
    ...outcome.unverified,
  ].filter(Boolean);

  if (!outcome.activated) {
    const recovery = outcome.reverted
      ? outcome.reactivated
        ? '已回读确认原保存值，并重新应用同一电源计划。'
        : '已回读确认原保存值，但未能重新激活同一电源计划。'
      : '未能证明全部保存值已恢复，请查看回读结果。';
    return `未生效：CPU 策略仅保存或已回滚，Windows 未确认激活。${details.join('；')}${details.length ? '。' : ''}${recovery}`;
  }

  const confirmed = [...outcome.applied, ...outcome.unchanged];
  let message = `Windows 已接受同一计划的重新应用，保存值回读一致：${confirmed.join('、') || '无变更'}`;
  if (outcome.overlay_error) {
    message += `；独立能源叠加未验证：${outcome.overlay_error}`;
  } else if (outcome.overlay_before !== outcome.overlay_after) {
    message += `；激活改变了能源叠加，已从已知原值恢复${outcome.overlay_preserved ? '并回读确认' : '但未确认'}`;
  }
  return message;
};

export const CpuPolicyPanel: React.FC = () => {
  const [snapshot, setSnapshot] = useState<CpuPolicySnapshot | null>(null);
  const [draft, setDraft] = useState<CpuPolicySnapshot | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');

  const load = useCallback(async () => {
    const next = await invoke<CpuPolicySnapshot>('get_cpu_policy');
    setSnapshot(next);
    setDraft(next);
    return next;
  }, []);

  useEffect(() => {
    load().catch((e) => setError(String(e)));
  }, [load]);

  const changeSide = (
    source: 'ac' | 'dc',
    field: keyof CpuSideValues,
    value: number | null,
  ) => {
    setDraft((current) => {
      if (!current) return current;
      const side = { ...current[source] };
      if (field === 'max_processor_state') side.max_processor_state = value ?? 5;
      else if (field === 'boost_mode') side.boost_mode = value ?? 0;
      else side.max_mhz = value;
      return source === 'ac' ? { ...current, ac: side } : { ...current, dc: side };
    });
  };

  const requestFromDraft = (): CpuPolicyUpdate | null => {
    if (!draft) return null;
    const side = (values: CpuSideValues): CpuSideUpdate => {
      const update: CpuSideUpdate = {
        max_processor_state: values.max_processor_state,
        boost_mode: values.boost_mode,
      };
      if (values.max_mhz !== null) update.max_mhz = values.max_mhz;
      return update;
    };
    return { ac: side(draft.ac), dc: side(draft.dc) };
  };

  const run = async (command: 'apply_cpu_policy' | 'reset_cpu_policy_session', request?: CpuPolicyUpdate) => {
    if (busy) return;
    setBusy(true);
    setError('');
    setMessage('');
    try {
      const args =
        command === 'apply_cpu_policy' && request
          ? { request, expectedPlanGuid: snapshot?.plan_guid ?? null }
          : undefined;
      const outcome = await invoke<CpuPolicyOutcome>(command, args);
      setMessage(describeOutcome(outcome));
      // Never treat the write response as proof: always re-query Windows.
      await load();
    } catch (e) {
      setError(String(e));
      await load().catch(() => {});
    } finally {
      setBusy(false);
    }
  };

  if (!snapshot || !draft) {
    return (
      <div className="glass-card cpu-policy-card cpu-policy-loading" role="status">
        {error || '正在读取 Windows 活动电源计划与处理器策略…'}
      </div>
    );
  }

  const mhzSupported = snapshot.ac.max_mhz !== null && snapshot.dc.max_mhz !== null;

  const renderSide = (source: 'ac' | 'dc', title: string) => {
    const values = draft[source];
    const original = snapshot[source];
    return (
      <section className="cpu-policy-side" aria-label={title}>
        <div className="cpu-policy-side-title">{title}</div>

        <label className="cpu-policy-field">
          <span className="cpu-policy-field-label">
            Windows 最大处理器状态 (%) <span className="cpu-policy-policy-tag">OS 策略</span>
          </span>
          <span className="cpu-policy-field-row">
            <input
              className="cpu-policy-range"
              type="range"
              min={5}
              max={100}
              step={1}
              value={values.max_processor_state}
              disabled={busy}
              onChange={(e) => changeSide(source, 'max_processor_state', Number(e.target.value))}
            />
            <span className="cpu-policy-value">{values.max_processor_state}%</span>
          </span>
          <span className="cpu-policy-field-desc">
            会话快照 {original.max_processor_state}%；修改仅作为待应用草稿，不预乐观显示成功。
          </span>
        </label>

        <label className="cpu-policy-field">
          <span className="cpu-policy-field-label">
            Windows 处理器性能提升模式 <span className="cpu-policy-policy-tag">OS 策略</span>
          </span>
          <select
            className="rev-select cpu-policy-select"
            value={values.boost_mode}
            disabled={busy}
            onChange={(e) => changeSide(source, 'boost_mode', Number(e.target.value))}
          >
            {BOOST_MODES.map((mode) => (
              <option key={mode.value} value={mode.value}>
                {mode.label}
              </option>
            ))}
          </select>
          <span className="cpu-policy-field-desc">会话快照 {original.boost_mode}；由 Windows 电源策略解释。</span>
        </label>

        {mhzSupported && (
          <label className="cpu-policy-field">
            <span className="cpu-policy-field-label">
              Windows 最大处理器频率策略 (MHz) <span className="cpu-policy-policy-tag">OS 策略</span>
            </span>
            <input
              className="cpu-policy-number"
              type="number"
              min={0}
              max={7000}
              step={100}
              value={values.max_mhz ?? 0}
              disabled={busy}
              onChange={(e) => changeSide(source, 'max_mhz', Number(e.target.value))}
            />
            <span className="cpu-policy-field-desc">
              0 = 不限制。仅为 Windows 策略值，不保证硅片实际频率，也不代表 TDP/电压。
            </span>
          </label>
        )}
      </section>
    );
  };

  const request = requestFromDraft();

  return (
    <details className="glass-card cpu-policy-card">
      <summary className="cpu-policy-summary">
        <span className="cpu-policy-title">
          <Cpu className="w-4 h-4" />
          Windows CPU 电源策略 (AC / 电池)
        </span>
        <span className="cpu-policy-summary-values">
          AC {snapshot.ac.max_processor_state}% · boost {snapshot.ac.boost_mode}
          {snapshot.ac.max_mhz !== null ? ` · ${snapshot.ac.max_mhz || '不限'} MHz 策略` : ''}
          <span className="cpu-policy-summary-separator">|</span>
          DC {snapshot.dc.max_processor_state}% · boost {snapshot.dc.boost_mode}
          {snapshot.dc.max_mhz !== null ? ` · ${snapshot.dc.max_mhz || '不限'} MHz 策略` : ''}
        </span>
        <span className="cpu-policy-expand">展开 / 收起</span>
      </summary>

      <div className="cpu-policy-body">
        <div className="cpu-policy-head">
          <div className="cpu-policy-desc">
            精确作用于活动 Windows 电源计划；保存值后会重新应用同一计划使其生效，不切换到其他计划，也不写 EC/TDP/电压。独立的 Windows 能源叠加会在激活前后回读，仅在已知原值时恢复。
          </div>
          <div className="cpu-policy-plan">
            <span>活动计划 GUID</span>
            <code>{snapshot.plan_guid}</code>
          </div>
        </div>

        <div className="cpu-policy-sides">
          {renderSide('ac', '已接通电源 (AC)')}
          {renderSide('dc', '使用电池 (DC)')}
        </div>

        {error && <div className="cpu-policy-error" role="alert">{error}</div>}
        {message && <div className="cpu-policy-result" role="status">{message}</div>}

        <div className="cpu-policy-actions">
          <button
            type="button"
            className="toggle-btn active-indigo"
            disabled={busy || !request}
            onClick={() => request && run('apply_cpu_policy', request)}
          >
            {busy ? '正在应用并回读…' : '应用 Windows CPU 策略'}
          </button>
          <button
            type="button"
            className="toggle-btn"
            disabled={busy}
            onClick={() => run('reset_cpu_policy_session')}
          >
            <RotateCcw className="w-3.5 h-3.5" />
            恢复到本次会话快照
          </button>
        </div>
      </div>
    </details>
  );
};
