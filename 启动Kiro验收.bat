@echo off
chcp 65001 >nul
set "KIRO_GATEWAY_URL=https://kiro.rent"
set "KIRO_AUTH_PORTAL_URL=%KIRO_GATEWAY_URL%"
set "AWS_ENDPOINT_URL=%KIRO_GATEWAY_URL%"
set "KIRO_DISABLE_SESSION_TITLE_LLM=true"
set "KIRO_DISABLE_RECAP=true"
tasklist /FI "IMAGENAME eq Kiro.exe" /NH | findstr /I /C:"Kiro.exe" >nul
if not errorlevel 1 (
  echo 请先保存工作并完全退出 Kiro，再用此入口启动，使证书配置生效。
  pause
  exit /b 1
)
set "KIRO_EXE=%LOCALAPPDATA%\Programs\Kiro\Kiro.exe"
if not exist "%KIRO_EXE%" (
  echo 未找到 Kiro：%KIRO_EXE%
  pause
  exit /b 1
)
start "" "%KIRO_EXE%"
