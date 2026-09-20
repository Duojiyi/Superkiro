@echo off
chcp 65001 >nul
cd /d "%~dp0"
if not defined KIRO_GATEWAY_URL set "KIRO_GATEWAY_URL=https://kiro.rent"
call npm --prefix apps/desktop-ui run build
if errorlevel 1 goto failed
cargo build --locked --offline -p desktop-host --bin Superkiro
if errorlevel 1 goto failed
start "" "%~dp0target\debug\Superkiro.exe"
exit /b 0
:failed
echo Debug build failed. No release was published.
pause
exit /b 1
