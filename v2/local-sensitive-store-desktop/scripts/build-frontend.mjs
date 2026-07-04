import { spawnSync } from "node:child_process";

const nodeMajor = Number.parseInt(process.versions.node.split(".")[0] || "0", 10);
const isWindows = process.platform === "win32";
const command = nodeMajor > 22 ? (isWindows ? "npx.cmd" : "npx") : (isWindows ? "npm.cmd" : "npm");
const args = nodeMajor > 22
  ? ["-y", "-p", "node@22", "-p", "npm@10", "npm", "run", "build:frontend:raw"]
  : ["run", "build:frontend:raw"];

if (nodeMajor > 22) {
  console.log(`[build-frontend] Node ${process.versions.node} detected; using Node 22 for Vite compatibility.`);
}

const result = spawnSync(command, args, {
  cwd: process.cwd(),
  env: process.env,
  shell: isWindows,
  stdio: "inherit",
});

if (result.error) {
  console.error(result.error.message || result.error);
  process.exit(1);
}

process.exit(result.status ?? 1);
