# XINGYAO / Strix Point 机型 WMI 通道实测（读命令全表）

> 目标：判断 open-revo 能否在这台 XINGYAO Series（AMD Ryzen AI 9 HX 365 + Radeon 880M，STX_BIOS 1.15）上实现
> 风扇调速、键盘背光档位、显卡模式切换。结论：**已登记的 WMI 通道能读取转速，并有功耗模式、背光休眠的机型配置；尚无已验证的直接风扇调速/强冷写入映射**，详见第 3、6 节。

## 1. 通道形态

```
root\wmi : OemWMIMethod
  instance : ACPI\PNP0C14\HWMI_0   (非管理员枚举不到实例，必须以管理员调用)
  method   : OemWMIfun( u8Input: UInt8Array[in], u32Resrved: UInt32[out], u8Output: UInt8Array[out] )
  buffer   : in 64 字节 / out 256 字节（out[0] - out[7] 为返回数据，out[0]=1 表示该命令不支持）
```

调用样例（管理员 PowerShell）：

```powershell
$inst = Get-CimInstance -Namespace root/wmi -ClassName OemWMIMethod | Where-Object { $_.InstanceName -like '*HWMI_0' } | Select-Object -First 1
$buf = New-Object byte[] 64; $buf[0] = 2; $buf[1] = 1
$r = Invoke-CimMethod -InputObject $inst -MethodName OemWMIfun -Arguments @{ u8Input = $buf }
$r.u8Output[0..7]
```

## 2. 实测命令表（2026-11-01，只读扫描 1..4 × 1..12）

| 命令 | 返回 | 判定 |
| --- | --- | --- |
| `[1,1]` | `0,4,0,2,...` | 有应答，疑似固件/平台信息 |
| `[2,1]` | `0,lo,hi,...` | **CPU 风扇转速** `u16_le @ offset1`（实测 4083 / 4391 RPM 与界面一致） |
| `[3,7]` | `0,1,...` | 有应答，布尔状态（未定语义） |
| `[3,8]` | `143,0,...` | 有应答，单字节状态（未定语义） |
| `[3,9]` | `0,2,...` | **当前性能模式**（2 = 均衡），与 manifest 的 `get_power_mode` 一致 |
| `[3,10]` | `1,...` | 写模式时需要带参：`[3,10,1/2/3]`（office/balance/turbo） |
| `[3,11]` | `0,88,2,...` | 有应答（疑似温度/档位组合，未定语义） |
| 其余 `[1,3..12]`、`[2,2..12]`、`[3,1..6]`、`[4,*]` | `1,0,...` | **不支持**（out[0]=1） |

关键点：**`[2,x]` 族里除 `[2,1]` 读转速外全部不支持**，也就是说这个 WMI 邮箱没有“设置风扇占空比/转速”的命令；
EC RAM 直读（厂商服务里的 SMRW 协议：`cmd 0xBB 读 / 0xAA 写`，地址放 offset 8）在这台机器上返回 `1`，同样不支持。

## 3. 厂商是怎么做到的

官方的风扇/背光控制不在这个 WMI 邮箱里，而在自家服务栈：

```
C:\Program Files\OEM\机械革命控制中心\AiStoneService\MyControlCenter\
├─ GCUService.exe       (.NET，Confuser 类混淆：方法体被替换为 throw，IL 已加密)
├─ ACPIDriverDll.dll     ← 厂商驱动封装
├─ GCUBridge.exe         ← 服务宿主（默认 Stopped，需要管理员启动）
├─ Config\DeviceBase64.dll  (264 KB，加密的机型配置)
└─ UserFanTables\*.json  ← 16 点风扇表（UpT / DownT / Duty，ID 0..15）
```

UI（`CCU.WinUI.msix` → `CCUWinUI.dll`）与服务的通信是 WCF：

```csharp
[ServiceContract] interface IWCFService {
    Task StartSendingToClient(string data);
    void SendToServer(string Topic, string Command);   // Topic 形如 "System/Control"、"Setting/Control"
}
```

即：**官方通过 GCUService → 厂商驱动/EC 写风扇表**，与 open-revo 走的通用 WMI 不是同一条路。

## 4. 对 open-revo 的意义

- `resources/drivers/xingyao_14_p916f.json` 里 `fan_custom_curve: false`、`rgb_keyboard.type: none`、`discrete_gpu: false` 是**如实声明**，不是漏配；
- 想在这台机器上做风扇曲线/背光档位，只有两条路：
  1. 逆向 `ACPIDriverDll.dll` + 驱动 IOCTL（工程量大，且属于写硬件，必须先做只读验证）；
  2. 调用官方 `GCUService` 的 WCF 总线（`SendToServer("Fan/...", json)`），依赖官方服务在跑，属于“借用厂商栈”。
- 在拿到确定的写入字节/IOCTL 之前，open-revo 不应发送任何写命令；现有 `hardware blast shield` 也应保持默认拦截。

## 5. 复现脚本（只读，可反复运行）

见 `docs/probe-oemwmi.ps1`（随本文件提交），输出与第 2 节一致。


## 6. 2026-11-05 复核：独立监控与控制缺口

- AMD Radeon 880M 的温度、3D 节点频率、内存频率可以通过 Windows WDDM
  `D3DKMTQueryAdapterInfo` 读取，共享显存用量来自按适配器 LUID 选择的 PDH
  `GPU Adapter Memory / Shared Usage`，无需 GCUBridge、GCUService 或 AMD SDK。
- 本机 manifest 已有 `set_power_mode`、`set_kb_sleep_timeout`，因此不能概括为
  “WMI 完全没有写通道”；但这不等于有直接强冷/风扇曲线/任意分钟背光控制。
- 厂商服务静态检查确认 GCUService 1.0.2.70 为 .NET Framework 4.8 程序。
  现有反编译中的关键风扇方法仍是运行时保护占位，尚未获得本机直接强冷写入帧。
  不把其他模具的 EC 地址或 IOCTL 硬套到 XINGYAO。
- 真机 A/B：服务强冷标记可从 0 → 1 → 0，物理 RPM 约 3200 且没有随标记明显升高。
  本轮没有把服务标记等同于物理强冷成功；具体样本记录在本地 validation 日志中。
