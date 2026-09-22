# One-shot: stop service -> cargo build release (product crates) -> start service.
# ASCII-only on purpose (PS 5.1 + no-BOM UTF8 pitfall). Output -> target\rebuild-svc.log
$ErrorActionPreference = 'Continue'
# repo root = parent of scripts dir; $PSScriptRoot can be empty in some hosts -> fall back to $MyInvocation
$scriptDir = if ($PSScriptRoot) { $PSScriptRoot } else { Split-Path -Parent $MyInvocation.MyCommand.Definition }
$Root = Split-Path -Parent $scriptDir
$log = Join-Path $Root 'target\rebuild-svc.log'
New-Item -ItemType Directory -Force -Path (Split-Path $log) | Out-Null
Start-Transcript -Path $log -Force | Out-Null

$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
Set-Location $Root

Write-Host '>> sc stop DualNICBalance'
sc.exe stop DualNICBalance | Out-Null
$deadline = (Get-Date).AddSeconds(20)
do {
    Start-Sleep -Milliseconds 500
    $s = (sc.exe query DualNICBalance | Select-String 'STOPPED').ToString()
} until ($s -or (Get-Date) -gt $deadline)
Write-Host ">> service stopped: $([bool]$s)"

Write-Host '>> cargo build'
cargo build --release -p dualnic-core -p dualnic-service -p dualnic-gui
Write-Host ">> cargo exit: $LASTEXITCODE"

Write-Host '>> copy built exe to service install dir'
# Copy built exe to the registered service binPath dir; keep this script ASCII-only
# (no-BOM UTF-8 Chinese comments are misparsed as ANSI by PS 5.1)
$svcPath = (Get-CimInstance Win32_Service -Filter "Name='DualNICBalance'").PathName
if (-not $svcPath) {
    Write-Host '!! service not found or query failed, skip copy'
} else {
    $svcPath = $svcPath.Trim('"')
    $installDir = Split-Path -Parent $svcPath
    Copy-Item -Force (Join-Path $Root 'target\release\dualnic-service.exe') $svcPath
    Write-Host ">> copied dualnic-service.exe -> $svcPath"
    $guiPath = Join-Path $installDir 'dualnic-gui.exe'
    if (Test-Path $guiPath) {
        Copy-Item -Force (Join-Path $Root 'target\release\dualnic-gui.exe') $guiPath
        Write-Host ">> copied dualnic-gui.exe -> $guiPath"
    }
}

Write-Host '>> sc start DualNICBalance'
sc.exe start DualNICBalance | Out-Null
Start-Sleep -Seconds 2
sc.exe query DualNICBalance

Stop-Transcript | Out-Null
