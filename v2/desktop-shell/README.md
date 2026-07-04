# Desktop Shell (Electron)

`desktop-shell`은 교사 대시보드/팀허브/연간시간표/수업계획/자리배치를 하나의 설치형 앱에서 실행하는 통합 런처입니다.

## 핵심 포인트

- 런타임: Electron (Chromium 내장)
- 로그인 세션 공유: `persist:onlineclass` 파티션
- 기본 실행: 시작 모듈 자동 진입(런처 비노출)
- 흰화면 복구: main-frame navigation 기준 로드 타임아웃 감시 + `did-fail-load`, `unresponsive`, `render-process-gone` 자동 복구
- 개발(localhost) 완화: 로드 타임아웃을 완화(60초)하고 반복 복구 루프를 자동 일시중지
- 설정 파일: `%AppData%/OnlineClass Desktop Shell/desktop-shell-config.json`
- 레거시 설정 자동 이관: `teacher-dashboard-desktop-config.json` 탐색 후 1회 마이그레이션

## 런처 실행(설정 모드)

- 기본 사용자 실행에서는 런처를 열지 않습니다.
- 설정이 필요하면 실행 파일에 `--launcher` 인자를 붙여 실행합니다.
  - 예: `OnlineClass Desktop Shell.exe --launcher`
- 설치본(NSIS)에는 바탕화면에 런처 전용 바로가기
  `온라인 학급 운영 프로그램 (런처).lnk`를 추가로 생성합니다.
  - 이 바로가기는 자동으로 `--launcher` 인자로 실행됩니다.
- 설치형 앱은 기본적으로 `localhost` baseUrl을 운영 URL로 자동 복구합니다.
- 단, 런처에서 사용자가 `localhost`를 명시 저장한 경우(`allowLocalhostBaseUrl`)에는 테스트용 로컬 URL을 유지합니다.
- 로컬 baseUrl이 `http://localhost:5000/v2/` 또는 `http://127.0.0.1:5002/`이면 모듈 실행 직전에 정적 서버, Firebase Emulator, collab preview 함수 서버(`localhost:8888`)를 자동 보장하고, 모듈 URL에 `useEmulator=true`를 붙입니다.
- checkout 탐색이 실패하는 환경에서는 `ONLINECLASS_WORKSPACE_ROOT` 또는 `ONLINECLASS_V2_ROOT` 환경변수로 워크스페이스 경로를 지정할 수 있습니다.
- collab preview 설정은 `v2/.env.collab.local`을 우선 사용합니다. 새 환경에서는 `v2/.env.collab.local.example`을 복사해 값만 채우면 됩니다.

## 바로가기/아이콘 정책

- 기본 바탕화면/시작메뉴 바로가기 이름은 `온라인 학급 운영 프로그램`으로 생성됩니다.
- 바탕화면에는 모듈 바로가기 3종을 추가 생성합니다.
  - `교사 대시보드.lnk` (`--module=teacher-dashboard`)
  - `팀허브.lnk` (`--module=team-hub`)
  - `Yearbook.lnk` (`--module=yearbook-index`)
- 각 바로가기는 Windows AppUserModelID를 함께 설정해 실행 시 해당 아이콘 자리에서 창이 열리도록 매핑합니다.
- 구버전 설치본에서 남아 있던 `OnlineClass Desktop Shell*.lnk`는 설치 시 자동 정리됩니다.
- 설치형 창 아이콘은 실행 파일 아이콘을 우선 사용해 작업표시줄/독 표시 일관성을 맞춥니다.

## 링크/줌 UX

- 내부 링크(`classaimate.netlify.app` 또는 baseUrl 동일 origin)는 앱 내부 같은 창에서 열립니다.
- 외부 링크만 기본 브라우저로 위임됩니다(`openExternalLinks=true`).
- 단축키:
  - `F5`, `Ctrl/Cmd + R` 새로고침
  - `Shift + F5`, `Ctrl/Cmd + Shift + R` 캐시 무시 새로고침
  - `Ctrl + Wheel` 확대/축소
  - `Ctrl + +`, `Ctrl + -`, `Ctrl + 0`
- 일부 환경에서는 `Ctrl + Wheel` 입력이 달라질 수 있어 `zoom-changed` fallback을 함께 적용합니다.

## 실행

```powershell
cd desktop-shell
npm install
npm run dev
```

## 설치본 빌드

```powershell
cd desktop-shell
npm install
npm run icon:generate
npm run build:installer
```

산출물:

- `desktop-shell/dist/*-setup.exe` (NSIS)
- `desktop-shell/dist/*-win.zip`

설치파일 빠르게 찾기:

- `build:installer` 또는 `build:msi` 실행 후 최신 설치파일은 항상 `v2/releases/desktop-shell/latest`로 자동 복사됩니다.
- 같은 시점의 통합 모음은 `v2/releases/desktop-unified/latest`에 자동 복사됩니다.
- 빌드 산출물 원본은 `desktop-shell/dist`에 유지됩니다.
- 수동 수집/정리는 아래 명령으로 다시 실행할 수 있습니다.

```powershell
npm run release:collect
# 폴더 자동 열기
npm run release:collect:open
# 전체 데스크톱 설치파일 통합 수집(워크스페이스 루트)
cd ..
npm run release:collect:desktop
```

## 자동 업데이트

- `electron-updater` + `generic` provider 사용
- publish URL: `https://classaimate.netlify.app/desktop-updates/`
- 패키징 앱에서 실행 시 자동 확인/다운로드/재시작 설치 동작

## 아이콘 관리

- 앱 아이콘 소스는 `desktop-shell/scripts/generate_icon.py`로 생성합니다.
- 출력 파일:
  - `desktop-shell/build/icon-master-1024.png`
  - `desktop-shell/build/icon.ico`
  - `desktop-shell/build/icon-preview-strip.png`
  - `desktop-shell/build/shortcut-icons/teacher-dashboard.ico`
  - `desktop-shell/build/shortcut-icons/team-hub.ico`
  - `desktop-shell/build/shortcut-icons/yearbook-index.ico`
- 재생성 명령:

```powershell
cd desktop-shell
npm run icon:generate
```

## URL 파라미터

모듈 실행 시 자동 부착:

- `desktop=1`
- `source=desktop-shell`
- `authMode={redirect|auto}`
- `tenantId={값이 있을 때만}`

## 실행 인자

- `--launcher` 또는 `--settings`: 런처(설정 모드) 열기
- `--module=<moduleId>`: 특정 모듈로 직접 진입
- `--app-id=<id>`: Windows 작업표시줄 그룹 식별자(AppUserModelID) 지정
