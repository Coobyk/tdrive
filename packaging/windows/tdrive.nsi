!ifndef APPVERSION
  !define APPVERSION "0.0.0"
!endif

Unicode True
Name "TDrive"
OutFile "..\..\tdrive-windows-x86_64-setup-${APPVERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\TDrive"
InstallDirRegKey HKCU "Software\Coobyk\TDrive" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUninstDetails show

VIProductVersion "${APPVERSION}.0"
VIAddVersionKey "ProductName" "TDrive"
VIAddVersionKey "CompanyName" "Coobyk"
VIAddVersionKey "FileDescription" "TDrive installer"
VIAddVersionKey "FileVersion" "${APPVERSION}"
VIAddVersionKey "ProductVersion" "${APPVERSION}"
VIAddVersionKey "LegalCopyright" "Copyright (c) 2025 Coobyk"

!define MUI_ICON "tdrive.ico"
!define MUI_UNICON "tdrive.ico"

!include "MUI2.nsh"

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\tdrive-gui.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch TDrive"
!define MUI_FINISHPAGE_RUN_NOTCHECKED

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "TDrive" SEC_TDrive
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "..\..\target\x86_64-pc-windows-msvc\release\tdrive.exe"
  File "..\..\target\x86_64-pc-windows-msvc\release\tdrive-gui.exe"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\TDrive"
  CreateShortcut "$SMPROGRAMS\TDrive\TDrive.lnk" "$INSTDIR\tdrive-gui.exe"
  CreateShortcut "$SMPROGRAMS\TDrive\Uninstall TDrive.lnk" "$INSTDIR\Uninstall.exe"

  WriteRegStr HKCU "Software\Coobyk\TDrive" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDrive" "DisplayName" "TDrive"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDrive" "DisplayVersion" "${APPVERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDrive" "Publisher" "Coobyk"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDrive" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDrive" "DisplayIcon" "$INSTDIR\tdrive-gui.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDrive" "UninstallString" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDrive" "URLInfoAbout" "https://github.com/Coobyk/tdrive"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDrive" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDrive" "NoRepair" 1
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\tdrive.exe"
  Delete "$INSTDIR\tdrive-gui.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\TDrive\TDrive.lnk"
  Delete "$SMPROGRAMS\TDrive\Uninstall TDrive.lnk"
  RMDir "$SMPROGRAMS\TDrive"

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDrive"
  DeleteRegKey HKCU "Software\Coobyk\TDrive"
SectionEnd
