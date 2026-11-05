import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface VendorServiceState {
  present: boolean;
  selected: boolean;
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
  const pending = useRef(false);

  const read = useCallback(async (command: string, args?: Record<string, unknown>) => {
    const res = await invoke<VendorResponse>(command, args);
    const message = res.messages.find((item) => item.topic === topic);
    const value = message ? parse(JSON.parse(message.payload)) : null;
    if (value === null) throw new Error(`未收到可确认的 ${topic} 状态`);
    return { value, latency: res.elapsed_ms };
  }, [topic, parse]);

  useEffect(() => {
    let disposed = false;
    let timer = 0;
    const tick = async () => {
      if (disposed) return;
      let interval = 30_000;
      if (document.visibilityState === 'visible' && !pending.current) {
        pending.current = true;
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
              if (disposed || !nextService.selected || !nextService.auto_start) throw failure;
              setSnapshot(null);
              await invoke('vendor_recover_service');
              if (disposed) return;
              next = await read(probe);
            }
            if (!disposed) setSnapshot(next);
            interval = 15_000;
          } else {
            setSnapshot(null);
          }
        } catch {
          if (!disposed) setSnapshot(null);
        } finally {
          pending.current = false;
          if (!disposed) setChecking(false);
        }
      }
      if (!disposed) timer = window.setTimeout(tick, interval);
    };
    void tick();
    return () => {
      disposed = true;
      window.clearTimeout(timer);
    };
  }, [probe, read]);

  const run = async (work: () => Promise<void>) => {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setError('');
    try {
      await work();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
      pending.current = false;
    }
  };

  return {
    service,
    snapshot,
    busy,
    checking,
    error,
    connect: (persistent?: boolean) => run(async () => {
      setService(await invoke<VendorServiceState>('vendor_connect_service', { persistent }));
      if (persistent !== false) {
        try {
          setSnapshot(await read(probe));
        } catch (failure) {
          setSnapshot(null);
          throw failure;
        }
      }
    }),
    write: (command: string, args: Record<string, unknown>) => run(async () => {
      setSnapshot(await read(command, args));
    }),
  };
}
