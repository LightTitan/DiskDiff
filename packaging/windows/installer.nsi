; DiskDiff installer for Windows, built by the release workflow:
;
;   makensis /DVERSION=0.4.0 /DSRC=target\release /DOUT=target\DiskDiff_0.4.0_x64-setup.exe packaging\windows\installer.nsi
;
; Installs for the current user (no administrator rights) into
; %LOCALAPPDATA%\Programs\DiskDiff, the usual place for per-user apps, with a
; Start menu shortcut, an optional desktop shortcut and an entry under
; Settings > Apps for uninstalling. Scans and settings live in
; %APPDATA%\DiskDiff Native and are kept when DiskDiff is uninstalled or
; updated. The fixed folder also keeps the path an AI agent's config points
; at (diskdiff-mcp.exe) the same across updates.

Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !error "pass /DVERSION=x.y.z"
!endif
!ifndef SRC
  !define SRC "..\..\target\release"
!endif
!ifndef OUT
  !define OUT "DiskDiff_${VERSION}_x64-setup.exe"
!endif

!define APP "DiskDiff"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\DiskDiff"

Name "${APP}"
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "${APP} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "GPL-3.0-or-later"

!define MUI_ICON "..\..\app\icon\DiskDiff.ico"
!define MUI_UNICON "..\..\app\icon\DiskDiff.ico"
!define MUI_ABORTWARNING

!define MUI_FINISHPAGE_RUN "$INSTDIR\diskdiff.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open DiskDiff"
; The "show readme" checkbox, used for the desktop shortcut.
!define MUI_FINISHPAGE_SHOWREADME
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Create a desktop shortcut"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION DesktopShortcut

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; Asks a running DiskDiff to quit (a normal close, so it saves and removes its
; agent endpoint), so its files can be replaced.
!macro CloseApp
  nsExec::Exec 'taskkill /IM diskdiff.exe'
  Pop $0
  Sleep 1500
!macroend

Section "Install"
  SetShellVarContext current
  !insertmacro CloseApp
  SetOutPath "$INSTDIR"
  ; If an AI agent is running diskdiff-mcp.exe, Windows keeps it locked and
  ; NSIS offers to retry once the agent app is closed.
  File "${SRC}\diskdiff.exe"
  File "${SRC}\diskdiff-mcp.exe"
  File "..\..\LICENSE"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\diskdiff.exe"

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${APP}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/LightTitan/DiskDiff"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\diskdiff.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  SectionGetSize 0 $0
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" $0
SectionEnd

Function DesktopShortcut
  SetShellVarContext current
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\diskdiff.exe"
FunctionEnd

Section "Uninstall"
  SetShellVarContext current
  !insertmacro CloseApp
  Delete "$INSTDIR\diskdiff.exe"
  Delete "$INSTDIR\diskdiff-mcp.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  ; Scans and settings in %APPDATA%\DiskDiff Native are kept on purpose.
SectionEnd
