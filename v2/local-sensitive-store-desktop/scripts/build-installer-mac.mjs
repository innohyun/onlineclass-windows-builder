import { existsSync, mkdirSync, rmSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import pkg from "../package.json" with { type: "json" };

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, "..");
const tauriRoot = path.join(projectRoot, "src-tauri");
const appName = "ClassAiMate 교사 데스크";
const arch = process.arch === "arm64" ? "aarch64" : process.arch;
const appPath = path.join(tauriRoot, "target", "release", "bundle", "macos", `${appName}.app`);
const dmgDir = path.join(tauriRoot, "target", "release", "bundle", "dmg");
const dmgPath = path.join(dmgDir, `${appName}_${pkg.version}_${arch}.dmg`);

function run(command, args, options = {}) {
  return spawnSync(command, args, {
    cwd: options.cwd || projectRoot,
    env: process.env,
    encoding: "utf8",
    stdio: options.stdio || "inherit"
  });
}

if (process.platform !== "darwin") {
  console.error("[build-installer-mac] macOS DMG builds must run on macOS.");
  process.exit(1);
}

const npxCommand = process.platform === "win32" ? "npx.cmd" : "npx";
const tauriResult = run(npxCommand, ["tauri", "build", "--bundles", "dmg"]);
if (tauriResult.status === 0) {
  process.exit(0);
}

if (!existsSync(appPath)) {
  console.error("[build-installer-mac] Tauri DMG build failed and no .app bundle was produced.");
  process.exit(tauriResult.status || 1);
}

mkdirSync(dmgDir, { recursive: true });
rmSync(dmgPath, { force: true });

console.warn("[build-installer-mac] Tauri DMG script failed; creating a plain unsigned DMG with hdiutil.");
const hdiutilResult = run("hdiutil", [
  "create",
  "-volname",
  appName,
  "-srcfolder",
  appPath,
  "-ov",
  "-format",
  "UDZO",
  dmgPath
]);

if (hdiutilResult.status !== 0) {
  process.exit(hdiutilResult.status || 1);
}
