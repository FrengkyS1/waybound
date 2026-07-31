; Silent/passive installs unconditionally create a desktop shortcut before
; this hook runs (see the generated installer.nsi's install section) — this
; removes it right after, so silent installs never leave one behind.
!macro NSIS_HOOK_POSTINSTALL
  Delete "$DESKTOP\${PRODUCTNAME}.lnk"
!macroend
