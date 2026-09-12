import { copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, renameSync, rmSync, rmdirSync, symlinkSync, writeFileSync } from "node:fs";
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
  // Apple credentials are unavailable, so create a complete ad-hoc bundle seal while
  // keeping the release receipt explicit that Developer ID signing/notarization are absent.
  requireCommandSuccess(run("npx", ["tauri", "bundle", "--config", "src-tauri/tauri.sidecar.conf.json", "--bundles", "app", "--no-sign"]), "Tauri app bundle");
  assertSourceUnchanged();
  if (digestPath(preparedSidecar) !== preparedSidecarDigest) {
    throw new Error("Tauri 빌드 중 준비된 MCP sidecar가 변경됐습니다.");
  }
  const sourceBinaries = verifyMacApp({ appPath, releaseDir, preparedSidecar, metadata, run });
  // xattr follows directory symlinks, so reject any bundle link that escapes the app
  // before recursively clearing build-time metadata.
  digestPath(appPath);
  requireCommandSuccess(run("/usr/bin/xattr", ["-cr", appPath]), "app 확장 속성 정리");
  const bundledSidecar = path.join(appPath, "Contents", "MacOS", sidecarName);
  requireCommandSuccess(run("/usr/bin/codesign", ["--force", "--sign", "-", "--timestamp=none", bundledSidecar]), "MCP sidecar ad-hoc 서명");
  requireCommandSuccess(run("/usr/bin/codesign", ["--force", "--sign", "-", "--timestamp=none", appPath]), "app bundle ad-hoc 서명");
  requireCommandSuccess(run("/usr/bin/codesign", ["--verify", "--deep", "--strict", "--verbose=4", appPath]), "app bundle strict 서명 검증");
  const binaries = verifyMacApp({ appPath, releaseDir, preparedSidecar, metadata, run, verifySource: false });
  const appSha256 = digestPath(appPath);

  mkdirSync(dmgDir, { recursive: true });
  const stagingDir = mkdtempSync(path.join(dmgDir, ".mac-build-"));
  const payloadDir = path.join(stagingDir, "payload");
  mkdirSync(payloadDir);
  const stagedAppPath = path.join(payloadDir, `${metadata.appName}.app`);
  cpSync(appPath, stagedAppPath, { recursive: true });
  symlinkSync("/Applications", path.join(payloadDir, "Applications"));
  writeFileSync(path.join(payloadDir, "설치 안내.txt"), [
    "ClassAiMate 교사 데스크 설치 안내",
    "",
    "1. 앱을 Applications 폴더로 드래그해 복사합니다.",
    "2. 복사가 끝나면 이 디스크 이미지를 추출(꺼내기)합니다.",
    "3. Applications 폴더에서 앱을 실행합니다.",
    "4. 개발자 확인 경고가 나오면 시스템 설정 > 개인정보 보호 및 보안에서 '그래도 열기'를 선택합니다.",
    "",
    "이 앱은 Developer ID 서명 및 Apple 공증이 아직 적용되지 않았습니다.",
  ].join("\n"), { flag: "wx" });
  const stagedDmg = path.join(stagingDir, path.basename(dmgPath));
  log("[build-installer-mac] Creating a DMG from the verified ad-hoc sealed app.");
  requireCommandSuccess(run("hdiutil", ["create", "-volname", metadata.appName, "-srcfolder", payloadDir, "-format", "UDZO", stagedDmg]), "hdiutil create");
  requireCommandSuccess(run("hdiutil", ["verify", stagedDmg]), "hdiutil verify");
  assertSourceUnchanged();
  if (digestPath(appPath) !== appSha256) throw new Error("DMG 생성 중 검증한 .app이 변경됐습니다.");
  if (digestPath(stagedAppPath) !== appSha256) throw new Error("DMG payload의 .app이 검증한 .app과 일치하지 않습니다.");

  const mountDir = path.join(stagingDir, "mounted");
  mkdirSync(mountDir);
  let attached = false;
  let mountedVerificationError = null;
  try {
    requireCommandSuccess(run("hdiutil", ["attach", "-readonly", "-nobrowse", "-mountpoint", mountDir, stagedDmg]), "hdiutil attach");
    attached = true;
    const mountedAppPath = path.join(mountDir, `${metadata.appName}.app`);
    if (digestPath(mountedAppPath) !== appSha256) throw new Error("DMG 내부 .app이 검증한 .app과 일치하지 않습니다.");
    if (!readFileSync(path.join(mountDir, "설치 안내.txt"), "utf8").includes("Applications 폴더로 드래그해 복사")) {
      throw new Error("DMG 내부 설치 안내가 없습니다.");
    }
    if (readlinkSync(path.join(mountDir, "Applications")) !== "/Applications") {
      throw new Error("DMG 내부 Applications 바로가기가 올바르지 않습니다.");
    }
    requireCommandSuccess(run("/usr/bin/codesign", ["--verify", "--deep", "--strict", "--verbose=4", mountedAppPath]), "DMG 내부 app bundle strict 서명 검증");
  } catch (error) {
    mountedVerificationError = error;
  }
  if (attached) {
    try {
      requireCommandSuccess(run("hdiutil", ["detach", mountDir]), "hdiutil detach");
    } catch (detachError) {
      if (mountedVerificationError) {
        throw new AggregateError([mountedVerificationError, detachError], "DMG 내부 검증과 안전한 추출이 모두 실패했습니다.");
      }
      throw detachError;
    }
  }
  if (mountedVerificationError) throw mountedVerificationError;
  if (existsSync(mountDir)) rmdirSync(mountDir);
  const dmgSha256 = digestPath(stagedDmg);
  const receipt = {
    schemaVersion: 2,
    builtAt: new Date().toISOString(),
    version: metadata.version,
    identifier: metadata.identifier,
    target: host,
    signing: "ad-hoc-bundle-no-developer-id-no-notarization",
    installationLayout: "drag-app-to-applications",
    source,
    appPath,
    appSha256,
    dmgPath,
    dmgSha256,
    sourceBinaries,
    binaries,
    recoveryDir,
  };
  const stagedReceipt = path.join(stagingDir, "build.json");
  writeFileSync(stagedReceipt, `${JSON.stringify(receipt, null, 2)}\n`, { flag: "wx" });
  rmSync(payloadDir, { recursive: true });
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
