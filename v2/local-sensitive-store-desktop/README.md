# OnlineClass Local Sensitive Store

`0.2.20`부터 학생 성별·생년월일과 비공개 학생 사진도 로컬 SQLite에 저장합니다. 학생 이름·번호·코드·PIN 확인값은 ClassAimate cloud의 기본 계정 범위이며, raw PIN은 이 프로그램에 저장하지 않습니다. 브라우저 연결은 `/v1/student-private-details`와 `/v1/student-private-photos/{studentCode}`를 사용합니다.

Windows installer for the loopback SQLite service used by tenant observation records in `local_sqlite` mode.

## Development

```powershell
npm --prefix local-sensitive-store-desktop install
npm --prefix local-sensitive-store-desktop run dev:desktop
```

## Build Installer

```powershell
npm --prefix local-sensitive-store-desktop run build:installer
```

The installer artifact is collected into `releases/desktop-unified/latest` by the shared desktop release collector.
