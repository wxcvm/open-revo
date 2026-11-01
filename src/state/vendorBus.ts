// OpenRevo - 厂商总线运行态（跨页共享的轻量订阅）
// 由监控页在连上 AiStone 总线时置位，用于抑制“接管官方控制台”提示的反复出现。

let active = false;
const listeners = new Set<(value: boolean) => void>();

export const getVendorBusActive = (): boolean => active;

export const setVendorBusActive = (value: boolean): void => {
  if (active === value) return;
  active = value;
  listeners.forEach((listener) => listener(value));
};

export const subscribeVendorBus = (listener: (value: boolean) => void): (() => void) => {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
};
