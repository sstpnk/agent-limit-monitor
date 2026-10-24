@echo off
setlocal
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if exist "%VSWHERE%" (
  for /f "usebackq tokens=*" %%i in (`"%VSWHERE%" -latest -products * -property installationPath`) do set "VSPATH=%%i"
)
if not defined VSPATH (
  for %%p in (
    "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools"
    "%ProgramFiles%\Microsoft Visual Studio\2022\BuildTools"
    "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\Community"
    "%ProgramFiles%\Microsoft Visual Studio\2022\Community"
  ) do if exist "%%~p\VC\Auxiliary\Build\vcvars64.bat" set "VSPATH=%%~p"
)
if not defined VSPATH (
  echo Visual Studio Build Tools with the C++ workload was not found.
  exit /b 1
)
call "%VSPATH%\VC\Auxiliary\Build\vcvars64.bat" >nul
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cargo %*
