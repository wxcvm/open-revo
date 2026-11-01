# 系统监控（System Monitor）数据链路与机型兼容性

> 对应前端页面：`src/components/SystemMonitorView.tsx` ＋ `src/components/ui/RevTickBar.tsx`
> 对应后端命令：`get_system_stats`（内存 / 卷容量）＋ 既有 `hardware-state-updated` 心跳（CPU / GPU / 风扇）

## 1. 指标来源

| 指标 | 采集方式 | 备注 |
| --- | --- | --- |
| CPU 使用率 | `GetSystemTimes` 差分（`system/cpu_monitor.rs` 既有实现） | 全平台可用 |
| CPU 频率 / 基准频率 | `CallNtPowerInformation(ProcessorInformation)` + PDH `% Processor Performance` | 基准频率取 ACPI 标称值（AMD APU 常见 2000 MHz） |
| CPU 温度 | PDH `\Thermal Zone Information(*)\Temperature` | 无可用热区时显示 `-- °C` |
| CPU / APU 功耗 | PDH `\Energy Meter(*)\Power` | 优先取含 `pkg` 的实例，否则取最大项 |
| 显卡使用率 | NVAPI（NVIDIA 独显）→ 无独显时回退 PDH `\GPU Engine(*)\Utilization Percentage`（累加 `engtype_3D`） | 新增 `system/gpu_engine.rs`，AMD / Intel 核显同样出数 |
| 显存 | NVAPI `get_vram_mb / used` | 总量为 0 时前端隐藏该行，不再显示 0/0 GB |
| 内存 | `GlobalMemoryStatusEx`（`system/sysinfo.rs`） | 31.3 GB 这类"可见内存"与任务管理器一致 |
| 卷容量 | `GetLogicalDrives` + `GetDriveTypeW`(DRIVE_FIXED) + `GetDiskFreeSpaceExW` + `GetVolumeInformationW` | 仅固定卷；U 盘 / 光驱 / 网络盘自动跳过；3 秒缓存 |
| 风扇转速 | EC / 机型驱动（既有 `cpu_fan_rpm`、`gpu_fan_rpm`） | 双通道均为 0 时页签显示 `EC 未映射` |

## 2. 本机实测（MECHREVO XINGYAO / AMD Ryzen AI 9 HX 365 + Radeon 880M / 32 GB）

在本机直接调用与代码完全相同的 Win32 / PDH 接口（`PdhAddEnglishCounterW`，中文系统同样有效）得到：

| 项目 | 实测值 | 页面表现 |
| --- | --- | --- |
| CPU | Ryzen AI 9 H 365，10C/20T，标称 2000 MHz | 处理器卡片：使用率 / 频率 / 基准频率 / 温度全部出数 |
| 温度 | 热区 322.00 K ≈ 48.9 °C | 与参考截图 48 °C 一致 |
| 功耗 | Energy Meter `Apu Power` = 14787.83 mW | 卡片右上角显示 ≈ 14.8 W |
| 内存 | TotalVisible 32,810,356 KB ≈ 31.3 GB | `26.5 GB/31.3 GB` 同款排版 |
| 卷 | C: Windows NTFS 299.0 GB；D: Data NTFS 652.9 GB | `220.1 GB/299.0 GB`、`174.5 GB/652.9 GB` |
| 显卡 | 无 NVIDIA 设备（NVAPI 无返回），`GPU Engine` 存在 `engtype_3D` 实例 | 走 PDH 回退，显存行自动隐藏，右上角标 `核显 · WDDM` |
| 风扇 | 取决于 `config/ec_registry.json` 是否覆盖该模具 | 未映射时显示 `-- RPM` + `EC 未映射` |

## 3. 兼容范围与降级策略

- **系统**：Windows 10 1809+ / Windows 11 x64；PDH 使用英文计数器路径，中文、英文、其他语言系统均可用。
- **机型**：独显机型（NVAPI）与核显机型（PDH）同一套组件；无风扇传感器机型自动显示 `-- RPM`。
- **容量**：`f64` GB 换算，2 TB 以上卷不溢出；BitLocker 未解锁卷读取失败时整卷跳过，不抛错。
- **主题**：新增样式全部落在 `src/index.css`，并补充 `[data-theme="mono"]` 单色工业风覆盖；组件内无硬编码色值。
- **降级**：命令不可用（浏览器预览 / 旧内核）时页面回退演示数据并打 `演示数据` 角标，不会白屏。
- **性能**：内存 / 卷容量 1.2 s 轮询（仅页面可见时挂载），PDH 查询单例复用，卷容量 3 s 缓存。

## 4. 构建

```powershell
pnpm install
pnpm build          # 前端 tsc + vite
pnpm tauri:dev      # 桌面端联调（需 Rust 工具链）
```
