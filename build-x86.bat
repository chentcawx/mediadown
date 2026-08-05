@echo off
REM ============================================
REM  MediaDown - Build 32-bit (i686) Windows app
REM  Requires: Rust with i686-pc-windows-msvc target
REM            + VS2022 Build Tools (C++ x86 libs)
REM  NSIS (makensis) is optional: when present it
REM  also produces an installer from MediaDown-installer.nsi
REM ============================================
setlocal
cd /d "%~dp0"
cd /d src-tauri

rustup target list --installed | findstr /i "i686-pc-windows-msvc" >nul
if errorlevel 1 (
    echo [STEP] Adding 32-bit rust target ...
    call rustup target add i686-pc-windows-msvc
    if errorlevel 1 (
        echo [ERROR] Failed to add i686-pc-windows-msvc target.
        exit /b 1
    )
)

echo [BUILD] Compiling MediaDown 32-bit (i686) release ...
cargo build --release --target i686-pc-windows-msvc
if errorlevel 1 (
    echo [FAILED] Build failed. See messages above.
    exit /b 1
)

if not exist ..\dist mkdir ..\dist
copy /Y target\i686-pc-windows-msvc\release\media-down.exe ..\dist\MediaDown-x86.exe >nul
echo [OK] Portable exe: dist\MediaDown-x86.exe

where makensis >nul 2>nul
if not errorlevel 1 (
    echo [BUNDLE] Building NSIS installer ...
    makensis MediaDown-installer.nsi
    if not errorlevel 1 (
        echo [OK] Installer written: MediaDown-x86-setup.exe
    ) else (
        echo [WARN] NSIS bundling failed; portable exe is still available.
    )
) else (
    echo [INFO] makensis not found - skipped installer. Portable exe is ready.
)
endlocal
