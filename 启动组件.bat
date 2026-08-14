@echo off
chcp 65001 >nul
title ZCode 用量监控组件（Electron 版）
cd /d "%~dp0"
echo 启动 ZCode 用量监控组件（Electron 版）...
if not exist "%~dp0node_modules\electron\dist\electron.exe" (
  echo.
  echo 找不到 electron.exe，请先运行: pnpm install 或 npm install
  pause >nul
  exit /b 1
)
call "%~dp0node_modules\electron\dist\electron.exe" .
if errorlevel 1 (
  echo.
  echo 启动失败（错误码 %errorlevel%），按任意键关闭...
  pause >nul
)
