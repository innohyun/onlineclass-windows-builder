# ClassAiMate 교사 데스크

Windows and macOS Apple Silicon desktop packaging for the loopback SQLite service used by tenant records in `local_sqlite` mode.

## 0.2.81 macOS packaging recovery notes

- Replaces the macOS `0.2.80` image whose app bundle did not have a complete bundle seal. The build now ad-hoc signs the nested MCP sidecar and the whole app, then requires `codesign --verify --deep --strict` before creating the disk image.
- The disk image contains an `Applications` shortcut and Korean installation instructions. Copy the app to Applications, eject the disk image, and launch the copied app; do not keep running it from the mounted image.
- Developer ID signing and Apple notarization are still unavailable. macOS may therefore require the user-authorized `Privacy & Security > Open Anyway` flow. Native service behavior and revision remain `2026-09-12.1-local-sync-safety`.

## 0.2.80 release notes

- Device-local restore intents and transactional commit receipts recover interrupted attachment replacement before serving requests. Uncertain recovery preserves original/rollback files and blocks changes for the affected tenant.
- Equal binding revision conflicts stop application and ACK; current attachment rows are rechecked after staging. MCP attachment access shares the restore guard.
- Separates announced metadata, read-only Cloud Files evidence, and another device's verified application. Status inspection does not hydrate or pin files. Shell tutorial v13 covers these boundaries in four short steps.
- Includes isolated two-device delivery/retry simulation and real process-kill recovery tests. Native service revision: 2026-09-12.1-local-sync-safety. Windows x64 and unsigned, not-notarized macOS Apple Silicon installers are published; actual OneDrive delivery remains a separate verification step.

## 0.2.79 source release notes

- Reads student-record evidence and current semester drafts from the pinned device for the shared web/chat preparation flow. A prepared snapshot is never reported as the current local draft.
- Matches legacy draft records consistently with the web writer, verifies committed rows before ACK, and rechecks exact receipts without duplicate writes after reconnect or lost ACK.
- Reports deterministic draft conflicts separately from unknown outcomes. Bounded local job diagnostics expose only fixed processing stages, safe outcome codes, counts, timestamps and opaque receipts.
- Native service revision: `2026-09-09.2-student-record-recovery`. Public installer and actual two-student verification are tracked in the matching change record.

## 0.2.78 source release notes

- New v4 backup attachments use short opaque paths while preserving original names in the sealed manifest. Existing v4 paths and v5 content-addressed objects remain readable.
- Windows download failures retain the exact file, read offset, request size and Win32 phase/code in the bounded local-only `onedrive-download-diagnostics.log`. File contents, credentials and absolute paths remain absent from remote status and MCP payloads.
- The actual OneDrive 380 incident was reproduced outside the app and resolved by shortening the selected snapshot directory without changing its manifest or file bytes. All 12 artifact hashes matched before the installed app applied generation 320 and published generation 321.
- Native service revision: `2026-09-09.1-onedrive-path-diagnostics`. Public installer delivery is tracked separately from this source version.

## 0.2.75 source release notes

- When the explicit Cloud Files hydration request is rejected with Win32 380, reads only that selected file to EOF in fixed-size chunks to request its content. No folder pinning, recursive download, or OneDrive account reset is performed.
- Keeps a shared bounded deadline for hydration and fallback, safely cancels pending I/O, and prepares incoming files again at restore boundaries without enabling downloads for protective backups.
- Distinguishes provider refusal, authentication, network, permissions and timeout from download pending. Shell guide v11 explains download, verification and preservation of current data on failure.
- Native service revision: `2026-09-08.4-onedrive-380-read-fallback`. Windows native provider tests and installer publication are separate from verification on the user's actual OneDrive PC.

## 0.2.74 source release notes

- MCP lesson observations use the existing local observation store, with up to 200 student records in one atomic write, revision checks and canonical readback.
- Capability `lesson_observations_mcp_v1` adds tenant-bound paginated reads. The previous `observation_evidence_v1` capability and immutable correction history remain required.
- Native service revision: `2026-09-08.3-mcp-lesson-observations`. Public Windows publication requires the builder and installer checks; this source version alone does not change download URLs.

## 0.2.73 source release notes

- Separates actual occurrence date/time (exact, approximate, unknown; Asia/Seoul) from helper-generated save time, including quick observation and its version 2 guide.
- Preserves immutable observation revisions, correction reasons, photo content hashes, atomic batch commitments, idempotent requests, and local export evidence. Hashes do not establish whether the described event happened.
- Automatically requests signed server receipts using the existing active device session and verifies ES256 against the fixed HTTPS public-key registry. Failed/offline requests remain pending; external timestamping is inactive.
- Includes provenance tables in backup/device sync, rejects history replacement and silent rewinds, and preserves divergent heads for explicit reasoned resolution. Older snapshots cannot erase known revisions or photos.
- Native service revision: `2026-09-08.2-observation-evidence`; capability `observation_evidence_v1`. Windows publication requires the public builder gate. Public macOS `0.2.22` remains unsupported for this capability.

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
