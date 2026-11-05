# 厂商控制总线（AiStone MQTT）实测

> 机型：MECHREVO XINGYAO Series（Ryzen AI 9 HX 365 + Radeon 880M，STX_BIOS 1.15）
> 目的：在尚无直接风扇写入映射（见 `xingyao-wmi-map.md`）的机型上，找到官方风扇/背光/模式的真实控制面。

## 1. 总线

官方控制中心（`GCUService.exe`）在本机跑一个 **MQTT broker**：

```
host  127.0.0.1
port  13688
用户  UWPClient_User_5
密码  UWPClient_Pwd888881772688_5
客户端 ID 必须是 UWPClient_<n>（用别的 ID 会 CONNACK rc=2 identifier rejected）
QoS  2（发布/订阅）
```

凭据与地址来自官方 UI 反编译：`ccu-decomp/DataService/MQTTDataService.cs:135-154`。

## 2. 主题（topic）

| 方向 | topic | 说明 |
| --- | --- | --- |
| 命令 | `Fan/Control` | 风扇/模式/曲线 |
| 状态 | `Fan/Status` | OperatingMode、FanBoostEnable、CPU_PL1… |
| 状态 | `Fan/Table` | 当前风扇曲线（Activated/Name/CPU/GPU） |
| 命令/状态 | `Keyboard/Ctrl`、`Keyboard/Status` | 键盘背光 |
| 命令/状态 | `System/Control`、`Setting/Control` | 其它整机设置 |

## 3. 命令（payload JSON，Action 取值来自 `RamFanMode1p5` 枚举）

```json
{"Action":"GETSTATUS"}
{"Action":"GET_FAN_SPEED_CURVE_SETTING"}
{"Action":"FAN_BOOST_ON"}
{"Action":"FAN_BOOST_OFF"}
{"Action":"OPERATING_OFFICE_MODE"}   // 还有 OPERATING_GAMING_MODE / OPERATING_TURBO_MODE / OPERATING_CUSTOM_MODE
{"Action":"SET_FAN_SPEED_CURVE_SETTING","Name":"OpenRevo Custom","Type":"CPU","T0":"0","T1":"30", ... "T15":"55"}
{"Action":"RESTORE_FAN_SPEED_CURVE_SETTING","Name":"..."}
```

`Type` 只能 `CPU` 或 `GPU`；`T0..T15` 为 16 个占空比百分比（字符串）。

## 4. 已验证（2026-11-01）

```
GETSTATUS  -> Fan/Status: {"OperatingMode":1,"FanBoostEnable":"0","IsAC":true,...}
GET_FAN_SPEED_CURVE_SETTING -> Fan/Table: {"Activated":false,"Name":null,"CPU":null,"GPU":null}
FAN_BOOST_ON  -> 再读 Fan/Status: FanBoostEnable = "1"
FAN_BOOST_OFF -> 再读 Fan/Status: FanBoostEnable = "0"
```

也就是说：**写入路径可用且可逆**，风扇曲线（SET_FAN_SPEED_CURVE_SETTING）与模式切换走同一总线。

## 5. OpenRevo 侧的接入

| 功能 | 状态来源 | 确认方式 |
| --- | --- | --- |
| 系统监控 → 强冷 | `Fan/Status.FanBoostEnable` | 命令回包或一次 `GETSTATUS` 必须与目标值匹配 |
| 系统设置 → 键盘背光 | `Setting/Status.SingleColorKBBL`、`CloseTimer` | 只显示已读到的值；写入不能确认时保留原状态并报错 |
| 背光延时 | `KEYBOARD_LIGHTBAR_TIMER_ON` / `OFF` | 字段名为 `Mins`，实际单位为秒 |

定时档位按厂商 UI 的映射提供：10/15/20/30/45 秒，1/2/5/10/20/30 分钟，
1/2/3 小时，以及永不。按本机能力，厂商模式不提供手动风扇曲线或显卡模式切换。

## 6. 连接与接管是两种互斥模式

- 「连接」只启动已安装的厂商后台；「连接并常开」还将 `GCUBridge` 设为 Windows 自动启动。
  若已注册按需启动的 `ACPIDriver`，先启动驱动，再启动后台；不安装驱动、不恢复官方 UI 或官方任务。
- 连接成功后记录厂商模式并清除旧的本地接管标记。OpenRevo 下次启动不再停掉该服务，
  同时跳过会争用硬件的启动恢复写入。退出页面或暂时收不到状态不会改变选定模式。
- 「关闭常开」将后台改回按需启动，不会立即中止已连接的服务。
  若确实要改回本地接管，可用托盘「接管/恢复 官方后台服务」。
- 如果只想继续看监控，厂商服务未安装也不影响 CPU/GPU/风扇 RPM 的独立采集。

## 7. 未打通：自定义风扇表（2026-11-01 实测）

按官方 UI 的写法发送，**表能发出去但不会激活**：

```
GETSTATUS                       -> OperatingMode 1
OPERATING_CUSTOM_MODE           -> OperatingMode 仍为 1（模式未切换）
SET_FAN_SPEED_CURVE_SETTING
  Name=M1T1 / DefaultFanTable_Office, Type=CPU, T0..T15
                                -> Fan/Table 仍为 Name=null, Activated=false
RESTORE_FAN_SPEED_CURVE_SETTING_ALL
```

结论：这台机型上「切自定义模式 + 写曲线」被服务端校验拦掉了（可能要求官方 UI 会话、机型白名单或走 `Setting/Control` 的另一条链路），
所以 open-revo 目前只对外暴露已验证可用的 `FAN_BOOST_ON/OFF`（强冷）。
`vendor_bus::apply_fan_preset` 保留在代码里但未接到界面，等把服务端校验摸清再启用。

## 8. 时延与故障处理

- 打开监控页只查 `Fan/Status`；打开系统设置只查 `Setting/Status`。
  常规轮询不读未启用的风扇曲线。MQTT 连接复用，命令逐条发送，不以无关或保留的消息判定在线。
- TCP 连接限时 300 ms，单次状态等待限时 600 ms。写入没有匹配回执时只补发一次只读查询，
  不重放控制命令；驱动或服务启动失败时显示系统返回错误。
- 首页不访问总线。监控或系统设置可见时，在线约 15 秒刷新；离线约 30 秒重试。
  只有选择了厂商模式且启用常开，才会对确认已停止的后台尝试恢复；不创建重启任务，
  不保证不稳定的 OEM 进程始终存活。
- 厂商通道是现有 Windows OEM 服务的兼容路径，不代表已经实现无厂商依赖的硬件写入。


## 9. 2026-11-05 真机复核

- broker 接受客户端身份不代表控制进程会处理它的命令。测试客户端
  `UWPClient_9` 可以完成连接却未得到设备状态；官方身份的同协议只读测试以及
  OpenRevo 生产 Rust 模块均读到了 `Fan/Status` 和 `Setting/Status`。
- 仅重启 GCUBridge 会遗留独立 GCUService 进程；界面新增的「重启后台」是显式操作：
  停 broker 后核实 GCUService 的完整安装路径，只结束对应工作进程，再启动后台。
  不停止 ACPI 驱动、不恢复官方 UI，不在自动轮询中终止工作进程。
- 强冷 A/B 测试能读回 0→1→0；实际风扇在约 3200 RPM 附近，开启阶段 12 秒没有明显升高。
  因此 UI 使用「强冷请求」，不把服务确认标记当作风扇已达到全速。
