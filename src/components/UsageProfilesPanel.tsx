import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface Profile { id: string; name: string; native_mode: number; energy_mode: string; brightness: number | null; keyboard_timeout: number | null; refresh_hz: number | null }
interface Settings { supported: boolean; profiles: Profile[]; auto_ac: string | null; auto_battery: string | null; current_energy: string | null; current_native_mode: number | null }
interface BatteryData { instance: string; designed_capacity: number | null; full_capacity: number | null; remaining_capacity: number | null; charge_rate: number | null; discharge_rate: number | null; cycles: number | null; charging: boolean | null; discharging: boolean | null; capacity_relative: number | null }
interface Policy { policy: string; paths: { target_id: number; drr_enabled: boolean; virtual_hz: number | null }[] }
const energyNames: Record<string, string> = { efficiency: '最佳能效', balanced: '平衡', performance: '最佳性能' };
const positive = (value: number | null | undefined): value is number => typeof value === 'number' && value > 0 && value < 0xffffffff;

export const UsageProfilesPanel: React.FC = () => {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [batteries, setBatteries] = useState<BatteryData[] | null>(null);
  const [policy, setPolicy] = useState<Policy | null>(null);
  const [message, setMessage] = useState('');
  const [batteryError, setBatteryError] = useState('');
  const [busy, setBusy] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [editing, setEditing] = useState(false);
  const refreshBattery = () => {
    setBatteryError('');
    invoke<BatteryData[]>('get_battery_health').then(setBatteries).catch(e => setBatteryError(String(e)));
  };
  useEffect(() => {
    let alive = true;
    invoke<Settings>('get_usage_settings').then(s => { if (alive) setSettings(s); }).catch(e => { if (alive) setMessage(String(e)); });
    const refresh = () => {
      invoke<Policy>('get_display_policy').then(p => { if (alive) setPolicy(p); }).catch(() => {});
      invoke<Settings>('get_usage_settings').then(current => {
        if (alive && current) setSettings(previous => previous ? { ...previous, current_energy: current.current_energy, current_native_mode: current.current_native_mode } : current);
      }).catch(() => {});
    };
    refresh();
    const timer = setInterval(refresh, 5000);
    return () => { alive = false; clearInterval(timer); };
  }, []);
  const run = async (action: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(true); setMessage('');
    try { await action(); } catch (e) { setMessage(String(e)); }
    finally { setBusy(false); }
  };
  const save = async () => {
    await invoke('save_usage_settings', { settings });
    setDirty(false); setMessage('场景配置已保存；尚未应用硬件设置');
  };
  const update = (id: string, patch: Partial<Profile>) => {
    setSettings(s => s && ({ ...s, profiles: s.profiles.map(p => p.id === id ? { ...p, ...patch } : p) }));
    setDirty(true);
  };
  return <section className="sys-sub-panel usage-panel" aria-label="场景与电池健康">
    <h3>场景与系统能源模式</h3>
    {message && <div role="status" className="usage-notice">{message}</div>}
    <div className="quick-row">
      <div><strong>Windows 能源模式</strong><p className="sys-row-desc">独立于固件档位；手动选择后关闭旧版档位联动，避免被覆盖</p></div>
      <select aria-label="Windows 能源模式" disabled={busy || !settings} value={settings?.current_energy ?? ''} onChange={e => {
        const mode = e.target.value;
        void run(async () => { await invoke('set_windows_energy', { mode }); setSettings(await invoke<Settings>('get_usage_settings')); });
      }}><option value="" disabled>未提供或正在读取</option>{Object.entries(energyNames).map(([value, name]) => <option key={value} value={value}>{name}</option>)}</select>
    </div>
    {settings?.supported && <>
      <p className="sys-row-desc">以下是使用场景，不是充电上限模式。未勾选的屏幕与背光项目保持当前设置。</p>
      <div className="usage-actions">
        {settings.profiles.map(p => <button key={p.id} className="toggle-btn" disabled={busy || dirty} onClick={() => void run(async () => {
          const result = await invoke<{ applied: string[]; failed: string[]; rollback_failed: string[] }>('apply_usage_profile', { id: p.id });
          setMessage(result.failed.length ? `应用失败：${result.failed.join('；')}。${result.rollback_failed.length ? `回退未完成：${result.rollback_failed.join('；')}` : '已尝试恢复原设置'}` : `已应用 ${p.name}：${result.applied.join('、')}`);
          setSettings(await invoke<Settings>('get_usage_settings'));
          setPolicy(await invoke<Policy>('get_display_policy'));
        })}>{p.name}</button>)}
        <button className="toggle-btn" onClick={() => setEditing(!editing)}>{editing ? '收起编辑' : '编辑场景'}</button>
      </div>
      <div className="usage-actions">
        {(['auto_ac', 'auto_battery'] as const).map(key => <label key={key}>{key === 'auto_ac' ? '插电时' : '使用电池时'}
          <select aria-label={key === 'auto_ac' ? '插电场景' : '电池场景'} disabled={busy} value={settings[key] ?? ''} onChange={e => { setSettings({ ...settings, [key]: e.target.value || null }); setDirty(true); }}>
            <option value="">不自动切换</option>{settings.profiles.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
          </select>
        </label>)}
        <button className="toggle-btn active-indigo" disabled={!dirty || busy} onClick={() => void run(save)}>保存配置</button>
      </div>
      {editing && settings.profiles.map(p => <div className="usage-editor" key={p.id}>
        <label>名称<input aria-label={`${p.id} 名称`} maxLength={40} value={p.name} onChange={e => update(p.id, { name: e.target.value })} /></label>
        <label>固件<select value={p.native_mode} onChange={e => update(p.id, { native_mode: Number(e.target.value) })}><option value={2}>均衡</option><option value={3}>性能</option></select></label>
        <label>Windows<select value={p.energy_mode} onChange={e => update(p.id, { energy_mode: e.target.value })}>{Object.entries(energyNames).map(([value, name]) => <option key={value} value={value}>{name}</option>)}</select></label>
        {(['brightness', 'keyboard_timeout', 'refresh_hz'] as const).map(key => <label key={key}>
          <span><input type="checkbox" checked={p[key] !== null} onChange={e => update(p.id, { [key]: e.target.checked ? ({ brightness: 50, keyboard_timeout: 600, refresh_hz: 60 }[key]) : null })} />{{ brightness: '接管亮度 %', keyboard_timeout: '接管背光超时 秒（0 常亮）', refresh_hz: '接管内屏固定 Hz' }[key]}</span>
          {p[key] !== null && <input type="number" min={key === 'refresh_hz' ? 1 : 0} max={key === 'brightness' ? 100 : key === 'refresh_hz' ? 1000 : 65535} value={p[key]!} onChange={e => update(p.id, { [key]: Number(e.target.value) })} />}
        </label>)}
      </div>)}
    </>}
    <div className="quick-row">
      <div><strong>刷新率控制：{policy?.policy === 'fixed' ? '本次固定覆盖' : '跟随 Windows'}</strong><p className="sys-row-desc">DRR：{!policy ? '未知' : policy.paths.some(p => p.drr_enabled) ? '已启用' : '当前未启用（不代表不支持）'}；插拔电不再自动写刷新率。异常退出后的状态请在系统显示设置中确认。</p></div>
      <button className="toggle-btn" disabled={busy} onClick={() => void run(async () => { await invoke('set_auto_min_refresh_on_battery', { enable: false }); setPolicy(await invoke<Policy>('get_display_policy')); setMessage('已交还 Windows 管理；不会擅自开启 DRR'); })}>跟随系统</button>
    </div>
    <div className="quick-row"><div><strong>电池健康与充放电</strong><p className="sys-row-desc">按需读取 Windows 电池驱动，不需要官方后台；不支持的数据不推测。</p></div><button className="toggle-btn" onClick={refreshBattery}>读取／刷新</button></div>
    {batteryError && <p role="status">{batteryError}</p>}
    {batteries?.length === 0 && <p>系统未提供电池健康信息</p>}
    {batteries && <p className="sys-row-desc">健康度为满充／设计容量的原始比值，校准或出厂余量可能使其超过 100%，不等于异常。</p>}
    {batteries?.map(b => {
      const absolute = b.capacity_relative === 0;
      const capacity = (n: number | null) => positive(n) ? `${(absolute ? n / 1000 : n).toFixed(1)} ${absolute ? 'Wh' : '相对单位'}` : '未提供';
      const health = positive(b.designed_capacity) && positive(b.full_capacity) ? `${(b.full_capacity / b.designed_capacity * 100).toFixed(1)}%` : '未提供';
      const rate = b.discharging ? b.discharge_rate : b.charging ? b.charge_rate : null;
      const estimate = b.discharging && positive(b.remaining_capacity) && positive(rate) ? `${Math.round(b.remaining_capacity / rate * 60)} 分钟（当前速率估算）` : '未提供';
      return <dl className="usage-battery" key={b.instance}>
        <div><dt>设计容量</dt><dd>{capacity(b.designed_capacity)}</dd></div><div><dt>满充容量</dt><dd>{capacity(b.full_capacity)}</dd></div>
        <div><dt>健康度</dt><dd>{health}</dd></div><div><dt>循环次数</dt><dd>{positive(b.cycles) ? b.cycles : '未提供'}</dd></div>
        <div><dt>{b.discharging ? '放电' : '充电'}功率</dt><dd>{absolute && positive(rate) ? `${(rate / 1000).toFixed(1)} W` : '未提供'}</dd></div><div><dt>预计续航</dt><dd>{estimate}</dd></div>
      </dl>;
    })}
  </section>;
};
