import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { HardwareState } from '../../types/hardware';
import { Cpu, Repeat, Crosshair, Lock } from 'lucide-react';
import type { GpuCapabilities } from '../GpuModeView';

interface DrawerGpuModeCardProps {
  status: HardwareState;
  onExpandDashboard: (tab?: 'overview' | 'gpu' | 'tuning' | 'lighting' | 'water' | 'system' | 'sponsor' | 'custom_tuning') => void;
}

export const DrawerGpuModeCard: React.FC<DrawerGpuModeCardProps> = ({
  status,
  onExpandDashboard,
}) => {
  // GPU architecture & mode state
  const [gpuModeInfo, setGpuModeInfo] = useState<{
    current_mode: string;
    has_dgpu: boolean;
    has_igpu_present: boolean;
    gpu_name: string;
    capabilities?: GpuCapabilities;
  } | null>(null);

  useEffect(() => {
    let disposed = false;
    let latest = 0;
    const fetchGpuMode = () => {
      const request = ++latest;
      invoke<typeof gpuModeInfo>('get_gpu_mode_info')
        .then((res) => {
          if (!disposed && request === latest) setGpuModeInfo(res);
        })
        .catch(() => { if (!disposed && request === latest) setGpuModeInfo(null); });
    };

    fetchGpuMode();
    window.addEventListener('focus', fetchGpuMode);

    let unlistenGpu: (() => void) | undefined;
    listen('gpu-mode-changed', fetchGpuMode).then((fn) => {
      if (disposed) fn(); else unlistenGpu = fn;
    }).catch(() => {});

    return () => {
      disposed = true;
      window.removeEventListener('focus', fetchGpuMode);
      if (unlistenGpu) unlistenGpu();
    };
  }, []);

  // Match the native MUX capability, never treat an absent read as permission.
  const nativeIgpu = gpuModeInfo?.has_dgpu === false;
  const caps = gpuModeInfo?.capabilities;
  const supportsIgpu = nativeIgpu || (gpuModeInfo?.has_dgpu === true && caps?.supports_igpu_only === true);
  const supportsHybrid = gpuModeInfo?.has_dgpu === true && caps?.supports_hybrid === true;
  const supportsDgpu = gpuModeInfo?.has_dgpu === true && caps?.supports_dgpu === true;
  const igpuLocked = gpuModeInfo?.has_dgpu === true && caps?.supports_igpu_only === false;
  const gpuTemp = Number.isFinite(status.gpu_temp) && status.gpu_temp > 0 ? `${status.gpu_temp}°C` : '--';
  const fan = Number.isFinite(status.gpu_fan_rpm) && status.gpu_fan_rpm > 0 ? `${status.gpu_fan_rpm} RPM` : '--';

  return (
    <div className="m-card">
      <div className="m-card-header">
        <div className="m-header-left">
          <Cpu size={13} style={{ color: 'var(--theme-gpu)' }} />
          <span>显卡模式:</span>
          <span style={{ color: 'var(--theme-gpu)', fontWeight: 700 }}>
            {gpuModeInfo?.has_dgpu === false
              ? '核显模式 (纯APU架构)'
              : gpuModeInfo?.current_mode === 'dgpu'
              ? '独显直连 (dGPU)'
              : gpuModeInfo?.current_mode === 'hybrid'
              ? '混合输出 (Dynamic)'
              : gpuModeInfo?.current_mode === 'igpu'
              ? '核显模式 (iGPU)'
              : '检测中...'}
          </span>
        </div>
        <div className="m-header-right" style={{ color: 'var(--theme-gpu)' }}>
          GPU: {gpuTemp} · Fan: {fan}
        </div>
      </div>

      <div className="m-grid-3">
        <button
          className="m-btn-action"
          disabled={!supportsIgpu}
          style={{
            ...(gpuModeInfo?.current_mode === 'igpu' || gpuModeInfo?.has_dgpu === false
              ? {
                  background: 'var(--theme-gpu-dim)',
                  borderColor: 'var(--theme-gpu-border)',
                  color: 'var(--theme-gpu)',
                  boxShadow: '0 0 12px var(--theme-gpu-glow)',
                }
              : {}),
          }}
          title={nativeIgpu ? '纯核显机型，打开显示设置' : igpuLocked
            ? (caps?.reason_igpu_unsupported || '双模机型，未提供纯集显直连')
            : supportsIgpu ? '打开显卡模式面板' : '尚未确认纯集显能力'}
          onClick={() => onExpandDashboard('gpu')}
        >
          {igpuLocked ? <Lock size={14} /> : <Cpu size={14} />}
          <span style={{ fontSize: '10.5px', fontWeight: 600 }}>
            {nativeIgpu ? '纯集显 (生效)' : igpuLocked ? '双模锁定' : '纯集显'}
          </span>
        </button>

        <button
          className="m-btn-action"
          disabled={!supportsHybrid}
          style={{
            ...(gpuModeInfo?.current_mode === 'hybrid' && gpuModeInfo?.has_dgpu !== false
              ? {
                  background: 'var(--theme-gpu-dim)',
                  borderColor: 'var(--theme-gpu-border)',
                  color: 'var(--theme-gpu)',
                  boxShadow: '0 0 12px var(--theme-gpu-glow)',
                }
              : {}),
            ...(gpuModeInfo?.has_dgpu === false
              ? { opacity: 0.35, cursor: 'not-allowed' }
              : {}),
          }}
          title={supportsHybrid ? '打开显卡模式面板' : '未确认混合输出可用'}
          onClick={() => {
            if (gpuModeInfo?.has_dgpu !== false) onExpandDashboard('gpu');
          }}
        >
          <Repeat size={14} />
          <span style={{ fontSize: '10.5px', fontWeight: 600 }}>标准混合</span>
        </button>

        <button
          className="m-btn-action"
          disabled={!supportsDgpu}
          style={{
            ...(gpuModeInfo?.current_mode === 'dgpu' && gpuModeInfo?.has_dgpu !== false
              ? {
                  background: 'var(--theme-gpu-dim)',
                  borderColor: 'var(--theme-gpu-border)',
                  color: 'var(--theme-gpu)',
                  boxShadow: '0 0 12px var(--theme-gpu-glow)',
                }
              : {}),
            ...(gpuModeInfo?.has_dgpu === false
              ? { opacity: 0.35, cursor: 'not-allowed' }
              : {}),
          }}
          title={supportsDgpu ? '打开显卡模式面板' : '未确认独显直连可用'}
          onClick={() => {
            if (gpuModeInfo?.has_dgpu !== false) onExpandDashboard('gpu');
          }}
        >
          <Crosshair size={14} />
          <span style={{ fontSize: '10.5px', fontWeight: 600 }}>独显直连</span>
        </button>
      </div>
    </div>
  );
};
