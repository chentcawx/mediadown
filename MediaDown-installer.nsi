; MediaDown NSIS installer script (ASCII)
; Build with: makensis MediaDown-installer.nsi
; Output: MediaDown-x86-setup.exe
!include "MUI2.nsh"

Name "MediaDown"
OutFile "MediaDown-x86-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\MediaDown"
InstallDirRegKey HKCU "Software\MediaDown" "InstallDir"

RequestExecutionLevel user
Unicode true

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  SetOutPath "$INSTDIR"
  File "dist\MediaDown-x86.exe"
  File "src-tauri\icons\icon.ico"

  WriteRegStr HKCU "Software\MediaDown" "InstallDir" "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\MediaDown"
  CreateShortCut "$SMPROGRAMS\MediaDown\MediaDown.lnk" "$INSTDIR\MediaDown-x86.exe" "" "$INSTDIR\icon.ico"
  CreateShortCut "$DESKTOP\MediaDown.lnk" "$INSTDIR\MediaDown-x86.exe" "" "$INSTDIR\icon.ico"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\MediaDown-x86.exe"
  Delete "$INSTDIR\icon.ico"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\MediaDown\MediaDown.lnk"
  RMDir "$SMPROGRAMS\MediaDown"
  Delete "$DESKTOP\MediaDown.lnk"
  DeleteRegKey HKCU "Software\MediaDown"
SectionEnd
