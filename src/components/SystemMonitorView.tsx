import React, { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Activity, Cpu, Fan, Gauge, HardDrive, Zap } from 'lucide-react';
import { HardwareState, SystemStats } from '../types/hardware';
import { RevTickBar, MeterTone } from './ui/RevTickBar';
import { setVendorBusActive } from '../state/vendorBus';

interface VendorMessagePayload {
  topic: string;
  payload: string;
}

interface VendorResponse {
  messages: VendorMessagePayload[];
  elapsed_ms: number;
}

interface SystemMonitorViewProps {
  status: HardwareState;
  /** 机型风扇物理上限（来自 capabilities.max_fan_rpm），用于转速刻度换算 */
  maxFanRpm?: number;
  /** 该机型 manifest 是否声明了风扇调速能力（fan_custom_curve / set_fan 动作） */
  fanControllable?: boolean;
}

const POLL_INTERVAL_MS = 1200;
const DEFAULT_FAN_CEILING = 6000;

/** 浏览器预览 / 后端尚未编译新命令时的演示快照 */
const DEMO_STATS: SystemStats = {
  mem: { total_mb: 32051, used_mb: 27136, free_mb: 4915, usage: 85, percent_load: 85 },
  disks: [
    { letter: 'C:', label: '系统', fs: 'NTFS', total_gb: 299.0, used_gb: 220.1, free_gb: 78.9, usage: 74, is_system: true },
    { letter: 'D:', label: '数据', fs: 'NTFS', total_gb: 652.9, used_gb: 174.5, free_gb: 478.4, usage: 27, is_system: false },
  ],
};

const clampPct = (value: number) => (Number.isFinite(value) ? Math.min(100, Math.max(0, value)) : 0);
const toGb = (mb: number) => (Number.isFinite(mb) ? Math.max(0, mb) / 1024 : 0);
const fmtGb = (value: number) => value.toFixed(1);
const meterTone = (pct: number): MeterTone => (pct >= 96 ? 'hot' : pct >= 88 ? 'warn' : 'ok');
const fmtWatt = (watts?: number) => (watts !== undefined && watts > 0 ? `${watts.toFixed(1)} W` : '-- W');

export const SystemMonitorView: React.FC<SystemMonitorViewProps> = ({
  status,
  maxFanRpm,
  fanControllable = false,
}) => {
  const [stats, setStats] = useState<SystemStats | null>(null);
  const [isDemo, setIsDemo] = useState(false);

  useEffect(() => {
    let alive = true;
    const sample = async () => {
      try {
        const snapshot = await invoke<SystemStats>('get_system_stats');
        if (!alive) return;
        setStats(snapshot);
        setIsDemo(false);
      } catch {
        if (!alive) return;
        setStats((prev) => prev ?? DEMO_STATS);
        setIsDemo(true);
      }
    };
    void sample();
    const timer = window.setInterval(() => {
      void sample();
    }, POLL_INTERVAL_MS);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  const [vendorOnline, setVendorOnline] = useState(false);
  const [vendorBoost, setVendorBoost] = useState<boolean | null>(null);
  const [vendorBusy, setVendorBusy] = useState(false);
  const [vendorLatency, setVendorLatency] = useState<number | null>(null);
  const [vendorPersist, setVendorPersist] = useState<boolean>(() => {
    try {
      return window.localStorage.getItem('openrevo.vendorBusPersist') === '1';
    } catch {
      return false;
    }
  });
  const vendorPending = useRef(false);
  const vendorOnlineRef = useRef(false);

  const applyVendorResponse = (response: VendorResponse) => {
    setVendorLatency(response.elapsed_ms);
    const statusMessage = response.messages.find((item) => item.topic === 'Fan/Status');
    if (statusMessage) {
      try {
        const parsed = JSON.parse(statusMessage.payload) as { FanBoostEnable?: string };
        setVendorBoost(parsed.FanBoostEnable === '1');
      } catch {
        setVendorBoost(null);
      }
    }
    vendorOnlineRef.current = true;
    setVendorOnline(true);
    setVendorBusActive(true);
  };

  const refreshVendorBus = async (): Promise<boolean> => {
    if (vendorPending.current) return vendorOnlineRef.current;
    vendorPending.current = true;
    try {
      const response = await invoke<VendorResponse>('vendor_bus_probe');
      applyVendorResponse(response);
      return true;
    } catch {
      vendorOnlineRef.current = false;
      setVendorOnline(false);
      setVendorBoost(null);
      setVendorBusActive(false);
      return false;
    } finally {
      vendorPending.current = false;
    }
  };

  const setVendorPersistMode = async (enabled: boolean) => {
    setVendorPersist(enabled);
    try {
      window.localStorage.setItem('openrevo.vendorBusPersist', enabled ? '1' : '0');
    } catch {
      /* storage may be unavailable */
    }
    try {
      await invoke<boolean>('vendor_set_service_persistent', { enabled });
      if (enabled) {
        await invoke<boolean>('vendor_ensure_service');
        await refreshVendorBus();
      }
    } catch (error) {
      console.error('Failed to switch vendor service mode:', error);
    }
  };

  const connectVendorService = async () => {
    if (vendorBusy) return;
    setVendorBusy(true);
    try {
      const started = await invoke<boolean>('vendor_ensure_service');
      if (started) {
        await refreshVendorBus();
      }
    } catch {
      vendorOnlineRef.current = false;
      setVendorOnline(false);
    } finally {
      setVendorBusy(false);
    }
  };

  const toggleVendorBoost = async () => {
    if (vendorBusy) return;
    setVendorBusy(true);
    try {
      await invoke<VendorResponse>('vendor_fan_boost', { enabled: !(vendorBoost ?? false) });
      await refreshVendorBus();
    } catch {
      vendorOnlineRef.current = false;
      setVendorOnline(false);
    } finally {
      setVendorBusy(false);
    }
  };

  useEffect(() => {
    let stopped = false;
    let timer = 0;
    const tick = async () => {
      if (stopped) return;
      if (document.visibilityState === 'visible') {
        await refreshVendorBus();
      }
      if (stopped) return;
      // 在线 6 s 一轮，离线退避到 30 s，避免空转与无谓的服务调用
      timer = window.setTimeout(tick, vendorOnlineRef.current ? 6000 : 30000);
    };
    void (async () => {
      if (vendorPersist) {
        try {
          await invoke<boolean>('vendor_ensure_service');
        } catch {
          /* best effort */
        }
        await refreshVendorBus();
      }
    })();
    void tick();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
      setVendorBusActive(false);
    };
  }, []);

  const cpuUsage = clampPct(status.cpu_usage ?? 0);
  const cpuFreq = status.cpu_freq ?? 0;
  const cpuBase = status.cpu_base_mhz && status.cpu_base_mhz > 0 ? status.cpu_base_mhz : 0;
  const cpuTemp = status.cpu_temp ?? 0;
  const freqCeiling = Math.max(3600, cpuBase * 1.8);
  const freqPct = clampPct((cpuFreq / freqCeiling) * 100);
  const tempPct = clampPct(((cpuTemp - 30) / 70) * 100);

  const gpuUsage = clampPct(status.gpu_usage ?? 0);
  const gpuVramUsed = status.gpu_vram_used_mb ?? 0;
  const gpuVramTotal = status.gpu_vram_mb ?? 0;
  const gpuVramPct = gpuVramTotal > 0 ? clampPct((gpuVramUsed / gpuVramTotal) * 100) : 0;
  const gpuTemp = status.gpu_temp ?? 0;
  // 核显机型（如 Ryzen AI 9 HX 365 + Radeon 880M）没有 NVAPI 显存总量，隐藏显存行
  const hasDedicatedVram = gpuVramTotal > 0;

  const mem = stats?.mem;
  const memPct = clampPct(mem?.usage ?? 0);
  const disks = stats?.disks ?? [];

  const fanCeiling = maxFanRpm && maxFanRpm > 0 ? maxFanRpm : DEFAULT_FAN_CEILING;
  const cpuFan = status.cpu_fan_rpm ?? 0;
  const gpuFan = status.gpu_fan_rpm ?? 0;
  // 无独显的小型机（XINGYAO / Ryzen AI 9 这类）没有 GPU 风扇通道，隐藏该行避免 "-- RPM" 噪音
  const hasGpuFanChannel = hasDedicatedVram || gpuFan > 0;
  const cpuFanPct = clampPct((cpuFan / fanCeiling) * 100);
  const gpuFanPct = clampPct((gpuFan / fanCeiling) * 100);

  return (
    <div className="sys-monitor">
      <div className="sys-monitor-head">
        <div className="sys-monitor-title">
          <Activity size={16} />
          <span>系统监控</span>
        </div>
        <div className="sys-monitor-badges">
          <span className={isDemo ? 'sys-monitor-badge is-demo' : 'sys-monitor-badge'}>
            <span className="live-dot" />
            {isDemo ? '演示数据' : 'LIVE · 1.2s'}
          </span>
        </div>
      </div>

      <div className="sys-monitor-grid">
        <section className="mon-card">
          <span className="oem-bracket-tl" />
          <span className="oem-bracket-tr" />
          <span className="oem-bracket-bl" />
          <span className="oem-bracket-br" />
          <div className="mon-card-head">
            <div className="mon-card-title">
              <Cpu />
              <span>处理器</span>
            </div>
            <div className="mon-card-tag">
              <Zap size={11} />
              {fmtWatt(status.cpu_power_w)}
            </div>
          </div>

          <div className="mon-row">
            <div className="mon-row-head">
              <span className="mon-row-label">使用率</span>
              <span className="mon-row-value">{Math.round(cpuUsage)} %</span>
            </div>
            <RevTickBar value={cpuUsage} tone={meterTone(cpuUsage)} />
          </div>

          <div className="mon-row">
            <div className="mon-row-head">
              <span className="mon-row-label">频率</span>
              <span className="mon-row-value">{cpuFreq} MHz</span>
            </div>
            <RevTickBar value={freqPct} />
          </div>

          <div className="mon-plain">
            <span className="mon-plain-label">基准频率</span>
            <span className="mon-plain-value">{cpuBase > 0 ? `${cpuBase} MHz` : '-- MHz'}</span>
          </div>

          <div className="mon-plain">
            <span className="mon-plain-label">温度</span>
            <span className={cpuTemp > 0 ? `mon-plain-value is-${meterTone(tempPct)}` : 'mon-plain-value'}>
              {cpuTemp > 0 ? `${cpuTemp} °C` : '-- °C'}
            </span>
          </div>
        </section>

        <section className="mon-card">
          <span className="oem-bracket-tl" />
          <span className="oem-bracket-tr" />
          <span className="oem-bracket-bl" />
          <span className="oem-bracket-br" />
          <div className="mon-card-head">
            <div className="mon-card-title">
              <Gauge />
              <span>显卡</span>
            </div>
            <div className="mon-card-tag">
              {hasDedicatedVram ? (
                <>
                  <Zap size={11} />
                  {fmtWatt(status.gpu_power_w)}
                </>
              ) : (
                '核显 · WDDM'
              )}
            </div>
          </div>

          <div className="mon-row">
            <div className="mon-row-head">
              <span className="mon-row-label">使用率</span>
              <span className="mon-row-value">{Math.round(gpuUsage)} %</span>
            </div>
            <RevTickBar value={gpuUsage} tone={meterTone(gpuUsage)} />
          </div>

          {hasDedicatedVram && (
            <div className="mon-row">
              <div className="mon-row-head">
                <span className="mon-row-label">显存</span>
                <span className="mon-row-value">
                  {fmtGb(toGb(gpuVramUsed))} GB/{fmtGb(toGb(gpuVramTotal))} GB
                </span>
              </div>
              <RevTickBar value={gpuVramPct} />
            </div>
          )}

          {(status.gpu_core_freq ?? 0) > 0 && (
            <div className="mon-plain">
              <span className="mon-plain-label">核心频率</span>
              <span className="mon-plain-value">{status.gpu_core_freq} MHz</span>
            </div>
          )}

          {gpuTemp > 0 && (
            <div className="mon-plain">
              <span className="mon-plain-label">温度</span>
              <span className={`mon-plain-value is-${meterTone(tempPct)}`}>{gpuTemp} °C</span>
            </div>
          )}

          {!hasDedicatedVram && (
            <div className="mon-plain">
              <span className="mon-plain-label">适配器</span>
              <span className="mon-plain-value">{status.gpu_name || '核显 / 共享显存'}</span>
            </div>
          )}
        </section>

        <section className="mon-card">
          <span className="oem-bracket-tl" />
          <span className="oem-bracket-tr" />
          <span className="oem-bracket-bl" />
          <span className="oem-bracket-br" />
          <div className="mon-card-head">
            <div className="mon-card-title">
              <HardDrive />
              <span>存储</span>
            </div>
            <div className="mon-card-tag">{disks.length} 个固定卷</div>
          </div>

          <div className="mon-row">
            <div className="mon-row-head">
              <span className="mon-row-label">RAM</span>
              <span className="mon-row-value">
                {mem ? `${fmtGb(toGb(mem.used_mb))} GB/${fmtGb(toGb(mem.total_mb))} GB` : '-- GB/-- GB'}
              </span>
            </div>
            <RevTickBar value={memPct} tone={meterTone(memPct)} />
          </div>

          {disks.length === 0 && (
            <div className="mon-plain">
              <span className="mon-plain-label">磁盘</span>
              <span className="mon-plain-value">采集中…</span>
            </div>
          )}

          {disks.map((disk) => (
            <div className="mon-row" key={disk.letter}>
              <div className="mon-row-head">
                <span className="mon-disk-label">
                  <span className={disk.is_system ? 'mon-disk-letter is-system' : 'mon-disk-letter'}>{disk.letter}</span>
                  {disk.label || disk.fs || '本地磁盘'}
                </span>
                <span className="mon-row-value">
                  {fmtGb(disk.used_gb)} GB/{fmtGb(disk.total_gb)} GB
                </span>
              </div>
              <RevTickBar value={disk.usage} tone={meterTone(disk.usage)} />
            </div>
          ))}
        </section>

        <section className="mon-card">
          <span className="oem-bracket-tl" />
          <span className="oem-bracket-tr" />
          <span className="oem-bracket-bl" />
          <span className="oem-bracket-br" />
          <div className="mon-card-head">
            <div className="mon-card-title">
              <Fan />
              <span>风扇转速</span>
            </div>
            <div className="mon-card-tag">
              {cpuFan === 0 && gpuFan === 0 ? 'EC 未映射' : `上限 ${fanCeiling} RPM`}
            </div>
          </div>

          <div className="mon-row">
            <div className="mon-row-head">
              <span className="mon-row-label">处理器</span>
              <span className="mon-row-value">
                {cpuFan > 0 ? `${cpuFan} RPM · ${Math.round(cpuFanPct)}%` : '-- RPM'}
              </span>
            </div>
            <RevTickBar value={cpuFanPct} />
          </div>

          {hasGpuFanChannel && (
            <div className="mon-row">
              <div className="mon-row-head">
                <span className="mon-row-label">显卡</span>
                <span className="mon-row-value">{gpuFan > 0 ? `${gpuFan} RPM` : '-- RPM'}</span>
              </div>
              <RevTickBar value={gpuFanPct} />
            </div>
          )}

          <div className="mon-plain">
            <span className="mon-plain-label">散热策略</span>
            <span className="mon-plain-value">{status.fan_boost ? '强冷 / 全速' : '智能曲线'}</span>
          </div>

          <div className="mon-plain">
            <span className="mon-plain-label">厂商通道</span>
            <span className={vendorOnline ? 'mon-plain-value is-ok' : 'mon-plain-value'}>
              {vendorOnline
                ? `在线 · localhost:13688 · ${vendorLatency ?? '--'} ms`
                : '离线（GCUBridge 未运行）'}
            </span>
          </div>

          {vendorOnline && (
            <div className="mon-plain">
              <span className="mon-plain-label">强冷</span>
              <button
                type="button"
                className={`toggle-btn ${vendorBoost ? 'active-emerald' : ''}`}
                onClick={toggleVendorBoost}
                disabled={vendorBusy}
              >
                {vendorBoost ? '已开启' : '已关闭'}
              </button>
            </div>
          )}

          {!vendorOnline && (
            <div className="mon-plain">
              <span className="mon-plain-label">连接</span>
              <button
                type="button"
                className="toggle-btn"
                onClick={connectVendorService}
                disabled={vendorBusy}
              >
                {vendorBusy ? '启动中…' : '连接厂商通道'}
              </button>
              <button
                type="button"
                className={'toggle-btn ' + (vendorPersist ? 'active-emerald' : '')}
                onClick={() => setVendorPersistMode(!vendorPersist)}
                disabled={vendorBusy}
                title="让 GCUBridge 随系统自动启动，厂商通道不再被回收"
              >
                {vendorPersist ? '常开已启用' : '常开'}
              </button>
            </div>
          )}

          <div className="mon-plain">
            <span className="mon-plain-label">转速调节</span>
            <span className="mon-plain-value">
              {vendorOnline
                ? '厂商总线（强冷）'
                : fanControllable
                  ? '本机型可调（电源管理 → 风扇曲线）'
                  : 'EC 自动（未接入调速协议）'}
            </span>
          </div>
        </section>
      </div>

      <div className="sys-monitor-foot">
        <span>采样间隔 1.2 s · 数据源 CPU/RAM/卷容量</span>
        <span>{status.model_name || 'OpenRevo'} · {isDemo ? 'DEMO' : 'LIVE'}</span>
      </div>
    </div>
  );
};
