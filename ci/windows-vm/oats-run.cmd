@echo off
rem Runs at first logon inside the evaluation VM. Reports over COM1 (qemu -serial file:serial.log).
echo OATS_LOGON > COM1
set SRC=
for %%d in (D E F G H I) do if exist %%d:\oats-meetings-rs.exe set SRC=%%d:
echo OATS_SRC=%SRC% > COM1
copy /y %SRC%\oats-meetings-rs.exe C:\oats-meetings-rs.exe > COM1
ver > COM1
C:\oats-meetings-rs.exe --headless --version > C:\oats-out.txt 2>&1
set RC=%ERRORLEVEL%
type C:\oats-out.txt > COM1
echo OATS_RC=%RC% > COM1
shutdown /s /t 3 /f
