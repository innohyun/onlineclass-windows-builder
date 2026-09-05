import { copyFileSync, existsSync, mkdirSync, mkdtempSync, renameSync, rmdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  captureMacBuildSource,
  digestPath,
  readMacBuildMetadata,
  requireCommandSuccess,
  verifyMacApp,
} from "./mac-installer-validation.mjs";

const defaultProjectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sidecarName = "classaimate-student-record-mcp";

export function buildMacInstaller({
  projectRoot = defaultProjectRoot,
  platform = process.platform,
  arch = process.arch,
  run = (command, args, options = {}) => spawnSync(command, args, {
    cwd: projectRoot,
    env: process.env,
    encoding: "utf8",
    stdio: "inherit",
    ...options,
  }),
  captureSource = () => captureMacBuildSource(projectRoot, run),
  log = console.log,
} = {}) {
  if (platform !== "darwin" || arch !== "arm64") {
    throw new Error("이번 Mac 설치본은 Apple Silicon macOS에서만 빌드합니다.");
  }
  const source = captureSource();
  const metadata = readMacBuildMetadata(projectRoot);
  const host = requireCommandSuccess(run("rustc", ["-vV"], { stdio: "pipe" }), "rustc")
    .split(/\r?\n/u).find((line) => line.startsWith("host: "))?.slice(6).trim();
  if (host !== "aarch64-apple-darwin") throw new Error("Rust host가 aarch64-apple-darwin이 아닙니다.");

  const tauriRoot = path.join(projectRoot, "src-tauri");
  const releaseDir = path.join(tauriRoot, "target", "release");
  const bundleDir = path.join(releaseDir, "bundle");
  const appPath = path.join(bundleDir, "macos", `${metadata.appName}.app`);
  const dmgDir = path.join(bundleDir, "dmg");
  const dmgPath = path.join(dmgDir, `${metadata.appName}_${metadata.version}_aarch64.dmg`);
  const receiptPath = `${dmgPath}.build.json`;
  const preparedSidecar = path.join(tauriRoot, "binaries", `${sidecarName}-${host}`);
  const previousPaths = [appPath, dmgPath, receiptPath, preparedSidecar].filter(existsSync);
  let recoveryDir = null;
  if (previousPaths.length) {
    mkdirSync(bundleDir, { recursive: true });
    recoveryDir = mkdtempSync(path.join(bundleDir, "previous-mac-build-"));
    for (const previousPath of previousPaths) {
      renameSync(previousPath, path.join(recoveryDir, path.basename(previousPath)));
    }
    log(`[build-installer-mac] 이전 산출물 보존: ${recoveryDir}`);
  }

  const assertSourceUnchanged = () => {
    if (JSON.stringify(captureSource()) !== JSON.stringify(source)) {
      throw new Error("빌드 도중 source commit 또는 파일이 변경됐습니다. 새 source로 다시 빌드하세요.");
    }
  };
  // Tauri adds --bins even when --bin is provided. Build both binaries once,
  // then stage that exact sidecar and bundle without invoking Cargo again.
  requireCommandSuccess(run("npx", ["tauri", "build", "--no-bundle"]), "Tauri native build");
  assertSourceUnchanged();
  mkdirSync(path.dirname(preparedSidecar), { recursive: true });
  copyFileSync(path.join(releaseDir, sidecarName), preparedSidecar);
  const preparedSidecarDigest = digestPath(preparedSidecar);
  if (preparedSidecarDigest !== digestPath(path.join(releaseDir, sidecarName))) {
    throw new Error("준비한 MCP sidecar가 현재 release 실행파일과 일치하지 않습니다.");
  }
  assertSourceUnchanged();

  // A failed app build must never turn an older .app into a successful installer.
  // Local-only packaging skips Apple signing credentials and notarization explicitly.
  requireCommandSuccess(run("npx", ["tauri", "bundle", "--config", "src-tauri/tauri.sidecar.conf.json", "--bundles", "app", "--no-sign"]), "Tauri app bundle");
  assertSourceUnchanged();
  if (digestPath(preparedSidecar) !== preparedSidecarDigest) {
    throw new Error("Tauri 빌드 중 준비된 MCP sidecar가 변경됐습니다.");
  }
  const binaries = verifyMacApp({ appPath, releaseDir, preparedSidecar, metadata, run });
  const appSha256 = digestPath(appPath);

  mkdirSync(dmgDir, { recursive: true });
  const stagingDir = mkdtempSync(path.join(dmgDir, ".mac-build-"));
  const stagedDmg = path.join(stagingDir, path.basename(dmgPath));
  log("[build-installer-mac] Creating a plain unsigned DMG from the verified current .app.");
  requireCommandSuccess(run("hdiutil", ["create", "-volname", metadata.appName, "-srcfolder", appPath, "-format", "UDZO", stagedDmg]), "hdiutil create");
  requireCommandSuccess(run("hdiutil", ["verify", stagedDmg]), "hdiutil verify");
  assertSourceUnchanged();
  if (digestPath(appPath) !== appSha256) throw new Error("DMG 생성 중 검증한 .app이 변경됐습니다.");
  const dmgSha256 = digestPath(stagedDmg);
  const receipt = {
    schemaVersion: 1,
    builtAt: new Date().toISOString(),
    version: metadata.version,
    identifier: metadata.identifier,
    target: host,
    signing: "local-only-no-sign-no-notarization",
    source,
    appPath,
    appSha256,
    dmgPath,
    dmgSha256,
    binaries,
    recoveryDir,
  };
  const stagedReceipt = path.join(stagingDir, "build.json");
  writeFileSync(stagedReceipt, `${JSON.stringify(receipt, null, 2)}\n`, { flag: "wx" });
  renameSync(stagedDmg, dmgPath);
  renameSync(stagedReceipt, receiptPath);
  // Failed staging artifacts remain available for diagnosis instead of being accepted.
  rmdirSync(stagingDir);
  log(`[build-installer-mac] 검증 영수증: ${receiptPath}`);
  return receipt;
}

// Keep imports side-effect-free so failure paths can be exercised without native builds.
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    buildMacInstaller();
  } catch (error) {
    console.error(`[build-installer-mac] ${error.message}`);
    process.exitCode = 1;
  }
}
