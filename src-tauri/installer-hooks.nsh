; Leave "Create desktop shortcut" unchecked on the installer's finish page.
;
; Tauri !includes this file near the top of the generated installer.nsi —
; before MUI_PAGE_FINISH is inserted — so a plain !define here lands in time
; for MUI to read it. That matters: the obvious alternative (forking
; installer.nsi via bundle.windows.nsis.template just to add this one line)
; was tried and broke silent installs outright, popping a full interactive
; wizard for `/S`.
;
; Deliberately no NSIS_HOOK_* macro in this file. A POSTINSTALL hook that
; deleted the shortcut was also tried; it runs before NSIS creates the
; shortcut, so it achieved nothing except making the icon visibly disappear
; and reappear mid-install.
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
