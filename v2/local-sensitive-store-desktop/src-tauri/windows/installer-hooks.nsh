!macro NSIS_HOOK_POSTINSTALL
  CreateShortCut "$DESKTOP\ClassAiMate 빠른 관찰기록.lnk" "$INSTDIR\ClassAiMate 교사 데스크.exe" "--quick-observation" "$INSTDIR\ClassAiMate 교사 데스크.exe" 0 SW_SHOWNORMAL "" "ClassAiMate 빠른 관찰기록"
  CreateShortCut "$SMPROGRAMS\ClassAiMate 빠른 관찰기록.lnk" "$INSTDIR\ClassAiMate 교사 데스크.exe" "--quick-observation" "$INSTDIR\ClassAiMate 교사 데스크.exe" 0 SW_SHOWNORMAL "" "ClassAiMate 빠른 관찰기록"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$DESKTOP\ClassAiMate 빠른 관찰기록.lnk"
  Delete "$SMPROGRAMS\ClassAiMate 빠른 관찰기록.lnk"
!macroend
