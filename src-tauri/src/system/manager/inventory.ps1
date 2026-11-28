$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$warnings = [Collections.Generic.List[string]]::new()
switch ($env:OPENREVO_MANAGER_QUERY) {
    'apps' {
        $rows = [Collections.Generic.List[object]]::new()
        $paths = @(
            'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
        )
        foreach ($path in $paths) {
            foreach ($p in @(Get-ItemProperty -Path $path -ErrorAction SilentlyContinue)) {
                if (!$p.DisplayName -or $p.SystemComponent -eq 1) { continue }
                $scope = if ($path.StartsWith('HKCU')) { 'user' } else { 'machine' }
                $rows.Add(@{
                    name = [string]$p.DisplayName
                    publisher = [string]$p.Publisher
                    version = [string]$p.DisplayVersion
                    kind = 'desktop'
                    scope = $scope
                })
            }
        }
        try {
            foreach ($p in (Get-AppxPackage | Where-Object { !$_.IsFramework -and !$_.IsResourcePackage })) {
                $rows.Add(@{name=[string]$p.Name; publisher=[string]$p.PublisherId; version=[string]$p.Version; kind='msix'; scope='user'})
            }
        } catch {
            $warnings.Add('AppX inventory unavailable; showing registered packaged Start apps where available: ' + $_.Exception.Message)
            try {
                foreach ($a in (Get-StartApps | Where-Object { $_.AppID -match '^[^!]+_[a-zA-Z0-9]{13}![^!]+$' })) {
                    $rows.Add(@{name=[string]$a.Name; publisher=''; version=''; kind='start_msix'; scope='user'})
                }
            } catch { $warnings.Add('Packaged Start apps: ' + $_.Exception.Message) }
        }
        $data = @{ rows=@($rows | Sort-Object -Property { $_.name },{ $_.version },{ $_.scope } -Unique | Select-Object -First 3000); warnings=$warnings.ToArray() }
    }
    'startup' {
        $rows = [Collections.Generic.List[object]]::new()
        foreach ($entry in @(
            @{base='HKLM:\Software\Microsoft\Windows\CurrentVersion'; scope='global'},
            @{base='HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion'; scope='global32'},
            @{base='HKCU:\Software\Microsoft\Windows\CurrentVersion'; scope='user'}
        )) {
            foreach ($key in @('Run','RunOnce')) {
                if ($entry.scope -eq 'user' -and $key -eq 'Run') { continue }
                if (!(Test-Path -LiteralPath ($entry.base + '\' + $key))) { continue }
                try {
                    $values = Get-ItemProperty -LiteralPath ($entry.base + '\' + $key)
                    foreach ($item in $values.PSObject.Properties) {
                        if ($item.Name -in @('PSPath','PSParentPath','PSChildName','PSDrive','PSProvider')) { continue }
                        $rows.Add(@{name=[string]$item.Name; source=($entry.scope + ':' + $key); detail=[string]$item.Value; state='registered'})
                    }
                } catch { $warnings.Add('Startup registry: ' + $_.Exception.Message) }
            }
        }
        try {
            $folder = [Environment]::GetFolderPath('CommonStartup')
            if ($folder -and (Test-Path -LiteralPath $folder)) {
                foreach ($item in (Get-ChildItem -LiteralPath $folder -File)) {
                    if ($item.Name -eq 'desktop.ini') { continue }
                    $rows.Add(@{name=$item.Name; source='common-folder'; detail=$item.FullName; state='registered'})
                }
            }
        } catch { $warnings.Add('Common startup folder: ' + $_.Exception.Message) }
        try {
            foreach ($t in (Get-ScheduledTask)) {
                $start = @($t.Triggers | Where-Object {
                    $_.CimClass.CimClassName -in @('MSFT_TaskBootTrigger','MSFT_TaskLogonTrigger')
                })
                if (!$start.Count) { continue }
                $rows.Add(@{name=[string]$t.TaskName; source='task'; detail=[string]$t.TaskPath; state=[string]$t.State})
            }
        } catch { $warnings.Add('Task Scheduler: ' + $_.Exception.Message) }
        try {
            foreach ($s in (Get-CimInstance Win32_Service -Filter "StartMode='Auto'" -OperationTimeoutSec 5)) {
                $rows.Add(@{name=[string]$s.DisplayName; source='service'; detail=[string]$s.Name; state=[string]$s.State})
            }
        } catch { $warnings.Add('Windows services: ' + $_.Exception.Message) }
        $data = @{rows=@($rows | Select-Object -First 3000); warnings=$warnings.ToArray()}
    }
    'health' {
        $u = @()
        try {
            $searcher = (New-Object -ComObject Microsoft.Update.Session).CreateUpdateSearcher()
            $count = [Math]::Min([int]$searcher.GetTotalHistoryCount(), 64)
            if ($count -gt 0) {
                $u = @($searcher.QueryHistory(0, $count) | Where-Object {
                    [int]$_.Operation -eq 1 -and [int]$_.ResultCode -in @(2,3)
                } | Select-Object -First 12 | ForEach-Object {
                    @{ time_ms=([DateTimeOffset]([datetime]$_.Date)).ToUnixTimeMilliseconds(); title=[string]$_.Title; result=[int]$_.ResultCode; source='WUA' }
                })
            }
        } catch { $warnings.Add('Windows Update history: ' + $_.Exception.Message) }
        if (!$u.Count) {
            try {
                $u = @(Get-WinEvent -FilterHashtable @{ LogName='System'; ProviderName='Microsoft-Windows-WindowsUpdateClient'; Id=19; StartTime=(Get-Date).AddDays(-90) } -MaxEvents 12 |
                    Where-Object { $_.TimeCreated } | ForEach-Object {
                        $title = if ($_.Properties.Count -gt 0) { [string]$_.Properties[0].Value } else { '' }
                        @{ time_ms=([DateTimeOffset]$_.TimeCreated).ToUnixTimeMilliseconds(); title=$title; result=2; source='WindowsUpdateClient' }
                    })
            } catch { if ($_.FullyQualifiedErrorId -notmatch '^NoMatchingEventsFound') { $warnings.Add('Windows Update event fallback: ' + $_.Exception.Message) } }
        }
        $data = @{ updates=$u; warnings=$warnings.ToArray() }
    }
    'events' {
        $hours = switch ($env:OPENREVO_MANAGER_RANGE) { '24' {24}; '168' {168}; default {1} }
        $since = (Get-Date).AddHours(-$hours)
        $rules = @(
            @{ source='Display'; log='System'; id=4101; kind='display' },
            @{ source='Microsoft-Windows-WER-SystemErrorReporting'; log='System'; id=1001; kind='bugcheck' },
            @{ source='Microsoft-Windows-Kernel-Power'; log='System'; id=41; kind='power' },
            @{ source='EventLog'; log='System'; id=6008; kind='shutdown' },
            @{ source='Application Error'; log='Application'; id=1000; kind='application' },
            @{ source='Application Hang'; log='Application'; id=1002; kind='hang' },
            @{ source='Microsoft-Windows-WindowsUpdateClient'; log='System'; id=19; kind='update' },
            @{ source='Microsoft-Windows-WindowsUpdateClient'; log='System'; id=20; kind='update_error' }
        )
        $rows = [Collections.Generic.List[object]]::new()
        foreach ($rule in $rules) {
            try {
                $f = @{ LogName=$rule.log; ProviderName=$rule.source; Id=$rule.id; StartTime=$since }
                foreach ($e in (Get-WinEvent -FilterHashtable $f -MaxEvents 48 -ErrorAction Stop)) {
                    $detail = ''
                    if ($e.Properties.Count -gt 0 -and $rule.kind -in @('application','hang','display','bugcheck','update','update_error')) {
                        $detail = [string]$e.Properties[0].Value
                        if ($detail.Length -gt 200) { $detail = $detail.Substring(0, 200) }
                    }
                    if (!$e.TimeCreated) { continue }
                    $rows.Add(@{ time_ms=([DateTimeOffset]$e.TimeCreated).ToUnixTimeMilliseconds(); kind=$rule.kind; source=$rule.source;
                        event_id=[int]$e.Id; record=[long]$e.RecordId; detail=$detail })
                }
            } catch {
                if ($_.FullyQualifiedErrorId -notmatch '^NoMatchingEventsFound') {
                    $warnings.Add($rule.source + '/' + $rule.id + ': ' + $_.Exception.Message)
                }
            }
        }
        $data=@{rows=@($rows | Sort-Object -Property { $_.time_ms } -Descending); warnings=$warnings.ToArray()}
    }
    default { throw 'Unknown local OpenRevo provider' }
}
ConvertTo-Json -InputObject $data -Compress -Depth 5
