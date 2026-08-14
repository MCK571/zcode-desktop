@echo off
chcp 65001 >nul
title ZCode 用量监控组件（Tauri 版）
cd /d "%~dp0"
rem 优先运行已构建的 release exe；无产物时走 tauri dev
if exist "%~dp0src-tauri\target\release\zcode-usage-widget.exe" (
  start "" "%~dp0src-tauri\target\release\zcode-usage-widget.exe"
  exit /b 0
)
echo 未找到 release 产物，启动开发模式（tauri dev）...
call npx tauri dev
if errorlevel 1 (
  echo.
  echo 启动失败（错误码 %errorlevel%），按任意键关闭...
  pause >nul
)
