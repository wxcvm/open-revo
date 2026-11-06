import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface VendorServiceState {
  present: boolean;
  selected: boolean;
  running: boolean;
  auto_start: boolean;
}

interface VendorResponse {
  messages: { topic: string; payload: string }[];
  elapsed_ms: number;
}

export interface KeyboardStatus {
  timerSec: number;
  backlight: boolean | null;
}

export const parseFanBoost = (json: Record<string, unknown>): boolean | null => {
  const value = json.FanBoostEnable;
  if (value === true || value === 1 || value === '1') return true;
  if (value === false || value === 0 || value === '0') return false;
  return null;
};

export const parseKeyboard = (json: Record<string, unknown>): KeyboardStatus | null => {
  const value = json.CloseTimer;
  const timerSec = typeof value === 'string' && /^\d+$/.test(value) ? Number(value) : value;
  if (typeof timerSec !== 'number' || !Number.isSafeInteger(timerSec) || timerSec < 0) return null;
  const backlight = json.SingleColorKBBL;
  return {
    timerSec,
    backlight: backlight === 'SINGLE_COLOR_KBBL_STATUS_ON' ? true
      : backlight === 'SINGLE_COLOR_KBBL_STATUS_OFF' ? false : null,
  };
};

export function useVendorControl<T>(
  probe: string,
  topic: string,
  parse: (json: Record<string, unknown>) => T | null,
) {
  const [service, setService] = useState<VendorServiceState | null>(null);
  const [snapshot, setSnapshot] = useState<{ value: T; latency: number } | null>(null);
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState('');
  const pending = useRef<symbol | null>(null);
  const lifetime = useRef(0);

  const read = useCallback(async (command: string, args?: Record<string, unknown>) => {
    const res = await invoke<VendorResponse>(command, args);
    const message = res.messages.find((item) => item.topic === topic);
    const value = message ? parse(JSON.parse(message.payload)) : null;
    if (value === null) throw new Error(`未收到可确认的 ${topic} 状态`);
    return { value, latency: res.elapsed_ms };
  }, [topic, parse]);

  useEffect(() => {
    const currentLifetime = ++lifetime.current;
    pending.current = null;
    setBusy(false);
    setChecking(false);
    let disposed = false;
    let timer = 0;
    const tick = async () => {
      if (disposed) return;
      let interval = 30_000;
      if (document.visibilityState === 'visible' && !pending.current) {
        const operation = Symbol();
        pending.current = operation;
        setChecking(true);
        try {
          const nextService = await invoke<VendorServiceState>('vendor_service_state');
          if (disposed) return;
          setService(nextService);
          if (nextService.present) {
            let next;
            try {
              next = await read(probe);
            } catch (failure) {
              // A running backend with a missing reply must not be restarted by polling.
              if (disposed || nextService.running !== false || !nextService.selected || !nextService.auto_start) throw failure;
              setSnapshot(null);
              await invoke('vendor_recover_service');
              if (disposed) return;
              next = await read(probe);
            }
            if (!disposed) { setSnapshot(next); setError(''); }
            interval = 15_000;
          } else {
            setSnapshot(null);
          }
        } catch {
          if (!disposed) setSnapshot(null);
        } finally {
          if (pending.current === operation) {
            pending.current = null;
            if (!disposed) setChecking(false);
          }
        }
      }
      if (!disposed) timer = window.setTimeout(tick, interval);
    };
    void tick();
    return () => {
      disposed = true;
      if (lifetime.current === currentLifetime) ++lifetime.current;
      window.clearTimeout(timer);
    };
  }, [probe, read]);

  // A broker can accept MQTT before its separate hardware worker is ready.
  // Only explicit connect/restart waits here; normal refresh stays bounded.
  const readWhenReady = async (active: () => boolean) => {
    const deadline = performance.now() + 15_000;
    while (active()) {
      try {
        return await read(probe);
      } catch (failure) {
        if (!active()) return null;
        if (performance.now() >= deadline) throw failure;
        await new Promise((resolve) => window.setTimeout(resolve, 300));
      }
    }
    return null;
  };

  const run = async (work: (active: () => boolean) => Promise<void>) => {
    if (pending.current) return;
    const operation = Symbol();
    const currentLifetime = lifetime.current;
    const active = () => lifetime.current === currentLifetime;
    pending.current = operation;
    setBusy(true);
    setError('');
    try {
      await work(active);
    } catch (failure) {
      if (active()) setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      // An old StrictMode/page lifetime must not clear a newer operation's lock.
      if (pending.current === operation) {
        pending.current = null;
        if (active()) setBusy(false);
      }
    }
  };

  return {
    service,
    snapshot,
    busy,
    checking,
    error,
    connect: (persistent?: boolean) => run(async (active) => {
      const nextService = await invoke<VendorServiceState>('vendor_connect_service', { persistent });
      if (!active()) return;
      setService(nextService);
      if (persistent !== false) {
        try {
          const next = await readWhenReady(active);
          if (active()) setSnapshot(next);
        } catch (failure) {
          if (active()) setSnapshot(null);
          throw failure;
        }
      }
    }),
    restart: () => run(async (active) => {
      setSnapshot(null);
      const nextService = await invoke<VendorServiceState>('vendor_restart_service');
      if (!active()) return;
      setService(nextService);
      const next = await readWhenReady(active);
      if (active()) setSnapshot(next);
    }),
    write: (command: string, args: Record<string, unknown>) => run(async (active) => {
      const next = await read(command, args);
      if (active()) setSnapshot(next);
    }),
  };
}
