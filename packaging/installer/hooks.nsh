; Lasso installer hooks (Tauri's NSIS bundler, same method as Aimdot's). Lasso is ONE program: lasso.exe also runs
; the Settings / Library windows as extra processes. Ask the running copy to quit (it saves first), then stop whatever
; still runs from THIS install folder (other copies, e.g. a portable one, are left alone).
; The autostart ("Start with your PC") Run key is NOT written here - an installer writing a Run key looks like malware
; persistence to antivirus heuristics; Lasso rewrites it with its own path when it runs.
!macro LassoStop
  ${If} ${FileExists} "$INSTDIR\lasso.exe"
    nsExec::Exec '"$INSTDIR\lasso.exe" --quit'
    Pop $0
  ${EndIf}
  nsExec::Exec `powershell -NoProfile -ExecutionPolicy Bypass -Command "$$d='$INSTDIR\*'; for($$i=0; $$i -lt 10; $$i++){ $$p = Get-Process lasso -ErrorAction SilentlyContinue | Where-Object { $$_.Path -like $$d }; if(-not $$p){ break }; Start-Sleep -Milliseconds 300; $$p | Stop-Process -Force -ErrorAction SilentlyContinue }"`
  Pop $0
!macroend

; The publisher went from "Resister0" to "Sonela" (24 Nov 2026: only GitHub says Resister0). Tauri remembers the
; install folder in HKCU\Software\<publisher>\<app> and looks only there, so without this an update or a reinstall of
; a copy in a folder the user picked would go to the default folder (a 2nd copy), and the reinstall page would start
; the old uninstaller with an empty folder. So: no new entry yet + an old one whose folder still has the program =
; use that folder and copy it to the new entry. Runs before the first page (MUI's GUI-init) and again at the start of
; the install step (silent installs show no pages). Names written out: this file is read before Tauri's !defines.
!define SONELA_OLD_KEY "Software\Resister0\Lasso"
!define SONELA_NEW_KEY "Software\Sonela\Lasso"
!define SONELA_EXE "lasso.exe"
; $R8 = the folder it was going to use (when switched; else ""): the install step has already made it - removed if empty
!macro SonelaOldFolder
  StrCpy $R8 ""
  ReadRegStr $R9 HKCU "${SONELA_NEW_KEY}" ""
  ${If} $R9 == ""
    ReadRegStr $R9 HKCU "${SONELA_OLD_KEY}" ""
    ${If} $R9 != ""
    ${AndIf} ${FileExists} "$R9\${SONELA_EXE}"
      WriteRegStr HKCU "${SONELA_NEW_KEY}" "" $R9
      ${If} $R9 != $INSTDIR
        StrCpy $R8 $INSTDIR
        StrCpy $INSTDIR $R9
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend
Function SonelaGuiInit
  !insertmacro SonelaOldFolder
FunctionEnd
!define MUI_CUSTOMFUNCTION_GUIINIT SonelaGuiInit

!macro NSIS_HOOK_PREINSTALL
  !insertmacro SonelaOldFolder
  SetOutPath $INSTDIR
  ${If} $R8 != ""
    RMDir $R8
  ${EndIf}
  !insertmacro LassoStop
!macroend

; the old entry isn't used any more (Software\Resister0 goes too once Aimdot's is gone)
!macro NSIS_HOOK_POSTINSTALL
  DeleteRegKey HKCU "${SONELA_OLD_KEY}"
  DeleteRegKey /ifempty HKCU "Software\Resister0"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro LassoStop
!macroend
