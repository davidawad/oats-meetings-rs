@echo off
rem Runs at first logon inside the evaluation VM; reports over COM1.
echo OATS_LOGON > COM1
set SRC=
for %%d in (D E F G H I) do if exist %%d:\oats-shadergen.exe set SRC=%%d:
echo OATS_SRC=%SRC% > COM1
ver > COM1
mkdir C:\sh
copy /y %SRC%\*.hlsl C:\sh\ > nul
%SRC%\oats-shadergen.exe C:\sh C:\shaders_bytes.rs 2> C:\sg.err
set RC=%ERRORLEVEL%
type C:\sg.err > COM1
if not "%RC%"=="0" goto done
certutil -hashfile C:\shaders_bytes.rs SHA256 > COM1
certutil -f -encode C:\shaders_bytes.rs C:\sb.b64 > nul
echo OATS_B64_BEGIN > COM1
type C:\sb.b64 > COM1
echo OATS_B64_END > COM1
:done
echo OATS_RC=%RC% > COM1
shutdown /s /t 3 /f
