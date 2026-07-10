@echo off
setlocal
PowerShell -NoProfile -ExecutionPolicy Bypass -File "%~dp0tools\start_rapture.ps1"
if errorlevel 1 (
    echo.
    echo Failed to start Rapture.
    pause
)
