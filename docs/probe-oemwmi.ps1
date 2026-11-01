$ErrorActionPreference = "Continue"
$inst = Get-CimInstance -Namespace root/wmi -ClassName OemWMIMethod | Where-Object { $_.InstanceName -like "*HWMI_0" } | Select-Object -First 1
if (-not $inst) { Write-Output "需要管理员权限运行（实例枚举为空）"; exit 1 }
foreach ($cmd in 1..4) {
  foreach ($sub in 1..12) {
    $b = New-Object byte[] 64
    $b[0] = $cmd; $b[1] = $sub
    $r = Invoke-CimMethod -InputObject $inst -MethodName OemWMIfun -Arguments @{ u8Input = $b } -ErrorAction SilentlyContinue
    $o = $r.u8Output
    "[$cmd,$sub] rc=$($r.u32Resrved) out=" + (($o[0..7]) -join ",")
  }
}
