@echo off
rem Drag a music folder onto this file to check how the player will read its tags.
rem (Runs check-tags.ps1 next to it and writes report files beside this .cmd)
setlocal
if "%~1"=="" (
  echo Usage: drag a music folder onto check-tags.cmd
  echo    or: check-tags.cmd "D:\Music"
  pause
  exit /b 1
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0check-tags.ps1" -Path "%~1" -Csv "%~dp0tag-report.csv" -Playlist "%~dp0music-to-fix.m3u8"
pause
