import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Activity, Cpu, Fan, Gauge, HardDrive, Zap } from 'lucide-react';
import { HardwareState, SystemStats } from '../types/hardware';
import { RevTickBar, MeterTone } from './ui/RevTickBar';
import { parseFanBoost, useVendorControl } from '../state/vendorControl';
import { TelemetryTracePanel } from './monitor/TelemetryTracePanel';
import { ResourceOverview } from './monitor/ResourceOverview';

interface SystemMonitorViewProps {
  status: HardwareState;
  /** 机型风扇物理上限（来自 capabilities.max_fan_rpm），用于转速刻度换算 */
  maxFanRpm?: number;
  /** 该机型 manifest 是否声明了风扇调速能力（fan_custom_curve / set_fan 动作） */
  fanControllable?: boolean;
  nativeControl?: boolean;
  discreteGpu?: boolean;
  resourceView: boolean;
  onResourceView: (open: boolean) => void;
}

const POLL_INTERVAL_MS = 1200;
const clampPct = (value: number) => (Number.isFinite(value) ? Math.min(100, Math.max(0, value)) : 0);
const toGb = (mb: number) => (Number.isFinite(mb) ? Math.max(0, mb) / 1024 : 0);
const fmtGb = (value: number) => value.toFixed(1);
const meterTone = (pct: number): MeterTone => (pct >= 96 ? 'hot' : pct >= 88 ? 'warn' : 'ok');
const temperatureTone = (celsius: number): MeterTone => celsius >= 95 ? 'hot' : celsius >= 85 ? 'warn' : 'ok';
const fmtWatt = (watts?: number) => (watts !== undefined && watts > 0 ? `${watts.toFixed(1)} W` : '-- W');

export const SystemMonitorView: React.FC<SystemMonitorViewProps> = ({
  status,
  maxFanRpm,
  fanControllable = false,
  nativeControl = false,
  discreteGpu,
  resourceView,
  onResourceView,
}) => {
  const [stats, setStats] = useState<SystemStats | null>(null);
  const [statsUnavailable, setStatsUnavailable] = useState(false);

  useEffect(() => {
    if (resourceView) return;
    let alive = true;
    let timer: number | undefined;
    const sample = async () => {
      try {
        const snapshot = await invoke<SystemStats | null>('get_system_stats');
        if (!alive) return;
        setStats(snapshot);
        setStatsUnavailable(!snapshot || (!snapshot.mem?.total_mb && !snapshot.disks?.length));
      } catch {
        if (!alive) return;
        setStats(null);
        setStatsUnavailable(true);
      } finally {
        // A slow native read must not pile up overlapping IPC calls.
        if (alive) timer = window.setTimeout(sample, POLL_INTERVAL_MS);
      }
    };
    void sample();
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [resourceView]);

  const vendor = useVendorControl('vendor_probe_fan', 'Fan/Status', parseFanBoost, !nativeControl && !resourceView);
  const vendorBoost = vendor.snapshot?.value;
  const boost = vendorBoost ?? (status.vendor_bus_selected ? undefined : status.fan_boost);

  const cpuUsage = clampPct(status.cpu_usage ?? 0);
  const cpuFreq = status.cpu_freq ?? 0;
  const cpuBase = status.cpu_base_mhz && status.cpu_base_mhz > 0 ? status.cpu_base_mhz : 0;
  const cpuTemp = status.cpu_temp ?? 0;
  const freqCeiling = Math.max(3600, cpuBase * 1.8);
  const freqPct = clampPct((cpuFreq / freqCeiling) * 100);

  const gpuUsage = clampPct(status.gpu_usage ?? 0);
  const gpuVramUsed = status.gpu_vram_used_mb ?? 0;
  const gpuVramTotal = status.gpu_vram_mb ?? 0;
  const gpuVramPct = gpuVramTotal > 0 ? clampPct((gpuVramUsed / gpuVramTotal) * 100) : 0;
  const gpuTemp = status.gpu_temp ?? 0;
  const hasDiscreteGpu = discreteGpu ?? /NVIDIA|RADEON RX/i.test(status.gpu_name ?? '');
  const hasDedicatedVram = hasDiscreteGpu && gpuVramTotal > 0;
  const hasSharedMemory = !hasDiscreteGpu && gpuVramTotal > 0 && status.gpu_memory_usage_available === true;
  const storageLabel = statsUnavailable ? '存储采样不可用' : stats ? '存储实时' : '等待存储采样';

  const mem = stats?.mem?.total_mb ? stats.mem : null;
  const memPct = clampPct(mem?.usage ?? 0);
  const disks = stats?.disks ?? [];

  const fanCeiling = maxFanRpm && maxFanRpm > 0 ? maxFanRpm : 0;
  const cpuFan = status.cpu_fan_rpm ?? 0;
  const gpuFan = status.gpu_fan_rpm ?? 0;
  // Dual-fan iGPU models expose an auxiliary chassis fan, not a dedicated GPU fan.
  const hasGpuFanChannel = hasDedicatedVram || gpuFan > 0;
  const cpuFanPct = fanCeiling ? clampPct((cpuFan / fanCeiling) * 100) : 0;
  const gpuFanPct = fanCeiling ? clampPct((gpuFan / fanCeiling) * 100) : 0;

  return (
    <div className="sys-monitor">
      <div className="sys-monitor-head">
        <div className="sys-monitor-title">
          <Activity size={16} />
          <span>系统监控</span>
        </div>
        <div className="sys-monitor-badges">
          <button className="resource-button" type="button" aria-pressed={resourceView}
            onClick={() => onResourceView(!resourceView)}>{resourceView ? '返回监控概览' : '资源去向'}</button>
          {!resourceView && <span className={statsUnavailable ? 'sys-monitor-badge is-demo' : 'sys-monitor-badge'}>
            <span className="live-dot" />
            {status.is_mock ? '硬件演示数据' : storageLabel}
          </span>}
        </div>
      </div>

      {resourceView ? <ResourceOverview /> : <>
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
            <span className={cpuTemp > 0 ? `mon-plain-value is-${temperatureTone(cpuTemp)}` : 'mon-plain-value'}>
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
              {hasDiscreteGpu ? (
                <>
                  <Zap size={11} />
                  {fmtWatt(status.gpu_power_w)}
                </>
              ) : (
                status.gpu_name ? '核显 · WDDM' : '适配器未识别'
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

          {(hasDedicatedVram || hasSharedMemory) && (
            <div className="mon-row">
              <div className="mon-row-head">
                <span className="mon-row-label">{hasSharedMemory ? '共享显存' : '显存'}</span>
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
              <span className={`mon-plain-value is-${temperatureTone(gpuTemp)}`}>{gpuTemp} °C</span>
            </div>
          )}

          {(!hasDiscreteGpu || status.gpu_name) && (
            <div className="mon-plain">
              <span className="mon-plain-label">适配器</span>
              <span className="mon-plain-value">{status.gpu_name || '未识别'}</span>
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
            <div className="mon-card-tag">{stats ? `${disks.length} 个固定卷` : '固定卷—'}</div>
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
              <span className="mon-plain-value">{statsUnavailable ? '无可用读数' : stats ? '没有固定卷数据' : '采集中…'}</span>
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
              {fanCeiling ? `上限 ${fanCeiling} RPM` : '实时转速'}
            </div>
          </div>

          <div className="mon-row">
            <div className="mon-row-head">
              <span className="mon-row-label">处理器</span>
              <span className="mon-row-value">
                {cpuFan > 0 ? `${cpuFan} RPM${fanCeiling ? ` · 约 ${Math.round(cpuFanPct)}%` : ''}` : '-- RPM'}
              </span>
            </div>
            {fanCeiling > 0 && <RevTickBar value={cpuFanPct} />}
          </div>

          {hasGpuFanChannel && (
            <div className="mon-row">
              <div className="mon-row-head">
                <span className="mon-row-label">{hasDiscreteGpu ? '显卡' : '辅助风扇'}</span>
                <span className="mon-row-value">{gpuFan > 0 ? `${gpuFan} RPM` : '-- RPM'}</span>
              </div>
              {fanCeiling > 0 && <RevTickBar value={gpuFanPct} />}
            </div>
          )}

          <div className="mon-plain">
            <span className="mon-plain-label">散热策略</span>
            <span className="mon-plain-value">{!status.vendor_bus_selected && status.support_flags?.has_fan_boost === false ? '固件自动温控' : boost === undefined ? '未确认' : boost ? (status.vendor_bus_selected ? '强冷请求已开启' : '强冷 / 全速') : '自动温控'}</span>
          </div>

          <div className="mon-plain">
            <span className="mon-plain-label">控制通道</span>
            <span className={vendor.snapshot ? 'mon-plain-value is-ok' : 'mon-plain-value'}>
              {nativeControl ? '原生 WMI · 无需官方后台' : vendor.snapshot ? `在线 · ${vendor.snapshot.latency} ms`
                : vendor.service?.selected === false ? '未启用 · 当前使用本地硬件通道'
                : vendor.service?.present ? (vendor.service.running ? '后台运行中 · 尚未收到风扇状态' : '厂商后台未运行')
                : vendor.service ? '未安装厂商后台' : '正在检查…'}
            </span>
          </div>

          {vendor.snapshot && (
            <div className="mon-plain">
              <span className="mon-plain-label">厂商强冷请求</span>
              <button
                type="button"
                className={`toggle-btn ${vendorBoost ? 'active-emerald' : ''}`}
                onClick={() => vendor.write('vendor_fan_boost', { enabled: !vendorBoost })}
                disabled={vendor.busy || vendor.checking}
              >
                {vendorBoost ? '请求已开启' : '请求已关闭'}
              </button>
            </div>
          )}

          {vendor.snapshot && (
            <div className="sys-row-desc">状态来自厂商服务回执，不代表风扇已经达到全速；实际效果请对照上方转速。</div>
          )}

          {vendor.service?.present && (
            <div className="mon-plain">
              <span className="mon-plain-label">厂商后台</span>
              {!vendor.snapshot && (
                <button type="button" className="toggle-btn" disabled={vendor.busy || vendor.checking}
                  onClick={() => vendor.connect()}>
                  {vendor.busy ? '启动中…' : '连接厂商通道'}
                </button>
              )}
              {!vendor.snapshot && vendor.service.running && (
                <button type="button" className="toggle-btn" disabled={vendor.busy || vendor.checking}
                  onClick={() => vendor.restart()} title="仅重启已核实的厂商服务和控制进程，不恢复官方界面">
                  重启后台
                </button>
              )}
              <button type="button" className={'toggle-btn ' + (vendor.service.selected && vendor.service.auto_start ? 'active-emerald' : '')}
                disabled={vendor.busy || vendor.checking}
                title="随系统启动；在监控或设置页可见时尝试恢复断开的通道"
                onClick={() => vendor.connect(!(vendor.service?.selected && vendor.service?.auto_start))}>
                {vendor.service.selected && vendor.service.auto_start ? '关闭常开' : '连接并常开'}
              </button>
            </div>
          )}
          {vendor.error && <div className="mon-plain" role="alert">{vendor.error}</div>}

          <div className="mon-plain">
            <span className="mon-plain-label">转速调节</span>
            <span className="mon-plain-value">
              {vendor.snapshot
                ? '厂商强冷请求（由固件执行）'
                : fanControllable
                  ? '本机型可调（电源管理 → 风扇曲线）'
                  : '固件自动温控'}
            </span>
          </div>
        </section>
      </div>

      <TelemetryTracePanel />
      <div className="sys-monitor-foot">
        <span>硬件遥测由宿主推送 · RAM / 卷容量独立采样</span>
        <span>{status.model_name || 'OpenRevo'}</span>
      </div>
      </>}
    </div>
  );
};
