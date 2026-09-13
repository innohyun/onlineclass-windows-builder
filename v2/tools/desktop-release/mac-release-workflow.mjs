import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { constants, copyFileSync, lstatSync, mkdtempSync, readFileSync, writeFileSync, appendFileSync } from "node:fs";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { captureMacBuildSource } from "../../local-sensitive-store-desktop/scripts/mac-installer-validation.mjs";

export const SOURCE_REPO = "innohyun/onlineClass-v3";
export const BUILDER_REPO = "innohyun/onlineclass-windows-builder";
export const DMG_NAME = "OnlineClass-Local-Sensitive-Store-macOS-arm64.dmg";
export const RECEIPT_NAME = "local-sensitive-store-macos-build.json";
const WINDOWS_NAME = "OnlineClass-Local-Sensitive-Store-Setup.exe";
const WINDOWS_MANIFEST = "local-sensitive-store-latest.json";
const SIGNING = "ad-hoc-bundle-no-developer-id-no-notarization";
const sha = (value, length) => typeof value === "string" && new RegExp(`^[a-f0-9]{${length}}$`, "u").test(value);

export function validateBuildSource(input) {
  assert.equal(input.repository, BUILDER_REPO, "unexpected builder repository");
  assert.equal(input.ref, "refs/heads/main", "dispatch must use builder main");
  assert.equal(input.source?.sourceRepo, SOURCE_REPO, "unexpected private source repository");
  assert.equal(input.source?.sourceDirty, false, "mirrored source must be clean");
  assert.equal(input.dirty, false, "builder checkout must be clean");
  assert.ok(sha(input.sourceCommit, 40), "invalid source_commit input");
  assert.equal(input.source.sourceCommit, input.sourceCommit, "source_commit does not match mirrored source");
  assert.ok(sha(input.builderCommit, 40), "invalid builder HEAD");
  assert.equal(input.builderCommit, input.workflowCommit, "builder HEAD differs from workflow SHA");
  assert.match(input.version, /^\d+\.\d+\.\d+$/u, "invalid package version");
  return { sourceRepo: SOURCE_REPO, sourceCommit: input.sourceCommit, builderRepo: BUILDER_REPO,
    builderCommit: input.builderCommit, version: input.version, releaseTag: `local-sensitive-store-v${input.version}` };
}

export function validateExistingWindowsRelease(authority, release, tagCommit, windows) {
  assert.equal(release.tag_name, authority.releaseTag, "wrong Windows release tag");
  assert.equal(release.draft, false, "Windows release must already be public");
  assert.equal(release.prerelease, false, "Windows release must not be a prerelease");
  assert.equal(release.target_commitish, authority.builderCommit, "Windows release target must identify this exact builder commit");
  assert.equal(tagCommit, authority.builderCommit, "peeled release tag differs from builder HEAD");
  const names = new Set(release.assets?.map((asset) => asset.name));
  assert.ok(names.has(WINDOWS_NAME) && names.has(WINDOWS_MANIFEST), "Windows release assets are incomplete");
  for (const name of [DMG_NAME, RECEIPT_NAME]) assert.ok(!names.has(name), `${name} already exists; inspect the prior/partial upload, never overwrite it`);
  for (const key of ["sourceRepo", "sourceCommit", "builderRepo", "builderCommit", "version", "releaseTag"]) {
    assert.equal(windows[key], authority[key], `Windows manifest ${key} mismatch`);
  }
  assert.equal(windows.fileName, WINDOWS_NAME);
  assert.equal(windows.platform, "windows");
  assert.ok(sha(windows.sha256, 64), "Windows manifest installer hash is missing");
  for (const name of [WINDOWS_NAME, WINDOWS_MANIFEST]) {
    const asset = release.assets.find((entry) => entry.name === name);
    assert.equal(asset.state, "uploaded", `${name} upload is incomplete`);
    assert.ok(Number.isSafeInteger(asset.size) && asset.size > 0, `${name} size is invalid`);
    assert.match(asset.digest || "", /^sha256:[a-f0-9]{64}$/u, `${name} digest is missing`);
    if (name === WINDOWS_NAME) assert.equal(asset.digest, `sha256:${windows.sha256}`, "Windows EXE digest differs from manifest");
  }
}

export function createPublicMacReceipt(authority, native, artifact) {
  assert.equal(native.schemaVersion, 2, "native build receipt schema mismatch");
  assert.equal(native.version, authority.version, "stale native version");
  assert.equal(native.identifier, "com.onlineclass.local-sensitive-store");
  assert.equal(native.target, "aarch64-apple-darwin");
  assert.equal(native.signing, SIGNING, "strict ad-hoc bundle receipt required");
  assert.equal(native.installationLayout, "drag-app-to-applications");
  assert.equal(native.source?.commit, authority.builderCommit, "native receipt must identify the public builder checkout");
  assert.equal(native.source?.dirty, false);
  assert.ok(sha(native.source?.sha256, 64));
  assert.ok(sha(native.appSha256, 64));
  assert.ok(sha(artifact.sha256, 64));
  assert.equal(native.dmgSha256, artifact.sha256, "DMG differs from validated native build");
  assert.ok(Number.isSafeInteger(artifact.bytes) && artifact.bytes > 0, "empty/invalid DMG size");
  assert.ok(typeof native.builtAt === "string" && Number.isFinite(Date.parse(native.builtAt)), "native build time missing");
  const binaries = (group) => Object.fromEntries(["local-sensitive-store-desktop", "classaimate-student-record-mcp"].map((name) => {
    const entry = group?.[name];
    assert.equal(entry?.architecture, "arm64", `missing/wrong architecture: ${name}`);
    assert.ok(sha(entry?.sha256, 64), `invalid binary hash: ${name}`);
    return [name, { architecture: entry.architecture, sha256: entry.sha256 }];
  }));
  return { schemaVersion: 1, ...authority, platform: "macos", arch: "arm64", fileName: DMG_NAME,
    sha256: artifact.sha256, bytes: artifact.bytes, releasedAt: native.builtAt,
    downloadUrl: `https://github.com/${BUILDER_REPO}/releases/download/${authority.releaseTag}/${DMG_NAME}`,
    signing: "unsigned", notarized: false,
    verificationLimits: ["Developer ID signing and Apple notarization are absent", "native Keychain interaction tests remain ignored", "user installation, WebView login and live OneDrive transport are not verified by this release build"],
    native: { schemaVersion: native.schemaVersion, version: native.version, identifier: native.identifier,
      target: native.target, signing: native.signing, installationLayout: native.installationLayout,
      source: { commit: native.source.commit, sha256: native.source.sha256, dirty: false },
      appSha256: native.appSha256, dmgSha256: native.dmgSha256, builtAt: native.builtAt,
      sourceBinaries: binaries(native.sourceBinaries), binaries: binaries(native.binaries) } };
}

export function validatePublicMacReceipt(authority, receipt, artifact) {
  assert.deepEqual(receipt, createPublicMacReceipt(authority, receipt.native, artifact), "public receipt/authority or artifact mismatch");
}

const expression = (value) => `\${{ ${value} }}`;
export function localSensitiveMacWorkflow() {
  return `name: ClassAiMate Apple Silicon Mac release
on:
  workflow_dispatch:
    inputs:
      source_commit:
        description: Exact private source commit mirrored into this builder; Windows release must exist before publication.
        required: true
        type: string
permissions:
  contents: read
concurrency:
  group: classaimate-local-store-macos-release
  cancel-in-progress: false
env:
  SOURCE_COMMIT_INPUT: ${expression("inputs.source_commit")}
jobs:
  build:
    runs-on: macos-15
    timeout-minutes: 90
    outputs:
      artifact_name: ${expression("steps.artifact_name.outputs.name")}
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${expression("github.sha")}
          persist-credentials: false
      - uses: actions/setup-node@v4
        with:
          node-version: '22'
          cache: npm
          cache-dependency-path: v2/local-sensitive-store-desktop/package-lock.json
      - uses: dtolnay/rust-toolchain@stable
      - name: Validate source and runner
        run: |
          set -euo pipefail
          test "$(uname -m)" = arm64
          node v2/tools/desktop-release/mac-release-workflow.mjs preflight
          echo "ONLINECLASS_LOCAL_STORE_DIR=$(mktemp -d "$RUNNER_TEMP/classaimate-macos-store.XXXXXX")" >> "$GITHUB_ENV"
      - name: Install locked frontend dependencies
        working-directory: v2/local-sensitive-store-desktop
        run: npm ci
      - name: Existing Node Mac packaging gate
        working-directory: v2
        run: node --test tests/local-sensitive-store-macos-packaging.test.mjs
      - name: Isolated Rust regression (native Keychain interaction tests remain ignored)
        working-directory: v2/local-sensitive-store-desktop
        run: |
          set -euo pipefail
          cargo test --locked --release --manifest-path src-tauri/Cargo.toml --lib -- --test-threads=1 2>&1 | tee "$RUNNER_TEMP/classaimate-macos-rust.log"
      - name: Build and strictly validate the Apple Silicon app and DMG
        working-directory: v2/local-sensitive-store-desktop
        run: |
          set -euo pipefail
          node scripts/build-installer-mac.mjs 2>&1 | tee "$RUNNER_TEMP/classaimate-macos-build.log"
      - name: Stage checked DMG and public provenance receipt
        id: stage
        run: node v2/tools/desktop-release/mac-release-workflow.mjs stage
      - name: Keep the original build artifact identity for failed-job retries
        id: artifact_name
        env:
          ARTIFACT_NAME: macos-release-${expression("github.sha")}-${expression("github.run_attempt")}
        run: echo "name=$ARTIFACT_NAME" >> "$GITHUB_OUTPUT"
      - uses: actions/upload-artifact@v4
        with:
          name: ${expression("steps.artifact_name.outputs.name")}
          path: ${expression("steps.stage.outputs.stage_dir")}
          if-no-files-found: error
          retention-days: 7
      - name: Preserve failure logs (never user DB or credentials)
        if: failure()
        uses: actions/upload-artifact@v4
        with:
          name: macos-failure-${expression("github.sha")}-${expression("github.run_attempt")}
          path: |
            ${expression("runner.temp")}/classaimate-macos-rust.log
            ${expression("runner.temp")}/classaimate-macos-build.log
          if-no-files-found: warn
          retention-days: 7
  publish:
    needs: build
    runs-on: ubuntu-latest
    timeout-minutes: 15
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${expression("github.sha")}
          persist-credentials: false
      - uses: actions/setup-node@v4
        with:
          node-version: '22'
      - uses: actions/download-artifact@v4
        with:
          name: ${expression("needs.build.outputs.artifact_name")}
          path: ${expression("runner.temp")}/macos-release
      - name: Revalidate Windows source/tag and publish new Mac assets only
        env:
          GH_TOKEN: ${expression("github.token")}
          MAC_RELEASE_STAGE_DIR: ${expression("runner.temp")}/macos-release
        run: node v2/tools/desktop-release/mac-release-workflow.mjs publish
`;
}

function command(binary, args) {
  return execFileSync(binary, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 120000 }).trim();
}
const readJson = (file) => JSON.parse(readFileSync(file, "utf8").replace(/^\uFEFF/u, ""));
function loadAuthority() {
  assert.equal(process.env.GITHUB_EVENT_NAME, "workflow_dispatch", "manual dispatch is required");
  const project = path.resolve("v2/local-sensitive-store-desktop");
  const authority = validateBuildSource({ source: readJson("builder-source.json"), sourceCommit: process.env.SOURCE_COMMIT_INPUT,
    builderCommit: command("git", ["rev-parse", "HEAD"]), workflowCommit: process.env.GITHUB_SHA,
    repository: process.env.GITHUB_REPOSITORY, ref: process.env.GITHUB_REF,
    version: readJson(path.join(project, "package.json")).version, dirty: Boolean(command("git", ["status", "--porcelain"])) });
  return { project, authority };
}

function api(relative, args = []) {
  return JSON.parse(command("gh", ["api", `repos/${BUILDER_REPO}/${relative}`, ...args]).replace(/^\uFEFF/u, ""));
}
function checkWindowsRelease(authority) {
  const release = api(`releases/tags/${authority.releaseTag}`);
  let object = api(`git/ref/tags/${authority.releaseTag}`).object;
  for (let depth = 0; object?.type === "tag" && depth < 5; depth++) object = api(`git/tags/${object.sha}`).object;
  assert.equal(object?.type, "commit", "release tag cannot be resolved to a commit");
  const asset = release.assets?.find((entry) => entry.name === WINDOWS_MANIFEST);
  assert.ok(Number.isSafeInteger(asset?.id), "Windows release manifest asset is missing");
  const windows = api(`releases/assets/${asset.id}`, ["-H", "Accept: application/octet-stream"]);
  validateExistingWindowsRelease(authority, release, object.sha, windows);
}

function inspectArtifact(file) {
  const stat = lstatSync(file);
  assert.ok(stat.isFile() && !stat.isSymbolicLink() && stat.size > 0, "artifact must be a nonempty regular file");
  return { bytes: stat.size, sha256: createHash("sha256").update(readFileSync(file)).digest("hex") };
}
function runnerDirectory(value) {
  const root = path.resolve(process.env.RUNNER_TEMP || "");
  assert.ok(process.env.RUNNER_TEMP && path.isAbsolute(process.env.RUNNER_TEMP), "RUNNER_TEMP is required");
  const directory = path.resolve(value);
  assert.ok(directory.startsWith(`${root}${path.sep}`), "release staging must stay inside RUNNER_TEMP");
  return directory;
}

function main(mode) {
  const { project, authority } = loadAuthority();
  if (mode === "preflight") {
    assert.equal(process.platform, "darwin");
    assert.equal(process.arch, "arm64");
    console.log(`Validated ${authority.releaseTag}: source=${authority.sourceCommit} builder=${authority.builderCommit}`);
    return;
  }
  if (mode === "stage") {
    assert.equal(process.platform, "darwin");
    assert.equal(process.arch, "arm64");
    const config = readJson(path.join(project, "src-tauri/tauri.conf.json"));
    assert.ok(typeof config.productName === "string" && !/[\/\\\0]/u.test(config.productName));
    const dmg = path.join(project, "src-tauri/target/release/bundle/dmg", `${config.productName}_${authority.version}_aarch64.dmg`);
    const native = readJson(`${dmg}.build.json`);
    const currentSource = captureMacBuildSource(project, (binary, args, options) => spawnSync(binary, args,
      { encoding: "utf8", cwd: project, ...options }));
    assert.deepEqual(native.source, currentSource, "native receipt source differs from the checked build source");
    const receipt = createPublicMacReceipt(authority, native, inspectArtifact(dmg));
    const stage = mkdtempSync(runnerDirectory(path.join(process.env.RUNNER_TEMP, "classaimate-macos-release-")));
    copyFileSync(dmg, path.join(stage, DMG_NAME), constants.COPYFILE_EXCL);
    writeFileSync(path.join(stage, RECEIPT_NAME), `${JSON.stringify(receipt, null, 2)}\n`, { flag: "wx" });
    assert.ok(process.env.GITHUB_OUTPUT, "GITHUB_OUTPUT is required");
    assert.doesNotMatch(stage, /[\r\n]/u);
    appendFileSync(process.env.GITHUB_OUTPUT, `stage_dir=${stage}\n`);
    console.log(`Staged ${DMG_NAME}: bytes=${receipt.bytes} sha256=${receipt.sha256}`);
    return;
  }
  if (mode === "publish") {
    const stage = runnerDirectory(process.env.MAC_RELEASE_STAGE_DIR || "");
    const dmg = path.join(stage, DMG_NAME);
    const receiptPath = path.join(stage, RECEIPT_NAME);
    assert.ok(lstatSync(receiptPath).isFile() && !lstatSync(receiptPath).isSymbolicLink());
    validatePublicMacReceipt(authority, readJson(receiptPath), inspectArtifact(dmg));
    checkWindowsRelease(authority);
    // No replacement capability: a partial previous upload requires inspection.
    try {
      command("gh", ["release", "upload", authority.releaseTag, dmg, receiptPath, "--repo", BUILDER_REPO]);
    } catch (error) {
      throw new Error("Mac upload failed; inspect any partial assets before retrying. Existing assets must not be overwritten.", { cause: error });
    }
    const uploaded = api(`releases/tags/${authority.releaseTag}`);
    for (const file of [dmg, receiptPath]) {
      const expected = inspectArtifact(file);
      const asset = uploaded.assets?.find((entry) => entry.name === path.basename(file));
      assert.equal(asset?.state, "uploaded", "uploaded asset readback is incomplete; do not re-upload");
      assert.equal(asset.size, expected.bytes, "uploaded asset size mismatch; do not re-upload");
      assert.equal(asset.digest, `sha256:${expected.sha256}`, "uploaded asset digest mismatch; do not re-upload");
    }
    console.log(`Published new Mac assets for ${authority.releaseTag}; Developer ID signing, notarization and live OneDrive validation remain absent.`);
    return;
  }
  throw new Error("Expected preflight, stage or publish");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { main(process.argv[2]); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
