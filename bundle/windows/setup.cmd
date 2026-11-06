@echo off
rem Unpacks the Blender zip into .\blender and puts portable\ into it. Idempotent.
setlocal
cd /d "%~dp0"

rem The tar built into Windows 10+ (bsdtar, reads zip); another tar on PATH might not
set "tar=%SystemRoot%\System32\tar.exe"
if not exist "%tar%" (
  echo ERROR: %tar% not found ^(needs Windows 10 1803 or later^)
  exit /b 1
)
set "zip="
for %%Z in (blender-*-windows-x64.zip) do set "zip=%%Z"
if not defined zip (
  echo ERROR: no blender-*-windows-x64.zip in the folder
  exit /b 1
)
if not exist "portable\" (
  echo ERROR: portable\ is missing
  exit /b 1
)
rem The zip's top folder is named like the zip (blender-<ver>-windows-x64)
for %%Z in ("%zip%") do set "top=%%~nZ"

set "xdir=blender-extract"
if exist "%xdir%" rmdir /s /q "%xdir%"
mkdir "%xdir%" || exit /b 1
echo Unpacking Blender (%zip%) ...
"%tar%" -xf "%zip%" -C "%xdir%"
if errorlevel 1 goto failed
if not exist "%xdir%\%top%\blender.exe" (
  echo ERROR: no %top%\blender.exe in the zip
  goto failed
)
if exist blender rmdir /s /q blender
if exist blender (
  echo ERROR: could not remove the old blender\ ^(Blender or packshot still running?^)
  goto failed
)
move "%xdir%\%top%" blender >nul || goto failed
rmdir /s /q "%xdir%"

rem portable\ (config and StarBreaker add-on) into Blender; robocopy: exit codes below 8 are success
if exist "blender\portable" rmdir /s /q "blender\portable"
robocopy portable blender\portable /E /NFL /NDL /NJH /NJS /NP >nul
if errorlevel 8 (
  echo ERROR: copying portable\ failed
  exit /b 1
)

echo Done. Next: packshot.json and run.cmd --render --export (what this node can do)
exit /b 0

:failed
if exist "%xdir%" rmdir /s /q "%xdir%"
exit /b 1
