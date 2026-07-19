@echo off
setlocal

if "%~1"=="" (
    echo Usage: %~nx0 NTP_SERVER_IP [OUTPUT_PATH]
    exit /b 2
)

if "%~2"=="" (
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0verify-windows-time.ps1" "%~1"
) else (
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0verify-windows-time.ps1" "%~1" -OutputPath "%~2"
)

exit /b %errorlevel%
