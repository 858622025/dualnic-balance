# One-shot full reset: stop+delete service, restore metrics, remove autostart task,
# delete ProgramData and temp scripts. ASCII-only, machine-independent (keep UTF-8 BOM for PS 5.1).
$ErrorActionPreference = 'Continue'
$Root = Split-Path -Parent $PSScriptRoot   # 仓库根 = scripts 的上一级（不硬编码盘符路径）
$log = Join-Path $Root 'target\reset-system.log'
New-Item -ItemType Directory -Force -Path (Split-Path $log) | Out-Null
Start-Transcript -Path $log -Force | Out-Null

Write-Host '>> [1/6] stop service'
sc.exe stop DualNICBalance | Out-Null
$deadline = (Get-Date).AddSeconds(20)
$stopped = $false
do {
    Start-Sleep -Milliseconds 500
    if (sc.exe query DualNICBalance | Select-String 'STOPPED') { $stopped = $true }
} until ($stopped -or (Get-Date) -gt $deadline)
Write-Host "   stopped=$stopped"

Write-Host '>> [2/6] restore fixed interface metrics to automatic (machine-independent)'
# 不认网卡名：恢复**所有**被固化 metric 的 IPv4 接口（精确的「撤销固化」语义，换机器同样有效）
$fixed = Get-NetIPInterface -AddressFamily IPv4 |
    Where-Object { $_.AutomaticMetric -eq 'Disabled' }
$fixed | Set-NetIPInterface -AutomaticMetric Enabled
$restored = if ($fixed) { ($fixed | ForEach-Object InterfaceAlias) -join ', ' } else { '(none was fixed)' }
Write-Host "   restored automatic metric on: $restored"

Write-Host '>> [3/6] delete service'
sc.exe delete DualNICBalance

Write-Host '>> [4/6] unregister autostart scheduled task'
Unregister-ScheduledTask -TaskName 'DualNICBalanceGUI' -Confirm:$false -ErrorAction SilentlyContinue

Write-Host '>> [5/6] delete ProgramData + temp setup scripts'
$DataDir = Join-Path $env:ProgramData 'DualNIC Balance'
Remove-Item $DataDir -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item (Join-Path $env:TEMP 'dualnic-balance-setup') -Recurse -Force -ErrorAction SilentlyContinue

Write-Host '>> [6/6] verify'
Write-Host ("service exists : " + [bool](sc.exe query DualNICBalance 2>$null | Select-String 'SERVICE_NAME'))
Write-Host ("task exists    : " + [bool](Get-ScheduledTask -TaskName 'DualNICBalanceGUI' -ErrorAction SilentlyContinue))
Write-Host ("programdata    : " + (Test-Path $DataDir))
Write-Host ("temp scripts   : " + (Test-Path (Join-Path $env:TEMP 'dualnic-balance-setup')))

Stop-Transcript | Out-Null
