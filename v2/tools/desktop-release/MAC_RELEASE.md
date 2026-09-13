# Apple Silicon 공개 빌드

이 절차는 공통 source의 Mac 설치 파일을 공개 builder에서 빌드한다. 실제 사용자 Mac의 설치·Keychain·OneDrive 동기화 완료를 뜻하지 않는다.

## 선행 조건

- 검증한 private `origin/main` source와 package/Tauri/Cargo 버전을 먼저 commit/push한다.
- `sync-windows-builder.mjs`의 dry-run과 secret scan을 확인하고 깨끗한 별도 builder checkout에 source를 동기화한다. dirty 허용 옵션을 쓰지 않는다.
- Windows와 Mac 빌드는 동일 builder HEAD에서 동시에 시작할 수 있다. 단 Mac publish 단계 전에는 Windows workflow가 성공해 `local-sensitive-store-v{version}` 공개 release, EXE와 Windows manifest가 존재해야 한다.
- 그 Windows release의 target, 실제 tag commit, manifest의 source/builder SHA가 현재 builder HEAD와 일치해야 한다. source sync를 또 해서 builder HEAD가 움직였다면 이전 release에 Mac만 섞어 넣지 않는다.
- 기존 Mac DMG나 receipt가 하나라도 있으면 workflow는 중단한다. 이전/부분 업로드를 먼저 검사하며 자동 덮어쓰기·삭제·재빌드 승격은 하지 않는다.
- Windows publisher도 같은 버전의 release 또는 tag가 이미 있으면 동일 source라도 기본 차단한다. 새 release 생성 뒤 실제 tag·release ID·target SHA를 다시 검증하고 그 release ID에 EXE → manifest 순서로 업로드한다. asset 교체/삭제와 자동 재업로드는 없다. 생성/업로드/readback이 불확실하면 이전 asset을 보존한 채 별도로 진단한다.

## 실행

```sh
gh workflow run local-sensitive-store-macos-release.yml \
  --repo innohyun/onlineclass-windows-builder --ref main \
  -f source_commit=<Windows와-동일한-private-source-SHA>
gh run view <run-id> --repo innohyun/onlineclass-windows-builder
```

수동 dispatch만 지원한다. public builder `main`, exact source input, sourceRepo, sourceDirty=false, clean builder HEAD를 검증한다. `macos-15` arm64 runner에서 Node 22와 Rust host를 확인하고, OS `TMPDIR` 안의 `mktemp -d`로 만든 `ONLINECLASS_LOCAL_STORE_DIR`에서 기존 Node packaging gate와 Rust lib tests를 실행한다. GitHub `RUNNER_TEMP`는 macOS/Rust의 임시 폴더와 다를 수 있으므로 사용자 DB 접근 방지 guard를 완화하지 않고 시험 경로를 OS 임시 폴더에 맞춘다. 산출물/로그 staging은 계속 `RUNNER_TEMP`를 사용한다. native Keychain 상호작용 시험 등 ignored 항목은 통과로 세지 않는다.

Mac publish가 Windows보다 먼저 도착하면 대기 루프나 자동 재시도 없이 실패한다. Windows 성공과 Mac asset 부재를 읽기 전용 확인한 뒤 같은 exact builder SHA에서 `gh run rerun <run-id> --failed --repo innohyun/onlineclass-windows-builder`로 실패 job을 재실행할 수 있다. publish는 현재 재실행 attempt로 이름을 다시 만들지 않고 성공한 build job의 `artifact_name` output을 사용하므로 원래 검증한 artifact를 받는다. 빌드 artifact는 7일간 남으며 만료 시 새 전체 빌드가 필요하다.

공통 `build-installer-mac.mjs`가 arm64 main/sidecar, 현재 source digest, app bundle strict ad-hoc seal, DMG read-only mount 내부 digest·설치 안내·Applications 링크·정상 detach를 검증한다. 별도 publish job만 contents:write를 가진다. publish 직전 source·tag·Windows manifest와 DMG digest를 다시 검사한다.

Windows asset은 이름만 존재해서는 준비 완료가 아니다. Mac publish guard는 두 Windows asset의 `state=uploaded`, 양수 size와 digest를 확인하며 EXE digest가 Windows manifest의 SHA-256과 정확히 같아야 한다. 초기 업로드의 `starter` 상태, 0 bytes 또는 hash 불일치는 재시도 성공으로 숨기지 않고 게시를 막는다.

출력은 다음 두 파일만이다.

- `OnlineClass-Local-Sensitive-Store-macOS-arm64.dmg`
- `local-sensitive-store-macos-build.json`

공개 receipt의 sourceCommit은 private source, builderCommit과 native.source.commit은 public builder HEAD다. runner 절대 경로·DB·credential은 공개 receipt에 넣지 않는다. 업로드 뒤 GitHub asset state·bytes·SHA-256 metadata를 다시 대조한다. 실패 로그는 7일 GitHub Actions artifact로 남고, release asset upload 실패는 부분 업로드 가능성을 명시한다. 재시도 전에 실제 asset 상태를 읽어야 한다. 원격 readback 실패를 재업로드 근거로 삼지 않는다.

## 서명과 실제 사용자 검증 경계

`signing=unsigned`, `notarized=false`는 Developer ID 서명과 Apple 공증이 없음을 뜻한다. native receipt의 `ad-hoc-bundle-no-developer-id-no-notarization`은 bundle 무결성 seal만 증명한다. Gatekeeper나 quarantine을 자동 해제하지 않는다.

실제 사용자는 Applications에 복사 → DMG 정상 추출 → Applications의 앱 실행 순서로 설치한다. 사용자 승인, Keychain 지속성, 기존 WebView 로그인, OneDrive 계정/전달 및 두 기기 적용 완료는 별도 검증이다. OS가 `User interaction is not allowed`를 반환한 native Keychain 시험을 mock 성공으로 대신하지 않는다.

## 웹 다운로드 전달

GitHub 공개 asset의 크기/digest와 직접 재다운로드한 bytes를 비교한다. Windows manifest updater는 기존 Mac 항목을 보존할 뿐 갱신하지 않으므로 Mac DMG mirror와 `platforms.macos` source/builder/version/hash를 공개 receipt에 맞춰 별도로 갱신한다.

설정/홈 버전 판정·다운로드·튜토리얼의 desktop/mobile Playwright, 관련 회귀와 specs/isolation gate 후 clean `main == origin/main`에서 reviewed V3 Pages-only 배포를 수행한다. 고유/default/`t` 세 origin의 manifest·두 설치기·변경 UI asset SHA-256까지 같아야 웹 전달 완료다. Worker·D1/R2·cron·사용자 DB·기존 revision은 변경하지 않는다.
