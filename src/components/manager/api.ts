import { invoke } from '@tauri-apps/api/core';

export type Volume = { root: string; total: number | null; free: number | null };
export type Health = {
  time_ms: number; memory: { visible: number; available: number; load: number } | null;
  antivirus: string; firewall: string; volumes: Volume[];
};
export type WindowsInventory<T> = { rows: T[]; warnings: string[] };
export type Startup = {
  id?: string; name: string; source: string; detail: string; state: string; action?: 'disable' | 'restore';
};
export type AppInfo = { name: string; publisher: string; version: string; kind: 'desktop' | 'msix' | 'start_msix'; scope: string };
export type WindowsEvent = {
  time_ms: number; kind: string; source: string; event_id: number; record: number; detail: string;
};
export type Peak = { time_ms: number; metric: 'memory' | 'cpu' | 'disk' | 'network'; value: number };
export type UpdateInfo = { time_ms: number; title: string; result: number; source?: string };

export type ScanTarget = { kind: 'volume'; root: string; verify_duplicates?: boolean }
  | { kind: 'folder'; verify_duplicates?: boolean } | { kind: 'cleanup' };

export type VolumeSample = {
  captured_ms: number;
  totals: { total: number; free: number; used: number; reserved?: number | null; pool_unavailable?: number | null } | null;
  classified: [number, number, number, number, number];
  unresolved: number | null;
  recycle: { bytes: number; items: number } | null;
  skipped: number;
  unknown_allocation: number;
};
export type VolumeOverview = {
  current: VolumeSample; previous: VolumeSample | null; save_available: boolean; saved: boolean; notice: string | null;
};
export type Session = { hardware: boolean };
export type PowerMode = 'efficiency' | 'balanced' | 'performance';
export type Playback = { device: string; volume: number; muted: boolean };
type ControlReading<T> = { value: T | null; problem: string | null };
export type ControlSnapshot = { audio: ControlReading<Playback>; power: ControlReading<PowerMode> };
export type ControlAction = { kind: 'volume'; device: string; expected: number; level: number }
  | { kind: 'mute'; device: string; expected: boolean; muted: boolean }
  | { kind: 'energy'; expected: PowerMode; mode: PowerMode };
export type AwakeState = { enabled: boolean; remaining_ms: number | null };
export type Section = 'tree' | 'largest' | 'duplicates' | 'cleanup';
export type ScanRow = {
  id: number; path: string; category: string; logical: number; allocated: number | null; count: number; group: number | null;
};
export type Scan = {
  id: number; kind: 'space' | 'cleanup'; state: 'running' | 'canceling' | 'canceled' | 'partial' | 'complete' | 'failed';
  scope: string; volume?: Volume | null; overview?: VolumeOverview | null; duplicates_requested?: boolean; notes: string[]; error?: string | null; total: number[];
  progress: { files: number; skipped: number; logical: number; allocated: number };
  stats?: { files: number; logical: number; allocated: number; skipped: number; aliases: number;
    unknown_allocation: number; uncategorized: number; cleanup_limited: boolean; duplicates_limited: boolean } | null;
};
export type Action = { kind: 'cleanup'; job_id: number; ids: number[] }
  | { kind: 'startup'; id: string; restore: boolean }
  | { kind: 'close'; process: string };
export type Preview = { id: number; kind: string; caption: string; items: string[]; selected_bytes: number | null; recovery: string };
export type ItemResult = { item: string; status: 'done' | 'skipped' | 'unverified' | 'pending'; detail: string };
export type Receipt = {
  id: number; time_ms: number; kind: string; canceled: boolean; rows: ItemResult[]; warning?: string | null;
  volumes: { root: string; free_before: number | null; free_after: number | null }[];
};
export type ShellPage = 'apps' | 'startup' | 'storage' | 'update' | 'security' | 'recycle';

export const manager = {
  session: () => invoke<Session>('manager_session'),
  hardwarePanel: () => invoke<void>('manager_hardware_panel'),
  quickControls: () => invoke<ControlSnapshot>('manager_quick_controls'),
  setQuickControl: (action: ControlAction) => invoke<ControlSnapshot>('manager_set_quick_control', { action }),
  awakeStatus: () => invoke<AwakeState>('get_background_awake_status'),
  setAwake: (enable: boolean, minutes?: number) => invoke<boolean>('set_background_awake', { enable, minutes }),
  saveVolume: (id: number) => invoke<Scan>('manager_save_volume_snapshot', { id }),
  health: () => invoke<Health>('manager_health'),
  windows: <T>(kind: 'apps' | 'startup' | 'health' | 'events', hours?: number) =>
    invoke<T>('manager_inventory', { kind, hours }),
  startup: () => invoke<WindowsInventory<Startup>>('manager_startup'),
  shell: (page: ShellPage) => invoke('manager_shell', { page }),
  scan: (target: ScanTarget) => invoke<Scan | null>('manager_scan', { target }),
  status: (id: number) => invoke<Scan>('manager_scan_status', { id }),
  page: (id: number, section: Section, offset: number) => invoke<ScanRow[]>('manager_scan_page', { id, section, offset }),
  cancel: (id: number) => invoke('manager_scan_cancel', { id }),
  reveal: (jobId: number, rowId: number) => invoke('manager_reveal', { jobId, rowId }),
  preview: (action: Action) => invoke<Preview>('manager_preview', { action }),
  apply: (id: number) => invoke<Receipt>('manager_apply', { id }),
  receipts: () => invoke<Receipt[]>('manager_receipts'),
  peaks: () => invoke<Peak[]>('manager_peaks'),
};
export const textError = (e: unknown) => e instanceof Error ? e.message : String(e);
export const dateTime = (ms: number) => new Date(ms).toLocaleString('zh-CN', { hour12: false });
export const basename = (path: string) => path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
