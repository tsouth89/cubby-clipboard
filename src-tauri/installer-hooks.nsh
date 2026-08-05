; NSIS hooks for the Cubby Clipboard installer.
;
; The autostart entry is written at runtime by tauri-plugin-autostart when the
; user turns on "start with Windows" -- the installer never creates it, so the
; default uninstaller does not know to remove it. Without the hook below,
; uninstalling leaves HKCU\...\Run\Cubby Clipboard pointing at a deleted
; cubby.exe: a failed startup entry on every boot, and a dangling Run value is
; itself something antimalware heuristics score against the machine.

!macro NSIS_HOOK_PREINSTALL
!macroend

!macro NSIS_HOOK_POSTINSTALL
!macroend

!macro NSIS_HOOK_PREUNINSTALL
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; installMode is currentUser, so the entry is always under HKCU. Deleting a
  ; value that was never created is not an error in NSIS, so this is safe for
  ; users who never enabled autostart.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Cubby Clipboard"
!macroend
