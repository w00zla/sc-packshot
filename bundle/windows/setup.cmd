@echo off
rem Sets up Blender for packshot: the official archive named in blender.pin (taken from this folder if it is here,
rem else downloaded from blender.org), checked against the SHA256 in blender.pin, unpacked into .\blender, with
rem portable\ (Blender's config, nothing in the user profile) and the StarBreaker add-on from .\addon enabled in it.
rem Run it again after a new bundle.
setlocal EnableDelayedExpansion
cd /d "%~dp0"

rem The tar and curl built into Windows 10+ (bsdtar reads zip); others on PATH might not
set "tar=%SystemRoot%\System32\tar.exe"
set "curl=%SystemRoot%\System32\curl.exe"
if not exist "%tar%" (
  echo ERROR: %tar% not found ^(needs Windows 10 1803 or later^)
  exit /b 1
)
if not exist "blender.pin" (
  echo ERROR: blender.pin is missing
  exit /b 1
)
if not exist "addon\starbreaker_addon\__init__.py" (
  echo ERROR: addon\starbreaker_addon is missing
  exit /b 1
)
for /f "usebackq tokens=1,* delims==" %%A in ("blender.pin") do set "pin_%%A=%%B"
if not defined pin_version (
  echo ERROR: blender.pin has no version
  exit /b 1
)
if not defined pin_sha256 (
  echo ERROR: blender.pin has no sha256
  exit /b 1
)
for /f "tokens=1,2 delims=." %%A in ("%pin_version%") do set "minor=%%A.%%B"
set "top=blender-%pin_version%-windows-x64"
set "zip=%top%.zip"

rem 1. The archive: here, or from blender.org
if not exist "%zip%" (
  if not exist "%curl%" (
    echo ERROR: no %zip% here and no %curl% to download it: put the archive next to setup.cmd
    exit /b 1
  )
  echo Downloading %zip% from blender.org ...
  "%curl%" -fL --retry 3 -o "%zip%.part" "https://download.blender.org/release/Blender%minor%/%zip%"
  if errorlevel 1 (
    if exist "%zip%.part" del "%zip%.part"
    echo ERROR: download failed: put %zip% next to setup.cmd and run it again
    exit /b 1
  )
  move /y "%zip%.part" "%zip%" >nul
)
set "hash="
for /f "skip=1 delims=" %%H in ('certutil -hashfile "%zip%" SHA256') do if not defined hash set "hash=%%H"
set "hash=!hash: =!"
if /i not "!hash!"=="%pin_sha256%" (
  echo ERROR: SHA256 of %zip% is !hash!, blender.pin says %pin_sha256%: delete it and run setup.cmd again
  exit /b 1
)
echo SHA256 ok ^(%zip%^)

rem 2. Unpacked into .\blender
set "xdir=blender-extract"
if exist "%xdir%" rmdir /s /q "%xdir%"
mkdir "%xdir%" || exit /b 1
echo Unpacking Blender %pin_version% ...
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

rem 3. portable\ with the add-on, enabled in its userpref.blend; robocopy: exit codes below 8 are success
mkdir "blender\portable\config" "blender\portable\scripts\addons" 2>nul
robocopy addon\starbreaker_addon blender\portable\scripts\addons\starbreaker_addon /E /XD __pycache__ /NFL /NDL /NJH /NJS /NP >nul
if errorlevel 8 (
  echo ERROR: copying the add-on failed
  exit /b 1
)
echo Enabling the add-on ...
blender\blender.exe -b --python-expr "import bpy, addon_utils; addon_utils.enable('starbreaker_addon', default_set=True, persistent=True); bpy.ops.wm.save_userpref(); print('ADDON', 'starbreaker_addon' in bpy.context.preferences.addons)" 2>&1 | findstr /c:"ADDON True" >nul
if errorlevel 1 (
  echo ERROR: Blender did not enable the add-on ^(run blender\blender.exe -b and look at its output^)
  exit /b 1
)

echo Done. The archive %zip% can go. Next: packshot.json and run.cmd --render --export (what this node can do)
exit /b 0

:failed
if exist "%xdir%" rmdir /s /q "%xdir%"
exit /b 1
