import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { MoonStar } from 'lucide-react';

export function BackgroundAwakeSwitch() {
  const [active, setActive] = useState<boolean | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const inFlight = useRef(false);
  const revision = useRef(0);

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    const read = async () => {
      const seen = revision.current;
      try {
        const enabled = await invoke<boolean>('get_background_awake');
        if (!disposed && seen === revision.current) {
          setActive(enabled);
          setError('');
        }
      } catch {
        if (!disposed && seen === revision.current) setError('未能读取后台保活状态');
      }
    };
    void listen<boolean>('background-awake-state-changed', ({ payload }) => {
      if (!disposed) {
        revision.current++;
        setActive(payload);
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

  const toggle = async () => {
    if (inFlight.current || active === null) return;
    inFlight.current = true;
    const seen = ++revision.current;
    setPending(true);
    setError('');
    try {
      const enabled = await invoke<boolean>('set_background_awake', { enable: !active });
      if (seen === revision.current) setActive(enabled);
    } catch (cause) {
      setError(typeof cause === 'string' ? cause : 'Windows 未接受后台保活请求');
    } finally {
      inFlight.current = false;
      setPending(false);
    }
  };

  return (
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
      <small>{error ? <span role="alert">{error}</span> : pending ? '正在请求…' : active ? '已请求 · 可熄屏' : active === null ? '读取中…' : '防空闲睡眠 · 可熄屏'}</small>
    </button>
  );
}
