# ClassAiMate 교사 데스크

Windows installer for the loopback SQLite service used by tenant observation records in `local_sqlite` mode.

## 0.2.72 source release notes

- Adds durable student-record workspace metadata to the existing tenant SQLite draft-set table, including selected evidence, review fingerprints, supplements, prompt snapshots, and draft links.
- Requires `expectedRevision` for workspace writes and advertises `student_record_workspace_v1`. The browser verifies an exact readback before reporting a saved workspace.
- Preserves the existing backup/device-sync tracking and per-area MCP draft write jobs; no new database table or snapshot format is required.
- Native service revision: `2026-09-08.1-student-record-workspace`. Public Windows installer publication follows the verified builder release; the public macOS `0.2.22` download remains unchanged.

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
