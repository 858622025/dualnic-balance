@echo off
rem ============================================================
rem  DualNIC Balance 一键安装 / 卸载
rem   - 双击运行：弹出菜单（1 安装 / 2 卸载 / 3 卸载并清数据 / 0 退出），
rem     每次操作完成后返回菜单，选 0 才退出
rem   - 命令行：setup.bat install / uninstall / purge（执行完直接退出）
rem   - 需要管理员权限：非管理员运行时会自动弹 UAC 重新拉起自己
rem   - 必须与 dualnic-service.exe / dualnic-gui.exe 同目录
rem  注意：本文件必须保存为 ANSI(GBK) 编码，中文 echo 才能正常显示；
rem        不要另存为 UTF-8（chcp 65001 会导致批处理解析错位）。
rem ============================================================

net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [UAC] 需要管理员权限，请在弹窗中点「是」...
    set "ARGS=%*"
    if defined ARGS (
        powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -ArgumentList '%ARGS%' -Verb RunAs" >nul 2>&1
    ) else (
        powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs" >nul 2>&1
    )
    exit /b
)

set "SVC=DualNICBalance"
set "TASK=DualNICBalanceGUI"
set "SVCEXE=%~dp0dualnic-service.exe"
set "GUIEXE=%~dp0dualnic-gui.exe"
set "DATADIR=%ProgramData%\DualNIC Balance"

if /i "%~1"=="install"   goto do_install
if /i "%~1"=="uninstall" goto do_uninstall
if /i "%~1"=="purge"     goto do_purge

:main
cls
echo.
echo  ============ DualNIC Balance ============
echo    1. 一键安装（后台服务 + GUI 登录自启动）
echo    2. 一键卸载（移除服务与自启动，保留数据）
echo    3. 卸载并彻底删除（含配置/事件数据）
echo    0. 退出
echo  =========================================
choice /c 1230 /n /m "  请选择: "
if errorlevel 4 exit /b 0
if errorlevel 3 goto do_purge
if errorlevel 2 goto do_uninstall
goto do_install

:do_install
if not exist "%SVCEXE%" (
    echo   X 未找到 %SVCEXE%
    echo     请把本脚本与两个 exe 放在同一目录后再运行。
    goto done_fail
)
echo.
echo ^>^> [1/4] 清理残留的本工具进程...
taskkill /F /IM dualnic-service.exe >nul 2>&1

echo ^>^> [2/4] 创建服务 %SVC%（LocalSystem、开机自启、崩溃自动重启）...
sc create %SVC% binPath= "\"%SVCEXE%\"" start= auto obj= LocalSystem DisplayName= "DualNIC Balance Service" >nul
if errorlevel 1 (
    echo     X 服务创建失败（退出码 %errorlevel%）。
    echo       若提示服务已存在：请先执行「卸载」再重新安装。
    goto done_fail
)
sc description %SVC% "双网卡分流常驻服务：路由对账/自愈、环境自动匹配、事件日志" >nul
sc failure %SVC% reset= 86400 actions= restart/60000/restart/60000/restart/60000 >nul

echo ^>^> [3/4] 启动服务...
sc start %SVC% >nul
if errorlevel 1 echo     ! 服务启动失败：请到服务管理器（services.msc）查看。

echo ^>^> [4/4] 注册 GUI 登录自启动（登录后 30 秒，取消 72 小时强制结束）...
schtasks /create /f /tn "%TASK%" /tr "\"%GUIEXE%\"" /sc onlogon /delay 0000:30 /rl limited >nul
if errorlevel 1 (
    echo     X 自启动任务注册失败（退出码 %errorlevel%）。
    goto done_fail
)
powershell -NoProfile -Command "$t=Get-ScheduledTask -TaskName '%TASK%'; $t.Settings.ExecutionTimeLimit='PT0S'; Set-ScheduledTask -InputObject $t | Out-Null" >nul 2>&1

echo.
echo   V 安装完成：服务已由系统托管，GUI 将在每次登录 30 秒后自启。
echo   提示：首次使用请打开 dualnic-gui.exe 完成网卡标记与方案配置。
goto done

:do_uninstall
echo.
echo ^>^> [1/4] 结束 GUI 进程...
taskkill /F /IM dualnic-gui.exe >nul 2>&1

echo ^>^> [2/4] 删除 GUI 登录自启动任务...
schtasks /delete /f /tn "%TASK%" >nul 2>&1

echo ^>^> [3/4] 停止并删除服务 %SVC%...
sc stop %SVC% >nul 2>&1
timeout /t 2 /nobreak >nul
sc delete %SVC% >nul 2>&1
taskkill /F /IM dualnic-service.exe >nul 2>&1

echo ^>^> [4/4] 卸载完成。
echo   配置/事件数据保留在 %DATADIR%\ ，如需彻底清理请选菜单 3（或 setup.bat purge）。
goto done

:do_purge
echo.
echo ^>^> [1/5] 结束 GUI 进程...
taskkill /F /IM dualnic-gui.exe >nul 2>&1

echo ^>^> [2/5] 删除 GUI 登录自启动任务...
schtasks /delete /f /tn "%TASK%" >nul 2>&1

echo ^>^> [3/5] 停止并删除服务 %SVC%...
sc stop %SVC% >nul 2>&1
timeout /t 2 /nobreak >nul
sc delete %SVC% >nul 2>&1
taskkill /F /IM dualnic-service.exe >nul 2>&1

echo ^>^> [4/5] 删除配置/事件数据目录...
rd /s /q "%DATADIR%" >nul 2>&1
if exist "%DATADIR%" (
    echo     X 目录删除失败（可能被占用），请手动删除： %DATADIR%
)

echo ^>^> [5/5] 卸载完成，配置与事件数据已彻底删除。
goto done

:done_fail
echo.
echo   本次操作未完成，请把本窗口内容截图反馈。
goto done

:done
echo.
if "%~1"=="" (
    echo   按任意键返回菜单...
    pause >nul
    goto main
)
exit /b 0
