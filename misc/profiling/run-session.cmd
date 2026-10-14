@echo off
cd /d "%~dp0..\.."
set RIO_LOG_LEVEL=info
if exist target\rio-profile-run.exitcode del target\rio-profile-run.exitcode
python misc\profiling\run-windows.py %* > target\rio-profile-run.log 2>&1
echo %errorlevel% > target\rio-profile-run.exitcode
