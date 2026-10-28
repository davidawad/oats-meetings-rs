@echo off
rem Runs at first logon inside the evaluation VM. Reports over COM1 (qemu -serial file:serial.log).
echo OATS_LOGON > COM1
rem Make later boots (overlay runs of a cached base image) run this script again from the config CD.
schtasks /query /tn OatsRun >nul 2>&1 || (
  echo @echo off> C:\oats-boot.cmd
  echo for %%%%d in (D E F G H I^) do if exist %%%%d:\oats-run.cmd call %%%%d:\oats-run.cmd>> C:\oats-boot.cmd
  schtasks /create /tn OatsRun /sc onstart /ru SYSTEM /rl highest /tr C:\oats-boot.cmd /f
)
set SRC=
for %%d in (D E F G H I) do if exist %%d:\oats-meetings-rs.exe set SRC=%%d:
echo OATS_SRC=%SRC% > COM1
copy /y %SRC%\oats-meetings-rs.exe C:\oats-meetings-rs.exe > COM1
ver > COM1
if exist %SRC%\dlls.txt for /f %%d in ('findstr /v /b /i "api-ms-" %SRC%\dlls.txt') do if not exist %windir%\system32\%%d echo OATS_MISSING_DLL %%d> COM1
rem 120 s limit: a loader failure would otherwise block on an invisible error dialog.
powershell -NoProfile -Command "$p = Start-Process C:\oats-meetings-rs.exe -ArgumentList '--headless','--version' -PassThru -RedirectStandardOutput C:\oats-out.txt -RedirectStandardError C:\oats-err.txt; if (-not $p.WaitForExit(120000)) { $p.Kill(); 'OATS_TIMEOUT' | Out-File C:\oats-out.txt; exit 124 }; exit $p.ExitCode"
set RC=%ERRORLEVEL%
type C:\oats-err.txt > COM1
type C:\oats-out.txt > COM1
echo OATS_RC=%RC% > COM1
shutdown /s /t 3 /f
