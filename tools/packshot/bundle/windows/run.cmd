@echo off
rem Starts packshot (TUI) with what this node can do now, e.g. run.cmd --render --export (both), run.cmd --render
rem (render only) or run.cmd (only start runs, store them, watch). Further arguments go to packshot (--workers 6,
rem --serve for no TUI, --help).
cd /d "%~dp0"

for %%A in (%*) do (
  if /i "%%~A"=="--render" if not exist "blender\blender.exe" (
    echo ERROR: --render needs Blender in .\blender: run setup.cmd first 1>&2
    exit /b 1
  )
)

"%~dp0packshot.exe" %*
