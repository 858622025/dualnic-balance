# 注销 GUI 登录自启动计划任务。
$TaskName = 'DualNICBalanceGUI'
Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
Write-Host "已注销登录自启动任务「$TaskName」（若原本不存在，错误可忽略）。"
