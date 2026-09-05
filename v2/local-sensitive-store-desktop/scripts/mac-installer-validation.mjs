import { createHash } from "node:crypto";
import { lstatSync, readFileSync, readdirSync, readlinkSync, realpathSync } from "node:fs";
import path from "node:path";

export function requireCommandSuccess(result, label) {
  if (result.error || result.status !== 0) {
    throw new Error(`${label} 실패${result.signal ? ` (${result.signal})` : ` (exit ${result.status ?? "unknown"})`}${result.error ? `: ${result.error.message}` : ""}`);
  }
  return String(result.stdout || "").trim();
}

export function digestPath(target) {
  const hash = createHash("sha256");
  const visit = (absolutePath, relativePath) => {
    const stat = lstatSync(absolutePath);
    if (stat.isFile()) {
      if (!relativePath) {
        if (stat.size === 0) throw new Error(`빈 산출물: ${absolutePath}`);
        hash.update(readFileSync(absolutePath));
      } else {
        hash.update(JSON.stringify([relativePath, "file", stat.mode & 0o777, stat.size]));
        hash.update(readFileSync(absolutePath));
      }
    } else if (stat.isDirectory()) {
      hash.update(JSON.stringify([relativePath, "directory", stat.mode & 0o777]));
      for (const name of readdirSync(absolutePath).sort()) visit(path.join(absolutePath, name), path.join(relativePath, name));
    } else if (stat.isSymbolicLink() && relativePath) {
      if (!realpathSync(absolutePath).startsWith(`${realpathSync(target)}${path.sep}`)) {
        throw new Error(`앱 밖을 가리키는 symlink는 패키징할 수 없습니다: ${relativePath}`);
      }
      hash.update(JSON.stringify([relativePath, "symlink", readlinkSync(absolutePath)]));
    } else {
      throw new Error(`일반 파일이나 앱 디렉터리가 아닙니다: ${absolutePath}`);
    }
  };
  visit(target, "");
  return hash.digest("hex");
}

export function captureMacBuildSource(projectRoot, run) {
  const options = { cwd: projectRoot, stdio: "pipe" };
  const commit = requireCommandSuccess(run("git", ["rev-parse", "HEAD"], options), "git HEAD");
  if (!/^[a-f0-9]{40,64}$/u.test(commit)) throw new Error("유효한 source commit을 확인하지 못했습니다.");
  const files = requireCommandSuccess(run("git", ["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "."], options), "git source files")
    .split("\0").filter(Boolean).sort();
  if (!files.includes("package.json") || !files.includes("src-tauri/Cargo.toml")) {
    throw new Error("Mac 빌드 source 목록이 불완전합니다.");
  }
  const hash = createHash("sha256");
  for (const file of files) {
    const absolutePath = path.resolve(projectRoot, file);
    if (!absolutePath.startsWith(`${path.resolve(projectRoot)}${path.sep}`) || !lstatSync(absolutePath).isFile()) {
      throw new Error(`빌드 source는 프로젝트 안의 일반 파일이어야 합니다: ${file}`);
    }
    const bytes = readFileSync(absolutePath);
    hash.update(JSON.stringify([file, bytes.length]));
    hash.update(bytes);
  }
  const dirty = Boolean(requireCommandSuccess(run("git", ["status", "--porcelain", "--untracked-files=normal", "--", "."], options), "git source status"));
  return { commit, sha256: hash.digest("hex"), dirty };
}

export function readMacBuildMetadata(projectRoot) {
  const pkg = JSON.parse(readFileSync(path.join(projectRoot, "package.json"), "utf8"));
  const config = JSON.parse(readFileSync(path.join(projectRoot, "src-tauri/tauri.conf.json"), "utf8"));
  const sidecarConfig = JSON.parse(readFileSync(path.join(projectRoot, "src-tauri/tauri.sidecar.conf.json"), "utf8"));
  const cargo = readFileSync(path.join(projectRoot, "src-tauri/Cargo.toml"), "utf8")
    .match(/\[package\]([\s\S]*?)(?=\n\[|$)/u)?.[1];
  const cargoVersion = cargo?.match(/^version\s*=\s*"([^"]+)"/mu)?.[1];
  const executable = config.mainBinaryName || cargo?.match(/^name\s*=\s*"([^"]+)"/mu)?.[1];
  if (typeof pkg.version !== "string" || !/^\d+\.\d+\.\d+(?:-[\w.-]+)?(?:\+[\w.-]+)?$/u.test(pkg.version)
    || pkg.version !== config.version || pkg.version !== cargoVersion) {
    throw new Error("package.json·Tauri·Cargo 버전이 일치하지 않습니다.");
  }
  for (const name of [config.productName, executable]) {
    if (!name || name === "." || name === ".." || /[/\\\0]/u.test(name)) throw new Error("안전한 앱/실행파일 이름을 확인하지 못했습니다.");
  }
  if (!config.identifier || !sidecarConfig.bundle?.externalBin?.includes("binaries/classaimate-student-record-mcp")) {
    throw new Error("앱 identifier 또는 MCP sidecar bundle 설정이 없습니다.");
  }
  return { appName: config.productName, version: pkg.version, identifier: config.identifier, executable };
}

export function verifyMacApp({ appPath, releaseDir, preparedSidecar, metadata, run }) {
  if (!lstatSync(appPath).isDirectory()) throw new Error("새 .app 번들이 없습니다.");
  const plist = JSON.parse(requireCommandSuccess(run("/usr/bin/plutil", ["-convert", "json", "-o", "-", path.join(appPath, "Contents/Info.plist")], { stdio: "pipe" }), "app Info.plist"));
  if (plist.CFBundleIdentifier !== metadata.identifier || plist.CFBundleShortVersionString !== metadata.version
    || plist.CFBundleVersion !== metadata.version || plist.CFBundleExecutable !== metadata.executable) {
    throw new Error("새 .app의 identifier·버전·실행파일이 현재 source metadata와 일치하지 않습니다.");
  }
  const result = {};
  for (const [name, expectedPath] of [[metadata.executable, path.join(releaseDir, metadata.executable)], ["classaimate-student-record-mcp", preparedSidecar]]) {
    const bundledPath = path.join(appPath, "Contents/MacOS", name);
    if (!lstatSync(bundledPath).isFile() || !(lstatSync(bundledPath).mode & 0o111)) throw new Error(`실행 가능한 번들 파일이 아닙니다: ${name}`);
    const architectures = requireCommandSuccess(run("/usr/bin/lipo", ["-archs", bundledPath], { stdio: "pipe" }), `architecture ${name}`);
    if (architectures !== "arm64") throw new Error(`Apple Silicon 전용 실행파일이 아닙니다: ${name} (${architectures})`);
    const sha256 = digestPath(bundledPath);
    if (sha256 !== digestPath(expectedPath)) throw new Error(`번들 실행파일이 현재 빌드와 일치하지 않습니다: ${name}`);
    result[name] = { sha256, architecture: architectures };
  }
  return result;
}
