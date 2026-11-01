; TextSwell (largetxt-gen) - NSIS installer script
; Builds TextSwell-v0.2.0-x64-Setup.exe
; Expects the release exe staged next to this script's build directory as
; "largetxt-gen.exe" (pass /DSTAGE=<dir> if it lives elsewhere).

!ifndef VERSION
  !define VERSION "0.2.0"
!endif
!ifndef STAGE
  !define STAGE "."
!endif

!include "WinMessages.nsh"

Name "TextSwell ${VERSION}"
OutFile "TextSwell-v${VERSION}-x64-Setup.exe"
InstallDir "$PROGRAMFILES64\TextSwell"
InstallDirRegKey HKLM "Software\TextSwell" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "TextSwell"
VIAddVersionKey "FileDescription" "largetxt-gen - massive test-file generator"
VIAddVersionKey "FileVersion" "${VERSION}.0"
VIAddVersionKey "ProductVersion" "${VERSION}.0"
VIAddVersionKey "LegalCopyright" "MIT License"

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

; --- PATH helpers -----------------------------------------------------------
; Append $INSTDIR to the system PATH (HKLM) if it is not already there.
Function AddToPath
  ReadRegStr $0 HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "Path"
  Push $0
  Push ";$INSTDIR;"
  Call StrStr
  Pop $1
  StrCmp $1 "" 0 done
  WriteRegExpandStr HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "Path" "$0;$INSTDIR"
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
done:
FunctionEnd

Function un.RemoveFromPath
  ReadRegStr $0 HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "Path"
  Push $0
  Push ";$INSTDIR"
  Call un.StrStr
  Pop $1
  StrCmp $1 "" 0 +2
  Return
  StrLen $2 ";$INSTDIR"
  StrCpy $3 $1 "" $2
  StrCpy $4 $0 $1
  WriteRegExpandStr HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "Path" "$4$3"
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
FunctionEnd

Function StrStr
  Exch $R1
  Exch
  Exch $R2
  Push $R3
  Push $R4
  Push $R5
  StrLen $R3 $R1
  StrCpy $R4 0
loop:
  StrCpy $R5 $R2 $R3 $R4
  StrCmp $R5 $R1 found
  StrCmp $R5 "" notfound
  IntOp $R4 $R4 + 1
  Goto loop
found:
  StrCpy $R1 $R4
  Goto end
notfound:
  StrCpy $R1 ""
end:
  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R2
  Exch $R1
FunctionEnd

Function un.StrStr
  Exch $R1
  Exch
  Exch $R2
  Push $R3
  Push $R4
  Push $R5
  StrLen $R3 $R1
  StrCpy $R4 0
loop:
  StrCpy $R5 $R2 $R3 $R4
  StrCmp $R5 $R1 found
  StrCmp $R5 "" notfound
  IntOp $R4 $R4 + 1
  Goto loop
found:
  StrCpy $R1 $R4
  Goto end
notfound:
  StrCpy $R1 ""
end:
  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R2
  Exch $R1
FunctionEnd

; --- Install ----------------------------------------------------------------
Section "TextSwell (required)"
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "${STAGE}/largetxt-gen.exe"
  File "${STAGE}/README.md"
  File "${STAGE}/LICENSE"
  WriteRegStr HKLM "Software\TextSwell" "InstallDir" "$INSTDIR"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\TextSwell" "DisplayName" "TextSwell ${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\TextSwell" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\TextSwell" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\TextSwell" "Publisher" "Sean Shaffer"
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\TextSwell" "NoModify" 1
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\TextSwell" "NoRepair" 1
SectionEnd

Section "Add largetxt-gen to the system PATH"
  Call AddToPath
SectionEnd

; --- Uninstall --------------------------------------------------------------
Section "Uninstall"
  Call un.RemoveFromPath
  Delete "$INSTDIR\largetxt-gen.exe"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "Software\TextSwell"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\TextSwell"
SectionEnd
