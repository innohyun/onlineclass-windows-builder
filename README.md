# OnlineClass Windows Builder

Public build-only mirror for OnlineClass Windows and Apple Silicon Mac installers.

This repository intentionally contains only the installer source needed for:

- `v2/local-sensitive-store-desktop`
- `v2/desktop-shell`

It must not contain private app history, Firebase credentials, `.env` files, classroom data, WIKI files, or service-account material.

## Release rule

Every workflow run requires `source_commit`. The workflow compares that value with `builder-source.json.sourceCommit` and fails if they differ, so an installer cannot be built from stale mirrored source by accident.
Mac build, strict bundle validation, release prerequisites and remaining user-device checks are documented in [the Mac release runbook](v2/tools/desktop-release/MAC_RELEASE.md).
