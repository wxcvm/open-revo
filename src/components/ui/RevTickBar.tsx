import React from 'react';

export type MeterTone = 'ok' | 'warn' | 'hot';

export interface RevTickBarProps {
  /** 0 - 100 百分比载体，非百分比指标请先自行换算 */
  value: number;
  /** 分段数量（默认 48 段，对齐设计稿的机械刻度密度） */
  segments?: number;
  /** 严重度着色，仅切换 CSS 变量，不携带任何硬编码色值 */
  tone?: MeterTone;
  className?: string;
}

/**
 * RevTickBar — 分段式刻度仪表（系统监控 / 遥测页通用）
 * 纯 CSS 变量驱动，主题切换由 [data-theme="mono"] 自动接管。
 */
export const RevTickBar: React.FC<RevTickBarProps> = ({
  value,
  segments = 48,
  tone = 'ok',
  className,
}) => {
  const clamped = Number.isFinite(value) ? Math.min(100, Math.max(0, value)) : 0;
  const lit = Math.round((clamped / 100) * segments);

  return (
    <div
      className={['rev-tick-bar', tone === 'warn' ? 'is-warn' : '', tone === 'hot' ? 'is-hot' : '', className]
        .filter(Boolean)
        .join(' ')}
      role="meter"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(clamped)}
    >
      {Array.from({ length: segments }, (_, index) => {
        const on = index < lit;
        return (
          <span
            key={index}
            className={on ? 'rev-tick on' : 'rev-tick'}
            style={{
              opacity: on ? 0.55 + (0.45 * (index + 1)) / segments : undefined,
              transitionDelay: `${index * 5}ms`,
            }}
          />
        );
      })}
    </div>
  );
};
