@echo off
title ZCode Usage Widget (Tauri)
cd /d "%~dp0"
rem prefer built release exe; fallback to tauri dev
if exist "%~dp0src-tauri\target\release\zcode-usage-widget.exe" (
  start "" "%~dp0src-tauri\target\release\zcode-usage-widget.exe"
  exit /b 0
)
echo release exe not found, starting dev mode (tauri dev)...
call npx tauri dev
if errorlevel 1 (
  echo.
  echo launch failed (errorlevel %errorlevel%), press any key to close...
  pause >nul
)
