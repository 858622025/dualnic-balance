# 注册计划任务：用户登录后 30 秒自启动 DualNIC Balance GUI。
# 以当前用户身份运行（普通权限，GUI 免提权）；无需管理员。
# 卸载：运行 uninstall-gui-autostart.ps1。
# -Exe：GUI exe 路径（GUI 设置页传 current_exe）；缺省按开发目录找。
param([string]$Exe = '')

$ErrorActionPreference = 'Stop'
$TaskName = 'DualNICBalanceGUI'
if ([string]::IsNullOrWhiteSpace($Exe)) {
    $Root = Split-Path -Parent $PSScriptRoot
    $Exe = Join-Path $Root 'target\release\dualnic-gui.exe'
}
if (-not (Test-Path $Exe)) {
    Write-Error "未找到 $Exe。请确认 dualnic-gui.exe 存在，或用 -Exe 指定路径。"
}

$Action = New-ScheduledTaskAction -Execute $Exe -WorkingDirectory (Split-Path -Parent $Exe)
$Trigger = New-ScheduledTaskTrigger -AtLogOn
# 登录后延迟 30 秒（ISO8601 duration），避开开机网络/服务未就绪的窗口
$Trigger.Delay = 'PT30S'
# ExecutionTimeLimit=PT0S：取消默认「72 小时后强制结束任务」，否则计划任务会把长驻 GUI 杀掉
$Settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -StartWhenAvailable

Register-ScheduledTask -TaskName $TaskName -Action $Action -Trigger $Trigger -Settings $Settings `
    -Description 'DualNIC Balance GUI：登录后 30 秒自启动' -Force | Out-Null

Write-Host "已注册登录自启动任务「$TaskName」（用户登录后 30 秒启动 GUI）。"
Write-Host "立即验证可手动运行：Start-ScheduledTask -TaskName $TaskName"
