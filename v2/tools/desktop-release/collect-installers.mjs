import { promises as fs } from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const workspaceRoot = path.resolve(__dirname, "..", "..");
const releasesRoot = path.join(workspaceRoot, "releases", "desktop-unified");
const latestDir = path.join(releasesRoot, "latest");
const archiveDir = path.join(releasesRoot, "archive", makeTimestamp(new Date()));

const shouldOpen = process.argv.includes("--open");
const onlyArgRaw = process.argv.find((arg) => arg.startsWith("--only=")) || "";
const onlySet = new Set(
  onlyArgRaw
    .slice("--only=".length)
    .split(",")
    .map((v) => String(v || "").trim())
    .filter(Boolean)
);

const appTargets = [
  {
    id: "desktop-shell",
    bundleRoot: path.join(workspaceRoot, "desktop-shell", "dist")
  },
  {
    id: "audio-splitter-desktop",
    bundleRoot: path.join(workspaceRoot, "audio-splitter-desktop", "src-tauri", "target", "release", "bundle")
  },
  {
    id: "audio-transcribe-desktop",
    bundleRoot: path.join(workspaceRoot, "audio-transcribe-desktop", "src-tauri", "target", "release", "bundle")
  },
  {
    id: "teacher-dashboard-desktop",
    bundleRoot: path.join(workspaceRoot, "teacher-dashboard-desktop", "src-tauri", "target", "release", "bundle")
  },
  {
    id: "local-sensitive-store-desktop",
    bundleRoot: path.join(workspaceRoot, "local-sensitive-store-desktop", "src-tauri", "target", "release", "bundle")
  }
];

const allowedExtensions = new Set([
  ".msi",
  ".exe",
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

function sanitizeName(raw) {
  return String(raw || "").replace(/[\\/:*?"<>|]/g, "_");
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

      if (!entry.isFile()) continue;

      const ext = path.extname(entry.name).toLowerCase();
      if (!allowedExtensions.has(ext)) continue;

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
  const latestByType = new Map();
  for (const artifact of sorted) {
    if (!latestByType.has(artifact.type)) {
      latestByType.set(artifact.type, artifact);
    }
  }
  return [...latestByType.values()].sort((a, b) => a.type.localeCompare(b.type));
}

async function copySelection(selection, destinationDir) {
  for (const item of selection) {
    const prefix = sanitizeName(item.appId);
    const fileName = sanitizeName(item.artifact.name);
    const targetPath = path.join(destinationDir, `${prefix}__${fileName}`);
    await fs.cp(item.artifact.fullPath, targetPath, {
      recursive: item.artifact.isDirectory,
      force: true
    });
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

async function collectAppArtifacts(app) {
  const selected = [];
  const stat = await fs.stat(app.bundleRoot).catch(() => null);
  if (!stat || !stat.isDirectory()) {
    return {
      appId: app.id,
      bundleRoot: app.bundleRoot,
      status: "missing_source",
      selected
    };
  }

  const artifacts = await listArtifacts(app.bundleRoot);
  const latestArtifacts = selectLatestByType(artifacts);

  for (const artifact of latestArtifacts) {
    selected.push({
      appId: app.id,
      bundleRoot: app.bundleRoot,
      artifact
    });
  }

  return {
    appId: app.id,
    bundleRoot: app.bundleRoot,
    status: selected.length ? "ok" : "no_artifacts",
    selected
  };
}

async function main() {
  const targets = onlySet.size
    ? appTargets.filter((app) => onlySet.has(app.id))
    : appTargets;

  if (!targets.length) {
    console.error("[release] No matching app target. Use --only with valid ids.");
    console.error(`[release] Valid ids: ${appTargets.map((app) => app.id).join(", ")}`);
    process.exit(1);
  }

  const summary = [];
  const selection = [];
  for (const app of targets) {
    const result = await collectAppArtifacts(app);
    summary.push(result);
    selection.push(...result.selected);
  }

  if (!selection.length) {
    console.error("[release] No installer artifacts found for selected app targets.");
    for (const row of summary) {
      console.error(`  - ${row.appId}: ${row.status} (${row.bundleRoot})`);
    }
    process.exit(1);
  }

  await fs.mkdir(archiveDir, { recursive: true });
  await fs.rm(latestDir, { recursive: true, force: true });
  await fs.mkdir(latestDir, { recursive: true });

  await copySelection(selection, archiveDir);
  await copySelection(selection, latestDir);

  const manifest = {
    generatedAt: new Date().toISOString(),
    scope: "desktop-unified",
    selectedApps: targets.map((app) => app.id),
    latestDir,
    archiveDir,
    summary: summary.map((row) => ({
      appId: row.appId,
      status: row.status,
      bundleRoot: row.bundleRoot,
      selectedCount: row.selected.length
    })),
    artifacts: selection.map((item) => ({
      appId: item.appId,
      type: item.artifact.type,
      name: item.artifact.name,
      source: item.artifact.fullPath,
      mtime: new Date(item.artifact.mtimeMs).toISOString(),
      copiedName: `${sanitizeName(item.appId)}__${sanitizeName(item.artifact.name)}`
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

  console.log(`[release] unified latest: ${latestDir}`);
  console.log(`[release] unified archive: ${archiveDir}`);
  for (const row of summary) {
    console.log(`  - ${row.appId}: ${row.status} (${row.selected.length})`);
  }
  for (const item of selection) {
    console.log(`    * ${item.appId} ${item.artifact.type} -> ${item.artifact.name}`);
  }

  if (shouldOpen) {
    await openFolder(latestDir);
  }
}

await main();
