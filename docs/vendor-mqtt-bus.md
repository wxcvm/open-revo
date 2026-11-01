# 厂商控制总线（AiStone MQTT）实测

> 机型：MECHREVO XINGYAO Series（Ryzen AI 9 HX 365 + Radeon 880M，STX_BIOS 1.15）
> 目的：在 WMI 邮箱没有写通道（见 `xingyao-wmi-map.md`）的机型上，找到官方风扇/背光/模式的真实控制面。

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

## 5. open-revo 侧的接入

`src-tauri/src/system/vendor_bus.rs`：不引第三方 MQTT 库，直接手写 CONNECT / SUBSCRIBE / PUBLISH 报文。

命令：

| 命令 | 作用 | 保护 |
| --- | --- | --- |
| `vendor_bus_probe` | 只读：Fan/Status + Fan/Table | — |
| `vendor_fan_boost(enabled)` | 强冷开关 | 仅发 FAN_BOOST_ON/OFF |
| `vendor_set_power_mode(mode)` | 1 办公 / 2 均衡 / 3 狂暴 / 4 自定义 | 限制 1..4 |
| `vendor_set_fan_table(target, duty, name)` | 16 点风扇曲线 | duty 必须 16 项且 0..100，target 仅 CPU/GPU |

服务未启动时 `vendor_bus_probe` 直接返回 `vendor bus offline`，前端显示「厂商通道 离线」，不会发送任何报文。

## 6. 前置条件

厂商 broker 由 `GCUBridge` 服务托管（默认 Stopped）：

```powershell
Start-Service GCUBridge      # 需要管理员
```

启动后 open-revo 的「系统监控 → 风扇转速」卡片会显示 `厂商通道 在线 · localhost:13688` 并出现「强冷」开关。

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
