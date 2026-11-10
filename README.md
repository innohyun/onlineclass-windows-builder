# OnlineClass Windows Builder

Public build-only mirror for OnlineClass Windows and Apple Silicon Mac installers.

This repository intentionally contains only the installer source needed for:

- `v2/local-sensitive-store-desktop`
- `v2/desktop-shell`
- The reviewed shared document editor modules in `v2/apps/web/src` and the standalone Markdown parser in `v2/packages/domain`

It must not contain private app history, Firebase credentials, `.env` files, classroom data, WIKI files, or service-account material.

## Release rule

Every workflow run requires `source_commit`. The workflow compares that value with `builder-source.json.sourceCommit` and fails if they differ, so an installer cannot be built from stale mirrored source by accident.
Mac build, strict bundle validation, release prerequisites and remaining user-device checks are documented in [the Mac release runbook](v2/tools/desktop-release/MAC_RELEASE.md).

For isolated V4 desktop QA, run `v4-windows-build.yml` with `qa_debug` enabled, a pinned `source_commit`, and the matching `version`. Use a temporary read-only deploy key for the V4 source in the `V4_QA_READ_ONLY_DEPLOY_KEY` environment secret, and revoke it after checkout. Download `v4-windows-x64-desktop-qa` and verify the executable against `Desktop-QA.json`. The executable requires `CLASSAIMATE_V4_DESKTOP_QA=1`, separate store and WebView2 directories under TEMP, and matching loopback authorization and sync origins.
