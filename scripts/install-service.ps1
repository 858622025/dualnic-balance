# 安装并启动 DualNICBalance 为 LocalSystem Windows 服务。
# 需以管理员运行 PowerShell：右键 PowerShell → 以管理员身份运行。
# 回滚：本脚本在任何失败点都会清理半成品（sc create 失败→删除；start 失败→停+删）。
# -Exe：服务 exe 路径（GUI 设置页传入「与 GUI 同目录」的服务 exe）；缺省按开发目录找。
# -Log：可选，Transcript 日志路径（GUI 传入；失败时 GUI 读它把真实报错显示到界面）。
param([string]$Exe = '', [string]$Log = '')

if ($Log) { try { Start-Transcript -Path $Log -Force | Out-Null } catch {} }

try {
    $ErrorActionPreference = 'Stop'
    $Name = 'DualNICBalance'
    $DisplayName = 'DualNIC Balance Service'
    $Description = '双网卡分流常驻服务：路由对账/自愈、环境自动匹配、事件日志 + 本机 IPC（LocalSystem）'

    if ([string]::IsNullOrWhiteSpace($Exe)) {
        $Root = Split-Path -Parent $PSScriptRoot
        $Exe = Join-Path $Root 'target\release\dualnic-service.exe'
    }
    if (-not (Test-Path $Exe)) {
        throw "未找到 $Exe。请确认 dualnic-service.exe 与 dualnic-gui.exe 在同一目录，或用 -Exe 指定路径。"
    }

    # 清掉残留的本工具进程（首启的临时后台引擎是提权运行的，必须在这里杀），
    # 否则它占着 IPC 端口，装好的服务起不来。
    # 注意：不能用 taskkill+2>$null——PS 5.1 下原生 stderr 重定向会变成 ErrorRecord，
    # 配合 $ErrorActionPreference='Stop' 直接终止脚本（进程不存在时必炸）。
    Write-Host '>> 清理残留的本工具进程（临时引擎）'
    Get-Process dualnic-service -ErrorAction SilentlyContinue | Stop-Process -Force

    Write-Host ">> 创建服务 $Name（binPath=$Exe）"
    & sc.exe create $Name binPath= "`"$Exe`"" start= auto obj= LocalSystem DisplayName= "`"$DisplayName`""
    if ($LASTEXITCODE -ne 0) {
        Write-Host "sc create 失败（退出码 $LASTEXITCODE）。若服务已存在，请先运行 uninstall-service.ps1 再重试。" -ForegroundColor Red
        exit 1
    }

    & sc.exe description $Name $Description | Out-Null

    Write-Host '>> 配置崩溃自恢复（进程失败 60 秒后自动重启，每日重置计数）'
    & sc.exe failure $Name reset= 86400 actions= restart/60000/restart/60000/restart/60000 | Out-Null

    Write-Host '>> 启动服务'
    & sc.exe start $Name | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Host '启动失败，回滚删除服务…' -ForegroundColor Yellow
        & sc.exe stop $Name | Out-Null
        & sc.exe delete $Name | Out-Null
        throw '服务启动失败且已删除。请查看 %ProgramData%\DualNIC Balance\logs\ 日志排查。'
    }

    Write-Host '>> 当前状态：'
    & sc.exe query $Name
    Write-Host '完成。配置与事件日志存 SQLite：%ProgramData%\DualNIC Balance\dualnic.db。'
    exit 0
} catch {
    Write-Host "安装脚本异常：$_"
    exit 1
} finally {
    if ($Log) { try { Stop-Transcript | Out-Null } catch {} }
}
