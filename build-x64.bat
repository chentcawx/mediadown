@echo off
REM ============================================
REM  MediaDown - Build 64-bit (x86_64) NSIS installer
REM ============================================
setlocal
cd /d "%~dp0"

where npx >nul 2>nul
if errorlevel 1 (
    echo [ERROR] Node.js / npm not found. Please install Node.js first.
    exit /b 1
)

echo [BUILD] Compiling MediaDown for 64-bit Windows (x86_64) ...
call npx tauri build --target x86_64-pc-windows-msvc --bundles nsis
if errorlevel 1 (
    echo [FAILED] Build failed. See messages above.
    exit /b 1
)

echo.
echo [OK] 64-bit installer written to:
echo     src-tauri\target\x86_64-pc-windows-msvc\release\bundle\nsis\
endlocal
