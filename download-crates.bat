@echo off
chcp 65001 >nul
set "CACHE_DIR=%USERPROFILE%\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f"
set "LOCK_FILE=D:\WorkBuddy\mediadown\media-down\src-tauri\Cargo.lock"
set "MIRROR=https://mirrors.huaweicloud.com/crates.io/crates"

echo Scanning Cargo.lock for packages...
for /f "tokens=1,2 delims==" %%a in ('findstr "^name = " "%LOCK_FILE%"') do (
    set "name=%%b"
    set "name=!name:"=!"
    for /f "tokens=1,2 delims==" %%c in ('findstr /n "^version = " "%LOCK_FILE%" ^| findstr "^%%a:"') do (
        set "ver=%%d"
        set "ver=!ver:"=!"
        set "pkg=!name!-!ver!"
        echo Checking: !pkg!
        if not exist "!CACHE_DIR!\!pkg!" (
            echo Downloading: !pkg!
            curl -s --connect-timeout 10 "%MIRROR%/!name!/!pkg!.crate" -o "!CACHE_DIR!\!pkg!.crate"
            if !errorlevel! equ 0 (
                echo OK: !pkg!
            ) else (
                echo FAIL: !pkg!
            )
        ) else (
            echo EXISTS: !pkg!
        )
    )
)
echo Done!
pause
