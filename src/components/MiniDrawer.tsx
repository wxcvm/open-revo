import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { HardwareState, PowerMode } from '../types/hardware';
import { ModelCapabilities } from '../plugins/plugin_manifest';
import { DrawerHeader } from './drawer/DrawerHeader';
import { DrawerPowerModeCard } from './drawer/DrawerPowerModeCard';
import { DrawerGpuModeCard } from './drawer/DrawerGpuModeCard';
import { DrawerDisplayCard } from './drawer/DrawerDisplayCard';
import { DrawerLightingCard } from './drawer/DrawerLightingCard';
import { DrawerBatteryCard } from './drawer/DrawerBatteryCard';
import { DrawerQuickSwitches } from './drawer/DrawerQuickSwitches';
import { DrawerFooter } from './drawer/DrawerFooter';
import './drawer/drawer.css';

interface MiniDrawerProps {
  status: HardwareState;
  theme?: 'mono' | 'cyber';
  capabilities?: ModelCapabilities;
  onToggleTheme?: () => void;
  onSetPowerMode: (mode: PowerMode) => void;
  onToggleFanBoost: () => void;
  onToggleBatteryLimit: (limit?: number) => void;
  onSwitchRefreshRate: (hz: number, invokeBackend?: boolean) => void;
  onToggleWinKey: () => void;
  onToggleAutostart: () => void;
  onToggleOemService: (takeOver: boolean) => Promise<void>;
  onExpandDashboard: (tab?: 'overview' | 'gpu' | 'tuning' | 'lighting' | 'water' | 'system' | 'sponsor' | 'custom_tuning') => void;
  onOpenSponsor: () => void;
  onMinimize: () => void;
  onClose: () => void;
}

export const MiniDrawer: React.FC<MiniDrawerProps> = ({
  status,
  theme: controlledTheme,
  capabilities,
  onToggleTheme,
  onSetPowerMode,
  onToggleFanBoost,
  onToggleBatteryLimit,
  onSwitchRefreshRate,
  onToggleWinKey,
  onToggleAutostart,
  onToggleOemService,
  onExpandDashboard,
  onOpenSponsor,
  onMinimize,
  onClose,
}) => {
  // Theme state: 'mono' (纯单色工业风) or 'cyber' (多彩风格)
  const [internalTheme, setInternalTheme] = useState<'mono' | 'cyber'>(() => {
    return (localStorage.getItem('openrevo_theme') as 'mono' | 'cyber') ||
      (localStorage.getItem('openrevo_mini_theme') as 'mono' | 'cyber') || 'mono';
  });
  const theme = controlledTheme ?? internalTheme;

  const [showOemModal, setShowOemModal] = useState(false);

  // Elastic height auto-fitting: measure natural content height and anchor to taskbar
  const contentRef = useRef<HTMLDivElement>(null);
  const lastHeightRef = useRef<number>(0);
  const [isOverflowing, setIsOverflowing] = useState(false);

  useEffect(() => {
    const el = contentRef.current;
    if (!el) return;

    const measureAndSync = (force: boolean = false) => {
      // getBoundingClientRect gives the exact fractional height
      const rect = el.getBoundingClientRect();
      const naturalHeight = Math.ceil(rect.height);

      if (naturalHeight > 250 && (force || Math.abs(naturalHeight - lastHeightRef.current) >= 2)) {
        lastHeightRef.current = naturalHeight;
        invoke<number>('fit_mini_window_height', { targetHeight: naturalHeight })
          .then((actualClampedH) => {
            if (actualClampedH && actualClampedH < naturalHeight - 2) {
              setIsOverflowing(true);
            } else {
              setIsOverflowing(false);
            }
          })
          .catch((err) => {
            console.debug('[MiniDrawer] fit_mini_window_height skipped or failed:', err);
          });
      }
    };

    // Immediate initial sync
    measureAndSync(true);

    // Use ResizeObserver to detect any dynamic card toggles / dropdown expansions
    let ro: ResizeObserver | null = new ResizeObserver(() => {
      measureAndSync(false);
    });
    ro.observe(el);

    // Re-measure when window regains focus or receives restore events
    const onWindowFocus = () => {
      measureAndSync(true);
    };
    window.addEventListener('focus', onWindowFocus);

    let unlistenReset: (() => void) | undefined;
    let unlistenSync: (() => void) | undefined;
    let unlistenFocus: (() => void) | undefined;

    listen('reset-to-mini', () => {
      measureAndSync(true);
    }).then((un) => { unlistenReset = un; });

    listen<boolean>('sync-window-mode', (e) => {
      if (!e.payload) {
        measureAndSync(true);
      }
    }).then((un) => { unlistenSync = un; });

    listen('tauri://focus', () => {
      measureAndSync(true);
    }).then((un) => { unlistenFocus = un; });

    return () => {
      if (ro) {
        ro.disconnect();
        ro = null;
      }
      window.removeEventListener('focus', onWindowFocus);
      if (unlistenReset) unlistenReset();
      if (unlistenSync) unlistenSync();
      if (unlistenFocus) unlistenFocus();
    };
  }, []);

  const checkOemGuard = (action: () => void) => {
    if (status.oem_running || status.vendor_bus_selected) {
      setShowOemModal(true);
      return;
    }
    action();
  };

  const toggleTheme = () => {
    if (onToggleTheme) {
      onToggleTheme();
    } else {
      const nextTheme = theme === 'mono' ? 'cyber' : 'mono';
      setInternalTheme(nextTheme);
      localStorage.setItem('openrevo_theme', nextTheme);
      localStorage.setItem('openrevo_mini_theme', nextTheme);
    }
  };

  return (
    <div
      className={`mini-drawer-root openrevo-mini-shell ${isOverflowing ? 'is-overflowing' : ''}`}
      data-theme={theme}
      data-slot="mini-shell"
    >
      <div
        ref={contentRef}
        className="mini-drawer-content"
      >
        {/* 顶部标题栏 */}
        <div className="mini-slot-header" data-slot="header">
          <DrawerHeader
            status={status}
            theme={theme}
            onToggleTheme={toggleTheme}
            onOpenSponsor={onOpenSponsor}
            onMinimize={onMinimize}
            onClose={onClose}
          />
        </div>

        {/* 第 1 排：性能模式 */}
        <div className="mini-slot-power" data-slot="power">
          <DrawerPowerModeCard
            status={status}
            capabilities={capabilities}
            onSetPowerMode={onSetPowerMode}
            checkOemGuard={checkOemGuard}
            onExpandDashboard={onExpandDashboard}
          />
        </div>

        {/* 第 2 排：显卡模式（仅具独立显卡机型显示） */}
        {capabilities?.discrete_gpu !== false && (
          <div className="mini-slot-gpu" data-slot="gpu">
            <DrawerGpuModeCard
              status={status}
              onExpandDashboard={onExpandDashboard}
            />
          </div>
        )}

        {/* 第 3 & 4 排：屏幕刷新率与亮度 */}
        <div className="mini-slot-display" data-slot="display">
          <DrawerDisplayCard
            status={status}
            onSwitchRefreshRate={onSwitchRefreshRate}
            checkOemGuard={checkOemGuard}
          />
        </div>

        {/* 第 5 排：键盘背光调节 (支持 RGB 或 超轻薄本延时休眠) */}
        {(capabilities?.rgb_keyboard?.supported !== false ||
          capabilities?.rgb_keyboard?.sleep_timeout_supported ||
          capabilities?.form_factor === 'thin_light') && (
          <div className="mini-slot-lighting" data-slot="lighting">
            <DrawerLightingCard
              status={status}
              capabilities={capabilities}
              onExpandDashboard={onExpandDashboard}
            />
          </div>
        )}



        {/* 第 6 排：电池健康 */}
        <div className="mini-slot-battery" data-slot="battery">
          <DrawerBatteryCard
            status={status}
            onToggleBatteryLimit={onToggleBatteryLimit}
            checkOemGuard={checkOemGuard}
            onExpandDashboard={onExpandDashboard}
          />
        </div>

        {/* 第 7 排：快捷开关矩阵 */}
        <div className="mini-slot-switches" data-slot="switches">
          <DrawerQuickSwitches
            status={status}
            capabilities={capabilities}
            checkOemGuard={checkOemGuard}
            onToggleFanBoost={onToggleFanBoost}
            onToggleWinKey={onToggleWinKey}
          />
        </div>

        {/* 底部操作栏及 OEM 冲突弹窗 */}
        <div className="mini-slot-footer" data-slot="footer">
          <DrawerFooter
            status={status}
            onToggleAutostart={onToggleAutostart}
            onToggleOemService={onToggleOemService}
            onExpandDashboard={onExpandDashboard}
            onClose={onClose}
            showOemModal={showOemModal}
            setShowOemModal={setShowOemModal}
          />
        </div>
      </div>
    </div>
  );
};
