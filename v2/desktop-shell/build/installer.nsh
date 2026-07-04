!macro customInstall
  ; Clean up legacy shortcut names from previous builds.
  Delete "$DESKTOP\OnlineClass Desktop Shell.lnk"
  Delete "$DESKTOP\OnlineClass Desktop Shell (Launcher).lnk"
  Delete "$DESKTOP\온라인 학급 운영 프로그램 (런처).lnk"
  Delete "$DESKTOP\교사 대시보드.lnk"
  Delete "$DESKTOP\팀허브.lnk"
  Delete "$DESKTOP\Yearbook.lnk"
  Delete "$DESKTOP\연간시간표.lnk"

  ; Extra shortcut for settings/launcher mode.
  CreateShortCut "$DESKTOP\온라인 학급 운영 프로그램 (런처).lnk" "$INSTDIR\OnlineClass Desktop Shell.exe" "--launcher --app-id=com.onlineclass.desktop-shell.launcher" "$INSTDIR\OnlineClass Desktop Shell.exe" 0
  WinShell::SetLnkAUMI "$DESKTOP\온라인 학급 운영 프로그램 (런처).lnk" "com.onlineclass.desktop-shell.launcher"

  ; Module direct shortcuts.
  CreateShortCut "$DESKTOP\교사 대시보드.lnk" "$INSTDIR\OnlineClass Desktop Shell.exe" "--module=teacher-dashboard --app-id=com.onlineclass.desktop-shell.teacher-dashboard" "$INSTDIR\resources\shortcut-icons\teacher-dashboard.ico" 0
  WinShell::SetLnkAUMI "$DESKTOP\교사 대시보드.lnk" "com.onlineclass.desktop-shell.teacher-dashboard"
  CreateShortCut "$DESKTOP\팀허브.lnk" "$INSTDIR\OnlineClass Desktop Shell.exe" "--module=team-hub --app-id=com.onlineclass.desktop-shell.team-hub" "$INSTDIR\resources\shortcut-icons\team-hub.ico" 0
  WinShell::SetLnkAUMI "$DESKTOP\팀허브.lnk" "com.onlineclass.desktop-shell.team-hub"
  CreateShortCut "$DESKTOP\Yearbook.lnk" "$INSTDIR\OnlineClass Desktop Shell.exe" "--module=yearbook-index --app-id=com.onlineclass.desktop-shell.yearbook" "$INSTDIR\resources\shortcut-icons\yearbook-index.ico" 0
  WinShell::SetLnkAUMI "$DESKTOP\Yearbook.lnk" "com.onlineclass.desktop-shell.yearbook"
!macroend

!macro customUnInstall
  Delete "$DESKTOP\온라인 학급 운영 프로그램 (런처).lnk"
  Delete "$DESKTOP\OnlineClass Desktop Shell (Launcher).lnk"
  Delete "$DESKTOP\온라인 학급 운영 프로그램.lnk"
  Delete "$DESKTOP\OnlineClass Desktop Shell.lnk"
  Delete "$DESKTOP\교사 대시보드.lnk"
  Delete "$DESKTOP\팀허브.lnk"
  Delete "$DESKTOP\Yearbook.lnk"
  Delete "$DESKTOP\연간시간표.lnk"
!macroend
