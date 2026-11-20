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

!macro NSIS_HOOK_PREINSTALL
  !insertmacro LassoStop
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro LassoStop
!macroend
