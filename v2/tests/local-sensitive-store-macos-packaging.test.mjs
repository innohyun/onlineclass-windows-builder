import assert from "node:assert/strict";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { buildMacInstaller } from "../local-sensitive-store-desktop/scripts/build-installer-mac.mjs";
import { captureMacBuildSource, digestPath, readMacBuildMetadata, requireCommandSuccess } from "../local-sensitive-store-desktop/scripts/mac-installer-validation.mjs";

const appName = "ClassAiMate 교사 데스크";
const executable = "local-sensitive-store-desktop";
const sidecarName = "classaimate-student-record-mcp";
const version = "0.2.65";
const identifier = "com.onlineclass.local-sensitive-store";
const write = (file, content, mode) => {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, content);
  if (mode) chmodSync(file, mode);
};
const json = (file, value) => write(file, JSON.stringify(value));

function fixture(t, options = {}) {
  const projectRoot = mkdtempSync(path.join(os.tmpdir(), "classaimate-macos-packaging-test-"));
  t.after(() => rmSync(projectRoot, { recursive: true, force: true }));
  const tauriRoot = path.join(projectRoot, "src-tauri");
  const releaseDir = path.join(tauriRoot, "target/release");
  const bundleDir = path.join(releaseDir, "bundle");
  const appPath = path.join(bundleDir, "macos", `${appName}.app`);
  const dmgPath = path.join(bundleDir, "dmg", `${appName}_${version}_aarch64.dmg`);
  const preparedSidecar = path.join(tauriRoot, "binaries", `${sidecarName}-aarch64-apple-darwin`);
  const metadataPath = path.join(tauriRoot, "tauri.conf.json");
  const metadata = { productName: appName, version, identifier };
  json(path.join(projectRoot, "package.json"), { version });
  json(metadataPath, metadata);
  json(path.join(tauriRoot, "tauri.sidecar.conf.json"), { bundle: { externalBin: [`binaries/${sidecarName}`] } });
  write(path.join(tauriRoot, "Cargo.toml"), `[package]\nname = "${executable}"\nversion = "${version}"\n\n[dependencies]\n`);
  const source = { commit: "a".repeat(40), sha256: "b".repeat(64), dirty: false };
  let sourceChanged = false;
  let createdSourceFolder = null;
  const calls = [];
  const success = (stdout = "") => ({ status: 0, stdout });
  const run = (command, args) => {
    calls.push([command, args]);
    if (command === "rustc") return success(`rustc 1.89.0\nhost: ${options.host || "aarch64-apple-darwin"}\n`);
    if (command === "npx" && args[1] === "build") {
      assert.deepEqual(args, ["tauri", "build", "--no-bundle"]);
      if (options.sidecarFailure) return { status: 3 };
      if (!options.missingPreparedSidecar) {
        write(path.join(releaseDir, sidecarName), "current-sidecar", 0o755);
      }
      write(path.join(releaseDir, executable), "current-main", 0o755);
      if (options.changeSourceAfterSidecar) sourceChanged = true;
      return success();
    }
    if (command === "npx") {
      assert.deepEqual(args, ["tauri", "bundle", "--config", "src-tauri/tauri.sidecar.conf.json", "--bundles", "app", "--no-sign"]);
      if (options.appFailure) return { status: 7 };
      if (options.missingApp) return success();
      write(path.join(appPath, "Contents/MacOS", executable), options.staleMain ? "old-main" : "current-main", 0o755);
      if (!options.missingBundledSidecar) write(path.join(appPath, "Contents/MacOS", sidecarName), options.staleBundledSidecar ? "old-sidecar" : "current-sidecar", 0o755);
      json(path.join(appPath, "Contents/Info.plist"), {
        CFBundleIdentifier: identifier,
        CFBundleShortVersionString: version,
        CFBundleVersion: version,
        CFBundleExecutable: executable,
        ...options.plist,
      });
      if (options.outsideSymlink) {
        const outsidePath = path.join(projectRoot, "outside-bundle-file");
        write(outsidePath, "must-not-be-touched");
        mkdirSync(path.join(appPath, "Contents/Resources"), { recursive: true });
        symlinkSync(outsidePath, path.join(appPath, "Contents/Resources/outside-link"));
      }
      if (options.nonExecutable) chmodSync(path.join(appPath, "Contents/MacOS", sidecarName), 0o644);
      if (options.changePreparedDuringApp) write(preparedSidecar, "changed-during-app", 0o755);
      if (options.changeSourceAfterApp) sourceChanged = true;
      return success();
    }
    if (command === "/usr/bin/plutil") return success(readFileSync(args.at(-1), "utf8"));
    if (command === "/usr/bin/lipo") {
      return success(options.wrongArchitecture === path.basename(args.at(-1)) ? "x86_64" : "arm64");
    }
    if (command === "/usr/bin/xattr") {
      assert.deepEqual(args, ["-cr", appPath]);
      return options.xattrFailure ? { status: 11 } : success();
    }
    if (command === "/usr/bin/codesign") {
      if (args[0] === "--verify") {
        assert.deepEqual(args.slice(0, 4), ["--verify", "--deep", "--strict", "--verbose=4"]);
        if (args.at(-1) === appPath) return options.signatureVerifyFailure ? { status: 14 } : success();
        assert.equal(path.basename(path.dirname(args.at(-1))), "mounted");
        assert.equal(path.basename(args.at(-1)), `${appName}.app`);
        return options.mountedSignatureVerifyFailure ? { status: 15 } : success();
      }
      assert.deepEqual(args.slice(0, 4), ["--force", "--sign", "-", "--timestamp=none"]);
      if (path.basename(args.at(-1)) === sidecarName && options.sidecarSignFailure) return { status: 12 };
      if (args.at(-1) === appPath && options.appSignFailure) return { status: 13 };
      return success();
    }
    if (command === "hdiutil") {
      if (args[0] === "create") {
        const sourceFolder = args[args.indexOf("-srcfolder") + 1];
        createdSourceFolder = sourceFolder;
        assert.ok(existsSync(path.join(sourceFolder, `${appName}.app`)));
        assert.equal(readlinkSync(path.join(sourceFolder, "Applications")), "/Applications");
        assert.match(readFileSync(path.join(sourceFolder, "설치 안내.txt"), "utf8"), /Applications 폴더로 드래그해 복사/u);
        if (options.dmgFailure) return { status: 9 };
        if (!options.missingDmg) write(args.at(-1), "current-dmg");
      } else if (args[0] === "verify") {
        if (options.dmgVerifyFailure) return { status: 10 };
        if (options.changeAppDuringDmg) write(path.join(appPath, "Contents/Resources/changed"), "changed");
        if (options.changeSourceAfterDmg) sourceChanged = true;
      } else if (args[0] === "attach") {
        if (options.attachFailure) return { status: 16 };
        const mountpoint = args[args.indexOf("-mountpoint") + 1];
        assert.deepEqual(args.slice(0, 5), ["attach", "-readonly", "-nobrowse", "-mountpoint", mountpoint]);
        for (const name of readdirSync(createdSourceFolder)) {
          cpSync(path.join(createdSourceFolder, name), path.join(mountpoint, name), { recursive: true });
        }
        if (options.changeMountedApp) write(path.join(mountpoint, `${appName}.app/Contents/Resources/changed`), "changed");
      } else if (args[0] === "detach") {
        if (options.detachFailure) return { status: 17 };
        for (const name of readdirSync(args[1])) rmSync(path.join(args[1], name), { recursive: true, force: true });
      } else assert.fail(`Unexpected hdiutil: ${args}`);
      return success();
    }
    assert.fail(`Unexpected command: ${command}`);
  };
  const execute = (overrides = {}) => buildMacInstaller({
    projectRoot, platform: "darwin", arch: "arm64", run,
    captureSource: () => ({ ...source, ...(sourceChanged ? { sha256: "c".repeat(64) } : {}) }),
    log() {}, ...overrides,
  });
  const seedPrevious = () => {
    write(path.join(appPath, "Contents/previous"), "previous-app");
    write(dmgPath, "previous-dmg");
    write(`${dmgPath}.build.json`, "previous-receipt");
    write(preparedSidecar, "previous-sidecar");
  };
  return { projectRoot, tauriRoot, bundleDir, appPath, dmgPath, preparedSidecar, metadataPath, metadata, calls, execute, seedPrevious, source };
}

test("현재 app·sidecar·DMG를 확인한 뒤 source와 파일 지문을 기록한다", (t) => {
  const f = fixture(t);
  const receipt = f.execute();
  assert.equal(receipt.schemaVersion, 2);
  assert.equal(receipt.version, version);
  assert.equal(receipt.target, "aarch64-apple-darwin");
  assert.equal(receipt.signing, "ad-hoc-bundle-no-developer-id-no-notarization");
  assert.equal(receipt.installationLayout, "drag-app-to-applications");
  assert.deepEqual(receipt.source, f.source);
  assert.equal(receipt.appSha256, digestPath(f.appPath));
  assert.equal(receipt.dmgSha256, digestPath(f.dmgPath));
  assert.equal(receipt.sourceBinaries[sidecarName].sha256, digestPath(f.preparedSidecar));
  assert.equal(receipt.binaries[sidecarName].sha256, digestPath(path.join(f.appPath, "Contents/MacOS", sidecarName)));
  assert.deepEqual(JSON.parse(readFileSync(`${f.dmgPath}.build.json`, "utf8")), receipt);
  const codesignCalls = f.calls.filter(([command]) => command === "/usr/bin/codesign").map(([, args]) => args);
  assert.deepEqual(codesignCalls[0], ["--force", "--sign", "-", "--timestamp=none", path.join(f.appPath, "Contents/MacOS", sidecarName)]);
  assert.deepEqual(codesignCalls[1], ["--force", "--sign", "-", "--timestamp=none", f.appPath]);
  assert.deepEqual(codesignCalls[2], ["--verify", "--deep", "--strict", "--verbose=4", f.appPath]);
  assert.deepEqual(codesignCalls[3].slice(0, 4), ["--verify", "--deep", "--strict", "--verbose=4"]);
  assert.equal(path.basename(codesignCalls[3].at(-1)), `${appName}.app`);
  assert.deepEqual(f.calls.filter(([command]) => command === "hdiutil").map(([, args]) => args[0]), ["create", "verify", "attach", "detach"]);
  assert.ok(!f.calls.some(([, args]) => args.includes("-ov")));
  assert.ok(!readdirSync(path.dirname(f.dmgPath)).some((name) => name.startsWith(".mac-build-")));
});

test("이전 app·DMG·영수증·sidecar를 삭제하지 않고 recovery 폴더에 보존한다", (t) => {
  const f = fixture(t);
  f.seedPrevious();
  const receipt = f.execute();
  assert.equal(readFileSync(path.join(receipt.recoveryDir, path.basename(f.dmgPath)), "utf8"), "previous-dmg");
  assert.equal(readFileSync(path.join(receipt.recoveryDir, `${appName}.app/Contents/previous`), "utf8"), "previous-app");
  assert.equal(readFileSync(path.join(receipt.recoveryDir, path.basename(f.preparedSidecar)), "utf8"), "previous-sidecar");
  assert.equal(readFileSync(f.dmgPath, "utf8"), "current-dmg");
});

test("Tauri 실패 뒤 남은 옛 app을 DMG로 승격시키지 않는다", (t) => {
  const f = fixture(t, { appFailure: true });
  f.seedPrevious();
  assert.throws(() => f.execute(), /Tauri app bundle 실패/u);
  assert.ok(!f.calls.some(([command]) => command === "hdiutil"));
  assert.ok(!existsSync(f.appPath));
  assert.ok(!existsSync(f.dmgPath));
  assert.ok(!existsSync(`${f.dmgPath}.build.json`));
  const recovery = readdirSync(f.bundleDir).find((name) => name.startsWith("previous-mac-build-"));
  assert.equal(readFileSync(path.join(f.bundleDir, recovery, path.basename(f.dmgPath)), "utf8"), "previous-dmg");
});

test("app 밖 symlink는 재귀 xattr 정리 전에 거부한다", (t) => {
  const f = fixture(t, { outsideSymlink: true });
  assert.throws(() => f.execute(), /앱 밖을 가리키는 symlink/u);
  assert.ok(!f.calls.some(([command]) => command === "/usr/bin/xattr"));
  assert.ok(!f.calls.some(([command]) => command === "/usr/bin/codesign"));
  assert.ok(!f.calls.some(([command]) => command === "hdiutil"));
});

for (const [name, options, error] of [
  ["native 빌드 실패", { sidecarFailure: true }, /Tauri native build 실패/u],
  ["sidecar 산출물 없음", { missingPreparedSidecar: true }, /ENOENT/u],
  ["성공 응답 뒤 새 app 없음", { missingApp: true }, /ENOENT/u],
  ["app version 불일치", { plist: { CFBundleShortVersionString: "0.2.22" } }, /metadata와 일치/u],
  ["bundle version 불일치", { plist: { CFBundleVersion: "0.2.22" } }, /metadata와 일치/u],
  ["bundle identifier 불일치", { plist: { CFBundleIdentifier: "other.app" } }, /metadata와 일치/u],
  ["다른 main 실행파일", { plist: { CFBundleExecutable: sidecarName } }, /metadata와 일치/u],
  ["main 실행파일이 오래됨", { staleMain: true }, /현재 빌드와 일치/u],
  ["번들 sidecar가 없음", { missingBundledSidecar: true }, /ENOENT/u],
  ["번들 sidecar가 오래됨", { staleBundledSidecar: true }, /현재 빌드와 일치/u],
  ["main 아키텍처 불일치", { wrongArchitecture: executable }, /Apple Silicon/u],
  ["sidecar 아키텍처 불일치", { wrongArchitecture: sidecarName }, /Apple Silicon/u],
  ["sidecar 실행권한 없음", { nonExecutable: true }, /실행 가능한/u],
  ["빌드 중 sidecar 교체", { changePreparedDuringApp: true }, /준비된 MCP sidecar가 변경/u],
  ["sidecar 빌드 중 source 변경", { changeSourceAfterSidecar: true }, /source commit 또는 파일/u],
  ["app 빌드 중 source 변경", { changeSourceAfterApp: true }, /source commit 또는 파일/u],
  ["확장 속성 정리 실패", { xattrFailure: true }, /app 확장 속성 정리 실패/u],
  ["sidecar ad-hoc 서명 실패", { sidecarSignFailure: true }, /MCP sidecar ad-hoc 서명 실패/u],
  ["app ad-hoc 서명 실패", { appSignFailure: true }, /app bundle ad-hoc 서명 실패/u],
  ["strict bundle 검증 실패", { signatureVerifyFailure: true }, /app bundle strict 서명 검증 실패/u],
]) {
  test(`${name}: DMG를 만들지 않고 실패한다`, (t) => {
    const f = fixture(t, options);
    f.seedPrevious();
    assert.throws(() => f.execute(), error);
    assert.ok(!f.calls.some(([command]) => command === "hdiutil"));
    assert.ok(!existsSync(f.dmgPath));
    assert.ok(!existsSync(`${f.dmgPath}.build.json`));
  });
}

for (const [name, options, error] of [
  ["DMG 생성 실패", { dmgFailure: true }, /hdiutil create 실패/u],
  ["DMG 검증 실패", { dmgVerifyFailure: true }, /hdiutil verify 실패/u],
  ["DMG 산출물 없음", { missingDmg: true }, /ENOENT/u],
  ["DMG 생성 중 app 변경", { changeAppDuringDmg: true }, /검증한 .app이 변경/u],
  ["DMG 생성 중 source 변경", { changeSourceAfterDmg: true }, /source commit 또는 파일/u],
  ["DMG attach 실패", { attachFailure: true }, /hdiutil attach 실패/u],
  ["DMG 내부 app 변경", { changeMountedApp: true }, /DMG 내부 .app이 검증한 .app과 일치/u],
  ["DMG 내부 strict 검증 실패", { mountedSignatureVerifyFailure: true }, /DMG 내부 app bundle strict 서명 검증 실패/u],
  ["DMG detach 실패", { detachFailure: true }, /hdiutil detach 실패/u],
]) {
  test(`${name}: 최종 DMG와 성공 영수증을 남기지 않는다`, (t) => {
    const f = fixture(t, options);
    assert.throws(() => f.execute(), error);
    assert.ok(!existsSync(f.dmgPath));
    assert.ok(!existsSync(`${f.dmgPath}.build.json`));
  });
}

test("macOS arm64 및 Rust host 검사를 산출물 이동보다 먼저 수행한다", (t) => {
  const f = fixture(t, { host: "x86_64-apple-darwin" });
  f.seedPrevious();
  assert.throws(() => f.execute({ platform: "linux" }), /Apple Silicon/u);
  assert.throws(() => f.execute({ arch: "x64" }), /Apple Silicon/u);
  assert.throws(() => f.execute(), /Rust host/u);
  assert.equal(readFileSync(f.dmgPath, "utf8"), "previous-dmg");
});

test("source의 package·Cargo·Tauri 버전 불일치와 sidecar 설정 누락을 거부한다", (t) => {
  const f = fixture(t);
  assert.deepEqual(readMacBuildMetadata(f.projectRoot), { appName, version, identifier, executable });
  json(f.metadataPath, { ...f.metadata, version: "0.2.22" });
  assert.throws(() => f.execute(), /버전이 일치하지/u);
  assert.equal(f.calls.length, 0);
  json(f.metadataPath, f.metadata);
  json(path.join(f.tauriRoot, "tauri.sidecar.conf.json"), { bundle: {} });
  assert.throws(() => readMacBuildMetadata(f.projectRoot), /sidecar bundle/u);
});

test("source 이름이 경로 밖을 가리키면 산출물 이동 전에 거부한다", (t) => {
  const f = fixture(t);
  json(f.metadataPath, { ...f.metadata, productName: "../../another-app" });
  assert.throws(() => f.execute(), /안전한 앱/u);
});

test("세 metadata에 같은 값이 있어도 경로로 사용할 수 있는 version은 거부한다", (t) => {
  const f = fixture(t);
  const invalidVersion = "../../outside";
  json(path.join(f.projectRoot, "package.json"), { version: invalidVersion });
  json(f.metadataPath, { ...f.metadata, version: invalidVersion });
  write(path.join(f.tauriRoot, "Cargo.toml"), `[package]\nname = "${executable}"\nversion = "${invalidVersion}"\n`);
  assert.throws(() => f.execute(), /버전이 일치하지/u);
});

test("spawn 실패와 signal 종료는 성공으로 처리하지 않는다", () => {
  assert.throws(() => requireCommandSuccess({ status: null, error: new Error("ENOENT") }, "build"), /build 실패.*ENOENT/u);
  assert.throws(() => requireCommandSuccess({ status: null, signal: "SIGTERM" }, "build"), /build 실패.*SIGTERM/u);
});

test("source 지문은 dirty 파일·새 파일을 포함하고, 잘못된 commit·불완전 목록을 거부한다", (t) => {
  const f = fixture(t);
  let commit = "a".repeat(40);
  let files = ["package.json", "src-tauri/Cargo.toml"];
  let dirty = "";
  const run = (_command, args) => ({ status: 0, stdout: args[0] === "rev-parse" ? commit : args[0] === "ls-files" ? `${files.join("\0")}\0` : dirty });
  const initial = captureMacBuildSource(f.projectRoot, run);
  json(path.join(f.projectRoot, "package.json"), { version, changed: true });
  dirty = " M package.json";
  const changed = captureMacBuildSource(f.projectRoot, run);
  assert.equal(changed.dirty, true);
  assert.notEqual(initial.sha256, changed.sha256);
  write(path.join(f.projectRoot, "new-source.ts"), "export const value = 1;");
  files.push("new-source.ts");
  assert.notEqual(captureMacBuildSource(f.projectRoot, run).sha256, changed.sha256);
  commit = "unknown";
  assert.throws(() => captureMacBuildSource(f.projectRoot, run), /source commit/u);
  commit = "b".repeat(40);
  files = ["package.json"];
  assert.throws(() => captureMacBuildSource(f.projectRoot, run), /목록이 불완전/u);
});

test("산출물 지문은 빈 파일과 root symlink를 거부하고 앱 내용·실행권한 변경을 감지한다", (t) => {
  const f = fixture(t);
  const file = path.join(f.projectRoot, "binary");
  write(file, "");
  assert.throws(() => digestPath(file), /빈 산출물/u);
  write(file, "binary");
  const link = path.join(f.projectRoot, "linked-binary");
  symlinkSync(file, link);
  assert.throws(() => digestPath(link), /일반 파일/u);
  f.execute();
  const initial = digestPath(f.appPath);
  chmodSync(path.join(f.appPath, "Contents/MacOS", executable), 0o644);
  assert.notEqual(digestPath(f.appPath), initial);
  symlinkSync(file, path.join(f.appPath, "Contents/external"));
  assert.throws(() => digestPath(f.appPath), /앱 밖을 가리키는/u);
});
