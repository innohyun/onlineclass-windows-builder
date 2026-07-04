import { promises as fs } from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, "..");
const workspaceRoot = path.resolve(projectRoot, "..");
const bundleRoot = path.join(projectRoot, "dist");
const releasesRoot = path.join(workspaceRoot, "releases", "desktop-shell");
const latestDir = path.join(releasesRoot, "latest");
const archiveDir = path.join(releasesRoot, "archive", makeTimestamp(new Date()));
const shouldOpen = process.argv.includes("--open");

const allowedExtensions = new Set([
  ".exe",
  ".msi",
  ".zip",
  ".dmg",
  ".pkg",
  ".deb",
  ".rpm",
  ".appimage"
]);

function makeTimestamp(date) {
  const pad = (n) => String(n).padStart(2, "0");
  return [
    date.getFullYear(),
    pad(date.getMonth() + 1),
    pad(date.getDate())
  ].join("") + "-" + [pad(date.getHours()), pad(date.getMinutes()), pad(date.getSeconds())].join("");
}

async function listArtifacts(rootDir) {
  const items = [];

  async function walk(currentDir) {
    const entries = await fs.readdir(currentDir, { withFileTypes: true });
    for (const entry of entries) {
      const fullPath = path.join(currentDir, entry.name);

      if (entry.isDirectory()) {
        if (entry.name.toLowerCase().endsWith(".app")) {
          const stat = await fs.stat(fullPath);
          items.push({
            type: ".app",
            fullPath,
            name: entry.name,
            mtimeMs: stat.mtimeMs,
            isDirectory: true
          });
          continue;
        }
        await walk(fullPath);
        continue;
      }

      if (!entry.isFile()) {
        continue;
      }

      const ext = path.extname(entry.name).toLowerCase();
      if (!allowedExtensions.has(ext)) {
        continue;
      }

      const stat = await fs.stat(fullPath);
      items.push({
        type: ext,
        fullPath,
        name: entry.name,
        mtimeMs: stat.mtimeMs,
        isDirectory: false
      });
    }
  }

  await walk(rootDir);
  return items;
}

function selectLatestByType(artifacts) {
  const sorted = [...artifacts].sort((a, b) => b.mtimeMs - a.mtimeMs);
  const latest = new Map();
  for (const artifact of sorted) {
    if (!latest.has(artifact.type)) {
      latest.set(artifact.type, artifact);
    }
  }
  return [...latest.values()].sort((a, b) => a.type.localeCompare(b.type));
}

async function copyArtifacts(artifacts, destinationDir) {
  for (const artifact of artifacts) {
    const targetPath = path.join(destinationDir, artifact.name);
    await fs.cp(artifact.fullPath, targetPath, { recursive: artifact.isDirectory, force: true });
  }
}

async function openFolder(targetPath) {
  let cmd = null;
  let args = [];

  if (process.platform === "win32") {
    cmd = "cmd";
    args = ["/c", "start", "", targetPath];
  } else if (process.platform === "darwin") {
    cmd = "open";
    args = [targetPath];
  } else {
    cmd = "xdg-open";
    args = [targetPath];
  }

  const child = spawn(cmd, args, {
    detached: true,
    stdio: "ignore"
  });
  child.unref();
}

async function main() {
  try {
    const stat = await fs.stat(bundleRoot).catch(() => null);
    if (!stat || !stat.isDirectory()) {
      console.error(`[release] Build output directory not found: ${bundleRoot}`);
      console.error("[release] Run a build first: npm run build:installer");
      process.exit(1);
    }

    const artifacts = await listArtifacts(bundleRoot);
    const selected = selectLatestByType(artifacts);

    if (!selected.length) {
      console.error(`[release] No installer artifacts found under: ${bundleRoot}`);
      process.exit(1);
    }

    await fs.mkdir(archiveDir, { recursive: true });
    await fs.rm(latestDir, { recursive: true, force: true });
    await fs.mkdir(latestDir, { recursive: true });

    await copyArtifacts(selected, archiveDir);
    await copyArtifacts(selected, latestDir);

    const manifest = {
      generatedAt: new Date().toISOString(),
      source: bundleRoot,
      latestDir,
      archiveDir,
      artifacts: selected.map((artifact) => ({
        type: artifact.type,
        name: artifact.name,
        source: artifact.fullPath,
        mtime: new Date(artifact.mtimeMs).toISOString()
      }))
    };

    await fs.writeFile(
      path.join(latestDir, "release-manifest.json"),
      JSON.stringify(manifest, null, 2),
      "utf8"
    );
    await fs.writeFile(
      path.join(archiveDir, "release-manifest.json"),
      JSON.stringify(manifest, null, 2),
      "utf8"
    );

    console.log(`[release] latest: ${latestDir}`);
    console.log(`[release] archive: ${archiveDir}`);
    for (const artifact of selected) {
      console.log(`  - ${artifact.type} -> ${artifact.name}`);
    }

    if (shouldOpen) {
      await openFolder(latestDir);
    }
  } catch (error) {
    console.error("[release] Failed to collect artifacts");
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}

await main();
