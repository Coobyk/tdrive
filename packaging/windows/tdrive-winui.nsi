!ifndef APPVERSION
  !define APPVERSION "0.0.0"
!endif

Unicode True
Name "TDrive (WinUI 3)"
OutFile "..\..\tdrive-windows-x86_64-winui-setup-${APPVERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\TDrive WinUI"
InstallDirRegKey HKCU "Software\Coobyk\TDriveWinUI" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUninstDetails show

VIProductVersion "${APPVERSION}.0"
VIAddVersionKey "ProductName" "TDrive (WinUI 3)"
VIAddVersionKey "CompanyName" "Coobyk"
VIAddVersionKey "FileDescription" "TDrive WinUI 3 installer"
VIAddVersionKey "FileVersion" "${APPVERSION}"
VIAddVersionKey "ProductVersion" "${APPVERSION}"
VIAddVersionKey "LegalCopyright" "Copyright (c) 2025 Coobyk"

!define MUI_ICON "tdrive.ico"
!define MUI_UNICON "tdrive.ico"

!include "MUI2.nsh"

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\TDrive.WinUI.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch TDrive (WinUI 3)"
!define MUI_FINISHPAGE_RUN_NOTCHECKED

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "TDrive WinUI 3" SEC_TDriveWinUI
  SectionIn RO
  SetOutPath "$INSTDIR"
  File /r "..\..\dist\windows-winui-stage\*.*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\TDrive WinUI"
  CreateShortcut "$SMPROGRAMS\TDrive WinUI\TDrive (WinUI 3).lnk" "$INSTDIR\TDrive.WinUI.exe"
  CreateShortcut "$SMPROGRAMS\TDrive WinUI\Uninstall TDrive (WinUI 3).lnk" "$INSTDIR\Uninstall.exe"

  WriteRegStr HKCU "Software\Coobyk\TDriveWinUI" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDriveWinUI" "DisplayName" "TDrive (WinUI 3)"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDriveWinUI" "DisplayVersion" "${APPVERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDriveWinUI" "Publisher" "Coobyk"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDriveWinUI" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDriveWinUI" "DisplayIcon" "$INSTDIR\TDrive.WinUI.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDriveWinUI" "UninstallString" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDriveWinUI" "URLInfoAbout" "https://github.com/Coobyk/tdrive"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDriveWinUI" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDriveWinUI" "NoRepair" 1
SectionEnd

Section "Uninstall"
  RMDir /r "$INSTDIR"

  Delete "$SMPROGRAMS\TDrive WinUI\TDrive (WinUI 3).lnk"
  Delete "$SMPROGRAMS\TDrive WinUI\Uninstall TDrive (WinUI 3).lnk"
  RMDir "$SMPROGRAMS\TDrive WinUI"

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TDriveWinUI"
  DeleteRegKey HKCU "Software\Coobyk\TDriveWinUI"
SectionEnd
