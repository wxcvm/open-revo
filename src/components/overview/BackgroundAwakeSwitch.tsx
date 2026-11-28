import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { MoonStar } from 'lucide-react';

type AwakeStatus = { enabled: boolean; remaining_ms: number | null };

export function BackgroundAwakeSwitch() {
  const [active, setActive] = useState<boolean | null>(null);
  const [minutes, setMinutes] = useState(0);
  const [deadline, setDeadline] = useState<number | null>(null);
  const [seconds, setSeconds] = useState(0);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const inFlight = useRef(false);
  const revision = useRef(0);
  const applyStatus = ({ enabled, remaining_ms }: AwakeStatus) => {
    setActive(enabled);
    setDeadline(enabled && remaining_ms != null ? performance.now() + remaining_ms : null);
    setSeconds(enabled && remaining_ms != null ? Math.ceil(remaining_ms / 1000) : 0);
  };

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    const read = async () => {
      const seen = revision.current;
      try {
        const status = await invoke<AwakeStatus>('get_background_awake_status');
        if (!disposed && seen === revision.current) {
          applyStatus(status);
          setError('');
        }
      } catch {
        if (!disposed && seen === revision.current) setError('未能读取后台保活状态');
      }
    };
    void listen<AwakeStatus>('background-awake-status-changed', ({ payload }) => {
      if (!disposed) {
        revision.current++;
        applyStatus(payload);
      }
    }).then((fn) => {
      if (disposed) fn();
      else unsubscribe = fn;
      return read();
    }).catch(() => { void read(); });
    window.addEventListener('focus', read);
    return () => {
      disposed = true;
      unsubscribe?.();
      window.removeEventListener('focus', read);
    };
  }, []);

  useEffect(() => {
    if (deadline === null) return;
    const timer = window.setInterval(() => {
      setSeconds(Math.max(0, Math.ceil((deadline - performance.now()) / 1000)));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [deadline]);

  const toggle = async () => {
    if (inFlight.current || active === null) return;
    inFlight.current = true;
    const seen = ++revision.current;
    setPending(true);
    setError('');
    try {
      await invoke<boolean>('set_background_awake', { enable: !active, minutes: !active && minutes > 0 ? minutes : null });
      const status = await invoke<AwakeStatus>('get_background_awake_status');
      if (seen === revision.current) applyStatus(status);
    } catch (cause) {
      setError(typeof cause === 'string' ? cause : 'Windows 未接受后台保活请求');
    } finally {
      inFlight.current = false;
      setPending(false);
    }
  };

  const countdown = [Math.floor(seconds / 3600), Math.floor(seconds / 60) % 60, seconds % 60]
    .map(part => String(part).padStart(2, '0')).join(':');
  return (
    <div className="awake-switch-wrap">
    <button
      type="button"
      className={`oem-switch-card ${active ? 'active' : ''}`}
      aria-label="后台保活"
      aria-pressed={active ?? undefined}
      disabled={pending || active === null}
      onClick={toggle}
      title="为 ARTEX 等后台任务请求阻止空闲睡眠；允许系统熄屏和锁屏，不阻止手动睡眠。长时间运行建议接电。"
    >
      <span className="oem-bracket-tl" /><span className="oem-bracket-tr" />
      <span className="oem-bracket-bl" /><span className="oem-bracket-br" />
      <span className="oem-icon-badge"><MoonStar className="w-4 h-4" /></span>
      <span className="oem-switch-title">后台保活</span>
      <small>{error ? <span role="alert">{error}</span> : pending ? '正在请求…'
        : active ? (deadline === null ? '已请求 · 可熄屏' : countdown)
        : active === null ? '读取中…' : '防空闲睡眠 · 可熄屏'}</small>
    </button>
    <select aria-label="后台保活时长" value={minutes} disabled={pending || active !== false}
      onChange={event => setMinutes(Number(event.target.value))}>
      <option value={0}>手动关闭</option>
      <option value={30}>30 分钟</option>
      <option value={120}>2 小时</option>
      <option value={480}>8 小时</option>
    </select>
    </div>
  );
}
