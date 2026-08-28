# ClassAiMate 교사 데스크

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

The build first compiles `classaimate-student-record-mcp` and bundles it as a Tauri external binary. This local stdio sidecar is the only MCP process used by the private ChatGPT Secure MCP Tunnel pilot.
