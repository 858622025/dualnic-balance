# 停止并删除 DualNICBalance Windows 服务。
# 需以管理员运行 PowerShell。
# 注意：本操作会删除服务注册，若有后续对账数据/日志请先自行备份。
# 退出码：0=已删除；1=异常；2=删除后服务仍存在（GUI 据此给出准确提示）。
# -Log：可选，Transcript 日志路径（GUI 传入；失败时 GUI 读它把真实报错显示到界面）。
param([string]$Log = '')

if ($Log) { try { Start-Transcript -Path $Log -Force | Out-Null } catch {} }

try {
    $ErrorActionPreference = 'Continue'
    $Name = 'DualNICBalance'

    & sc.exe stop $Name | Out-Null
    # 等待服务停止（最多 15 秒）；服务本就不存在（1060）直接视为已删
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline) {
        $null = & sc.exe query $Name 2>$null
        if ($LASTEXITCODE -eq 1060) { break }
        $q = (& sc.exe query $Name 2>$null) -join ' '
        if ($q -match 'STOPPED') { break }
        Start-Sleep -Milliseconds 300
    }
    & sc.exe delete $Name | Out-Null
    # 临时引擎（--console 提权进程）不受 SCM 管理，一并清掉
    # （不用 taskkill+2>$null：原生 stderr 重定向在 EA=Stop/Continue 下都会往错误流里塞噪音）
    Get-Process dualnic-service -ErrorAction SilentlyContinue | Stop-Process -Force
    # 终判：查询不到（1060）才算卸载成功
    $null = & sc.exe query $Name 2>$null
    if ($LASTEXITCODE -eq 1060) {
        Write-Host "已停止并删除服务 $Name。"
        exit 0
    }
    Write-Host "卸载后服务仍存在，请查看上方 sc 输出（可能被失败恢复策略拉回）。"
    exit 2
} catch {
    Write-Host "卸载脚本异常：$_"
    exit 1
} finally {
    if ($Log) { try { Stop-Transcript | Out-Null } catch {} }
}
