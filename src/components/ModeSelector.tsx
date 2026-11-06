import React from 'react';
import { POWER_MODES, PowerMode } from '../types/hardware';
import { ModelCapabilities } from '../plugins/plugin_manifest';
import { Shield, Zap, Flame, Sliders, Settings2 } from 'lucide-react';

interface ModeSelectorProps {
  currentMode: PowerMode | number;
  onSelectMode: (mode: PowerMode) => void;
  disabled?: boolean;
  isDcOnline?: boolean;
  unlockBatteryLimit?: boolean;
  onOpenCustomTuning?: () => void;
  capabilities?: ModelCapabilities;
  isThinLight?: boolean;
}

export const ModeSelector: React.FC<ModeSelectorProps> = ({
  currentMode,
  onSelectMode,
  disabled = false,
  isDcOnline = true,
  unlockBatteryLimit = false,
  onOpenCustomTuning,
  capabilities,
  isThinLight = false,
}) => {
  const isLight = isThinLight || capabilities?.form_factor === 'thin_light';
  const isUnadapted = capabilities?.is_adapted === false;
  const hasCustomMode = !isUnadapted && (capabilities?.power_modes ? capabilities.power_modes.includes('custom') : !isLight);
  const modeKeys = ['office', 'balance', 'turbo', 'custom'];
  const availableModes = POWER_MODES.filter((m) => (hasCustomMode || m.id !== 4)
    && (!capabilities?.power_modes || capabilities.power_modes.includes(modeKeys[m.id - 1])));

  const getIcon = (id: PowerMode) => {
    switch (id) {
      case 1:
        return <Shield className="w-4 h-4 text-emerald-400" />;
      case 2:
        return <Zap className="w-4 h-4 text-blue-400" />;
      case 3:
        return <Flame className="w-4 h-4 text-rose-400" />;
      case 4:
        return <Sliders className="w-4 h-4 text-purple-400" />;
      default:
        return <Zap className="w-4 h-4" />;
    }
  };

  const getActiveClass = (id: PowerMode) => {
    if (currentMode !== id) return '';
    switch (id) {
      case 1:
        return 'active-office';
      case 2:
        return 'active-balance';
      case 3:
        return 'active-beast';
      case 4:
        return 'active-custom';
      default:
        return 'active-balance';
    }
  };

  return (
    <div style={{ display: 'grid', gridTemplateColumns: `repeat(${availableModes.length}, 1fr)`, gap: '8px' }}>
      {availableModes.map((mode) => {
        const isActive = currentMode === mode.id;
        const isCustom = mode.id === 4;
        const isRestricted = !isLight && (mode.id === 3 || mode.id === 4) && isDcOnline === false && !unlockBatteryLimit;

        return (
          <button
            key={mode.id}
            disabled={disabled || isRestricted}
            onClick={() => {
              if (isRestricted) return;
              if (isActive && isCustom && onOpenCustomTuning) {
                onOpenCustomTuning();
              } else {
                onSelectMode(mode.id);
              }
            }}
            className={`mode-btn ${getActiveClass(mode.id)}`}
            title={
              isRestricted
                ? '电池供电或受限供电下不可用，需接入原装电源适配器'
                : isCustom
                ? (isLight ? '点击直接打开自定义风扇曲线调校' : (isActive ? '已激活自定义模式，点击可直接打开硬件参数调校' : `${mode.nameEn} - ${mode.desc}`))
                : `${mode.nameEn} - ${mode.desc}`
            }
            style={{
              position: 'relative',
              ...(isRestricted
                ? {
                    opacity: 0.45,
                    cursor: 'not-allowed',
                    filter: 'grayscale(0.6)',
                  }
                : {}),
            }}
          >
            {/* Corner Badge for Custom Mode Tuning */}
            {isCustom && (isActive || isLight) && onOpenCustomTuning && !isRestricted && (
              <div
                onClick={(e) => {
                  e.stopPropagation();
                  onOpenCustomTuning();
                }}
                style={{
                  position: 'absolute',
                  top: '4px',
                  right: '5px',
                  display: 'flex',
                  alignItems: 'center',
                  gap: '2px',
                  padding: '1px 5px',
                  borderRadius: '4px',
                  background: 'rgba(168, 85, 247, 0.35)',
                  border: '1px solid rgba(192, 132, 252, 0.6)',
                  color: '#e9d5ff',
                  fontSize: '9px',
                  fontWeight: 700,
                  cursor: 'pointer',
                  boxShadow: '0 0 6px rgba(168, 85, 247, 0.4)',
                }}
                title="打开自定义硬件参数调校"
              >
                <Settings2 className="w-2.5 h-2.5" />
                <span>调校</span>
              </div>
            )}

            <div style={{ marginBottom: '2px' }}>{getIcon(mode.id)}</div>
            <div className="mode-title">{mode.name.replace('模式', '')}</div>
            <div className="mode-sub">
              {isRestricted
                ? '需圆口DC'
                : isActive
                ? isCustom
                  ? '● 极客调校'
                  : '● 当前'
                : mode.id === 1
                ? '静音'
                : mode.id === 2
                ? '日常'
                : mode.id === 3
                ? '狂暴'
                : '极客'}
            </div>
          </button>
        );
      })}
    </div>
  );
};
