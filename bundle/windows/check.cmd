@echo off
rem Checks Blender with the StarBreaker add-on and an EEVEE render on Vulkan (run setup.cmd first).
setlocal
cd /d "%~dp0"
if not exist "blender\blender.exe" (
  echo ERROR: no blender\blender.exe: run setup.cmd first
  exit /b 1
)
set "failed=0"
set "png=%TEMP%\packshot-check-%RANDOM%.png"
set "log=%TEMP%\packshot-check.log"

where nvidia-smi >nul 2>&1 && nvidia-smi --query-gpu=name,driver_version --format=csv,noheader

rem 1: add-on
blender\blender.exe -b --python-expr "import addon_utils; print('ADDON', addon_utils.check('starbreaker_addon'))" >"%log%" 2>&1
findstr /c:"ADDON (True, True)" "%log%" >nul
if errorlevel 1 (
  echo ERROR  Addon starbreaker_addon ^(output in %log%^)
  set "failed=1"
) else (
  echo OK     Addon starbreaker_addon
)

rem 2: EEVEE on Vulkan, headless
if exist "%png%" del "%png%"
blender\blender.exe -b --factory-startup --gpu-backend vulkan --python-expr "import bpy; s=bpy.context.scene; s.render.engine='BLENDER_EEVEE'; s.render.resolution_x=64; s.render.resolution_y=64; s.render.filepath=r'%png%'; bpy.ops.render.render(write_still=True)" >"%log%" 2>&1
if exist "%png%" (
  echo OK     EEVEE render ^(Vulkan^)
  del "%png%"
) else (
  echo ERROR  EEVEE render ^(Vulkan^) ^(output in %log%^)
  set "failed=1"
)

exit /b %failed%
