@echo off
chcp 65001 >nul
title ZCode 用量监控组件（Electron 版）
cd /d "%~dp0"
echo 启动 ZCode 用量监控组件（Electron 版）...
call npx electron .
if errorlevel 1 (
  echo.
  echo 启动失败（错误码 %errorlevel%），按任意键关闭...
  pause >nul
)
