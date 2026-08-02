# OnlineClass Local Sensitive Store

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
