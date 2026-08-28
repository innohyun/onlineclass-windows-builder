import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(scriptDir, "..");
const tauriRoot = path.join(projectRoot, "src-tauri");
const binaryName = "classaimate-student-record-mcp";
const executableSuffix = process.platform === "win32" ? ".exe" : "";

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd || projectRoot,
    env: process.env,
    encoding: "utf8",
    stdio: options.stdio || "pipe",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = [result.stdout, result.stderr].filter(Boolean).join("\n").trim();
    throw new Error(`${command} ${args.join(" ")} failed${detail ? `:\n${detail}` : ""}`);
  }
  return String(result.stdout || "").trim();
}

const rustc = run("rustc", ["-vV"]);
const host = rustc.split(/\r?\n/u).find((line) => line.startsWith("host: "))?.slice(6).trim();
if (!host || !/^[a-z0-9_.-]+$/u.test(host)) {
  throw new Error("Rust host target을 확인하지 못했습니다.");
}

run("cargo", ["build", "--release", "--bin", binaryName], { cwd: tauriRoot, stdio: "inherit" });
const source = path.join(tauriRoot, "target", "release", `${binaryName}${executableSuffix}`);
if (!existsSync(source)) throw new Error(`MCP sidecar binary를 찾지 못했습니다: ${source}`);

const binariesDir = path.join(tauriRoot, "binaries");
const target = path.join(binariesDir, `${binaryName}-${host}${executableSuffix}`);
mkdirSync(binariesDir, { recursive: true });
copyFileSync(source, target);
console.log(`[student-record-mcp-sidecar] ${target}`);
