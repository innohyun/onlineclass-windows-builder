import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { constants, copyFileSync, lstatSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { BUILDER_REPO, validateBuildSource } from "./mac-release-workflow.mjs";

const EXE = "OnlineClass-Local-Sensitive-Store-Setup.exe";
const MANIFEST = "local-sensitive-store-latest.json";

export function validateWindowsArtifacts(artifacts) {
  assert.equal(artifacts.length, 2, "exact Windows EXE and manifest are required");
  assert.deepEqual(artifacts.map((artifact) => artifact.name), [EXE, MANIFEST], "EXE must precede the manifest");
  for (const artifact of artifacts) {
    assert.ok(typeof artifact.path === "string" && artifact.path.length > 0);
    assert.ok(Number.isSafeInteger(artifact.bytes) && artifact.bytes > 0, "empty Windows artifact");
    assert.match(artifact.sha256, /^[a-f0-9]{64}$/u);
  }
}

function validateCreatedRelease(authority, release, tagCommit, expectedId) {
  assert.ok(Number.isSafeInteger(release?.id) && release.id > 0, "release creation/readback is uncertain");
  if (expectedId !== undefined) assert.equal(release.id, expectedId, "release identity changed");
  assert.equal(release.tag_name, authority.releaseTag);
  assert.equal(release.target_commitish, authority.builderCommit, "release target changed");
  assert.equal(tagCommit, authority.builderCommit, "release tag does not identify the built source");
  assert.equal(release.draft, false);
  assert.equal(release.prerelease, false);
}

export async function publishNewWindowsRelease(authority, artifacts, api) {
  validateWindowsArtifacts(artifacts);
  // A previous successful OR partial publication requires inspection, not reuse.
  assert.equal(await api.getRelease(), null, "Windows release already exists; do not replace assets");
  assert.equal(await api.getTagCommit(), null, "Windows tag already exists; do not reuse its version");
  const created = await api.create(authority);
  assert.ok(Number.isSafeInteger(created?.id) && created.id > 0, "release creation response is uncertain");
  const release = await api.getRelease();
  validateCreatedRelease(authority, release, await api.getTagCommit(), created.id);
  assert.deepEqual(release.assets, [], "new release acquired unexpected assets before upload");
  // The upload adapter uses this release ID, not a tag re-resolved after the check.
  await api.upload(release.id, artifacts);
  const uploaded = await api.getRelease();
  validateCreatedRelease(authority, uploaded, await api.getTagCommit(), release.id);
  for (const expected of artifacts) {
    const asset = uploaded.assets?.find((entry) => entry.name === expected.name);
    assert.equal(asset?.state, "uploaded", "asset upload/readback incomplete; do not re-upload");
    assert.equal(asset.size, expected.bytes, "asset size mismatch; do not re-upload");
    assert.equal(asset.digest, `sha256:${expected.sha256}`, "asset digest mismatch; do not re-upload");
  }
}

function inspect(file, name = path.basename(file)) {
  const stat = lstatSync(file);
  assert.ok(stat.isFile() && !stat.isSymbolicLink() && stat.size > 0, "artifact must be a nonempty regular file");
  return { name, path: file, bytes: stat.size, sha256: createHash("sha256").update(readFileSync(file)).digest("hex") };
}

export function createWindowsReleaseApi(authority, { token = process.env.GH_TOKEN, fetcher = fetch } = {}) {
  assert.ok(token, "workflow token is required");
  async function request(relative, { method = "GET", body, upload = false, missing = false } = {}) {
    const origin = upload ? "https://uploads.github.com" : "https://api.github.com";
    const response = await fetcher(`${origin}/repos/${BUILDER_REPO}/${relative}`, {
      method, signal: AbortSignal.timeout(upload ? 120000 : 30000),
      headers: { Authorization: `Bearer ${token}`, Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28", "Content-Type": upload ? "application/octet-stream" : "application/json" },
      ...(body === undefined ? {} : { body: upload ? body : JSON.stringify(body) }),
    });
    if (missing && response.status === 404) return null;
    assert.ok(response.ok, `GitHub ${method} failed (${response.status}); inspect release state before retrying`);
    return response.json();
  }
  return {
    getRelease: () => request(`releases/tags/${authority.releaseTag}`, { missing: true }),
    async getTagCommit() {
      const reference = await request(`git/ref/tags/${authority.releaseTag}`, { missing: true });
      if (!reference) return null;
      let object = reference.object;
      for (let depth = 0; object?.type === "tag" && depth < 5; depth++) {
        assert.match(object.sha, /^[a-f0-9]{40}$/u);
        object = (await request(`git/tags/${object.sha}`)).object;
      }
      assert.equal(object?.type, "commit", "release tag cannot be resolved");
      return object.sha;
    },
    create: () => request("releases", { method: "POST", body: { tag_name: authority.releaseTag,
      target_commitish: authority.builderCommit, name: `ClassAiMate 교사 데스크 ${authority.version}`,
      body: `Windows installer from ${authority.sourceRepo}@${authority.sourceCommit}. Builder: ${authority.builderCommit}.`,
      draft: false, prerelease: false, make_latest: "true" } }),
    async upload(releaseId, artifacts) {
      assert.ok(Number.isSafeInteger(releaseId) && releaseId > 0);
      // EXE first, then manifest: a parallel Mac cannot see the manifest before the EXE finishes.
      for (const artifact of artifacts) {
        const current = inspect(artifact.path, artifact.name);
        assert.deepEqual(current, artifact, "staged Windows artifact changed before upload");
        const uploaded = await request(`releases/${releaseId}/assets?name=${encodeURIComponent(artifact.name)}`,
          { method: "POST", upload: true, body: readFileSync(artifact.path) });
        assert.equal(uploaded.state, "uploaded");
        assert.equal(uploaded.size, artifact.bytes);
        assert.equal(uploaded.digest, `sha256:${artifact.sha256}`);
      }
    },
  };
}

async function main() {
  assert.equal(process.platform, "win32", "Windows runner is required");
  assert.equal(process.env.GITHUB_EVENT_NAME, "workflow_dispatch");
  const json = (file) => JSON.parse(readFileSync(file, "utf8").replace(/^\uFEFF/u, ""));
  const git = (...args) => execFileSync("git", args, { encoding: "utf8", timeout: 30000 }).trim();
  const project = path.resolve("v2/local-sensitive-store-desktop");
  const authority = validateBuildSource({ source: json("builder-source.json"), sourceCommit: process.env.SOURCE_COMMIT_INPUT,
    builderCommit: git("rev-parse", "HEAD"), workflowCommit: process.env.GITHUB_SHA,
    repository: process.env.GITHUB_REPOSITORY, ref: process.env.GITHUB_REF,
    version: json(path.join(project, "package.json")).version, dirty: Boolean(git("status", "--porcelain")) });
  assert.ok(!process.env.RELEASE_TAG_INPUT || process.env.RELEASE_TAG_INPUT === authority.releaseTag, "release tag must match package version");
  const bundle = path.join(project, "src-tauri/target/release/bundle/nsis");
  const installers = readdirSync(bundle, { withFileTypes: true }).filter((entry) => entry.isFile() && entry.name.endsWith(".exe"));
  assert.equal(installers.length, 1, "expected exactly one newly built NSIS installer");
  const source = inspect(path.join(bundle, installers[0].name));
  assert.ok(process.env.RUNNER_TEMP && path.isAbsolute(process.env.RUNNER_TEMP));
  const stage = mkdtempSync(path.join(process.env.RUNNER_TEMP, "classaimate-windows-release-"));
  const exe = path.join(stage, EXE);
  copyFileSync(source.path, exe, constants.COPYFILE_EXCL);
  const manifest = { name: "ClassAiMate 교사 데스크", ...authority, platform: "windows", fileName: EXE,
    downloadUrl: `https://github.com/${BUILDER_REPO}/releases/download/${authority.releaseTag}/${EXE}`,
    status: "available", available: true, sha256: source.sha256, releasedAt: new Date().toISOString() };
  const manifestPath = path.join(stage, MANIFEST);
  writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, { flag: "wx" });
  await publishNewWindowsRelease(authority, [inspect(exe), inspect(manifestPath)], createWindowsReleaseApi(authority));
  console.log(`Published and verified ${authority.releaseTag}: source=${authority.sourceCommit} builder=${authority.builderCommit}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(`${error.message}\nNo automatic replacement, deletion or upload retry was attempted. Inspect any partial release before continuing.`);
    process.exitCode = 1;
  });
}
