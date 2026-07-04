const fs = require("node:fs");
const http = require("node:http");
const net = require("node:net");
const path = require("node:path");
const { spawn } = require("node:child_process");
const {
  deriveLocalWorkspaceBootstrapPlan,
  LOCAL_COLLAB_PREVIEW_PORT
} = require("./module-url");

const DEFAULT_HOST = "127.0.0.1";
const DEFAULT_EMULATOR_PORTS = [9099, 8080, 5002, 9199];
const COLLAB_PREVIEW_HEALTH_PATH = "/__collab/health";
const COLLAB_PREVIEW_TIMEOUT_MS = 30000;
const STATIC_SERVER_TIMEOUT_MS = 10000;
const EMULATOR_TIMEOUT_MS = 90000;
const PORT_PROBE_INTERVAL_MS = 500;
const MIME_TYPES = {
  ".css": "text/css; charset=utf-8",
  ".gif": "image/gif",
  ".html": "text/html; charset=utf-8",
  ".ico": "image/x-icon",
  ".jpeg": "image/jpeg",
  ".jpg": "image/jpeg",
  ".js": "application/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".mjs": "application/javascript; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".wasm": "application/wasm",
  ".webp": "image/webp"
};

function createNoopLogger() {
  return {
    info() {},
    warn() {},
    error() {}
  };
}

function normalizeDir(rawPath) {
  const value = String(rawPath || "").trim();
  if (!value) return "";
  try {
    return path.resolve(value);
  } catch (_error) {
    return "";
  }
}

function fileExists(filePath) {
  try {
    return fs.existsSync(filePath);
  } catch (_error) {
    return false;
  }
}

function listAncestorDirs(startPath) {
  const first = normalizeDir(startPath);
  if (!first) return [];
  const out = [];
  let current = first;
  while (current) {
    out.push(current);
    const parent = path.dirname(current);
    if (!parent || parent === current) break;
    current = parent;
  }
  return out;
}

function probeWorkspaceRoots(candidateDir) {
  const dir = normalizeDir(candidateDir);
  if (!dir) return null;

  const repoRoot = dir;
  const repoV2Root = path.join(repoRoot, "v2");
  if (
    fileExists(path.join(repoRoot, "index.html")) &&
    fileExists(path.join(repoV2Root, "firebase.json")) &&
    fileExists(path.join(repoV2Root, "package.json"))
  ) {
    return {
      workspaceRoot: repoRoot,
      v2Root: repoV2Root,
      sourceDir: dir
    };
  }

  const v2Root = dir;
  const workspaceRoot = path.dirname(v2Root);
  if (
    fileExists(path.join(v2Root, "firebase.json")) &&
    fileExists(path.join(v2Root, "package.json")) &&
    fileExists(path.join(v2Root, "desktop-shell", "package.json")) &&
    fileExists(path.join(workspaceRoot, "index.html"))
  ) {
    return {
      workspaceRoot,
      v2Root,
      sourceDir: dir
    };
  }

  return null;
}

function findWorkspaceRoots(options = {}) {
  const env = options.env && typeof options.env === "object" ? options.env : process.env;
  const rawCandidates = [
    env.ONLINECLASS_WORKSPACE_ROOT,
    env.ONLINECLASS_V2_ROOT,
    ...(Array.isArray(options.candidates) ? options.candidates : []),
    __dirname
  ];
  const seen = new Set();

  for (const rawCandidate of rawCandidates) {
    const ancestors = listAncestorDirs(rawCandidate);
    for (const candidate of ancestors) {
      const normalized = normalizeDir(candidate);
      if (!normalized || seen.has(normalized)) continue;
      seen.add(normalized);
      const found = probeWorkspaceRoots(normalized);
      if (found) {
        return found;
      }
    }
  }

  return null;
}

function isPortReady(port, host = DEFAULT_HOST, timeoutMs = 400) {
  return new Promise((resolve) => {
    const socket = new net.Socket();
    let settled = false;

    const finish = (value) => {
      if (settled) return;
      settled = true;
      try {
        socket.destroy();
      } catch (_error) {
        // noop
      }
      resolve(value);
    };

    socket.setTimeout(timeoutMs);
    socket.once("connect", () => finish(true));
    socket.once("timeout", () => finish(false));
    socket.once("error", () => finish(false));

    try {
      socket.connect(port, host);
    } catch (_error) {
      finish(false);
    }
  });
}

async function arePortsReady(ports, host = DEFAULT_HOST) {
  const checks = await Promise.all(ports.map((port) => isPortReady(port, host)));
  return checks.every(Boolean);
}

async function waitForPort(port, options = {}) {
  const host = options.host || DEFAULT_HOST;
  const timeoutMs = Number(options.timeoutMs || STATIC_SERVER_TIMEOUT_MS);
  const intervalMs = Number(options.intervalMs || PORT_PROBE_INTERVAL_MS);
  const startedAt = Date.now();

  while ((Date.now() - startedAt) <= timeoutMs) {
    if (await isPortReady(port, host)) {
      return true;
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }

  throw new Error(`포트 ${host}:${port} 준비 대기 시간 초과`);
}

async function waitForPorts(ports, options = {}) {
  const host = options.host || DEFAULT_HOST;
  const timeoutMs = Number(options.timeoutMs || EMULATOR_TIMEOUT_MS);
  const intervalMs = Number(options.intervalMs || PORT_PROBE_INTERVAL_MS);
  const startedAt = Date.now();

  while ((Date.now() - startedAt) <= timeoutMs) {
    if (await arePortsReady(ports, host)) {
      return true;
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }

  throw new Error(`포트 준비 대기 시간 초과: ${ports.join(", ")}`);
}

function httpGetJson(url, options = {}) {
  const timeoutMs = Number(options.timeoutMs || 2000);
  return new Promise((resolve, reject) => {
    const req = http.get(url, { timeout: timeoutMs }, (res) => {
      let raw = "";
      res.setEncoding("utf8");
      res.on("data", (chunk) => {
        raw += chunk;
      });
      res.on("end", () => {
        let payload = {};
        try {
          payload = raw ? JSON.parse(raw) : {};
        } catch (_error) {
          payload = {};
        }
        resolve({
          statusCode: Number(res.statusCode || 0),
          payload
        });
      });
    });

    req.once("timeout", () => {
      req.destroy(new Error("timeout"));
    });
    req.once("error", reject);
  });
}

async function probeCollabPreviewHealth(options = {}) {
  const host = options.host || DEFAULT_HOST;
  const port = Number(options.port || LOCAL_COLLAB_PREVIEW_PORT);
  const url = `http://${host}:${port}${COLLAB_PREVIEW_HEALTH_PATH}`;
  try {
    const result = await httpGetJson(url, { timeoutMs: options.timeoutMs || 2000 });
    if (
      result.statusCode === 200 &&
      result.payload?.ok === true &&
      result.payload?.service === "collab-bootstrap"
    ) {
      return result.payload;
    }
  } catch (_error) {
    // noop
  }
  return null;
}

async function waitForCollabPreview(options = {}) {
  const host = options.host || DEFAULT_HOST;
  const port = Number(options.port || LOCAL_COLLAB_PREVIEW_PORT);
  const timeoutMs = Number(options.timeoutMs || COLLAB_PREVIEW_TIMEOUT_MS);
  const intervalMs = Number(options.intervalMs || PORT_PROBE_INTERVAL_MS);
  const startedAt = Date.now();

  while ((Date.now() - startedAt) <= timeoutMs) {
    const payload = await probeCollabPreviewHealth({
      host,
      port,
      timeoutMs: Math.min(intervalMs, 2000)
    });
    if (payload) return payload;
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }

  throw new Error(`collab preview health timeout: http://${host}:${port}${COLLAB_PREVIEW_HEALTH_PATH}`);
}

function createStaticFileServer(rootDir, logger) {
  return http.createServer((req, res) => {
    try {
      const urlPath = decodeURIComponent(String(req.url || "/").split("?")[0] || "/");
      const requestPath = urlPath === "/" ? "/index.html" : urlPath;
      const normalized = path.normalize(requestPath).replace(/^(\.\.[/\\])+/, "");
      const targetPath = path.join(rootDir, normalized);
      const relative = path.relative(rootDir, targetPath);

      if (relative.startsWith("..") || path.isAbsolute(relative)) {
        res.writeHead(403, { "Content-Type": "text/plain; charset=utf-8" });
        res.end("forbidden");
        return;
      }

      let filePath = targetPath;
      if (fileExists(filePath) && fs.statSync(filePath).isDirectory()) {
        filePath = path.join(filePath, "index.html");
      }

      if (!fileExists(filePath)) {
        res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
        res.end("not found");
        return;
      }

      const extension = path.extname(filePath).toLowerCase();
      const contentType = MIME_TYPES[extension] || "application/octet-stream";
      res.writeHead(200, { "Content-Type": contentType });
      fs.createReadStream(filePath).pipe(res);
    } catch (error) {
      logger.warn("local static server request failed", String(error));
      res.writeHead(500, { "Content-Type": "text/plain; charset=utf-8" });
      res.end("internal server error");
    }
  });
}

function ensureLogDir(logDir) {
  fs.mkdirSync(logDir, { recursive: true });
}

function buildFirebaseCommand(v2Root) {
  const safeRoot = String(v2Root || "").replace(/"/g, '""');
  return `cd /d "${safeRoot}" && node_modules\\.bin\\firebase.cmd emulators:start --only auth,firestore,storage,hosting`;
}

function startLoggedProcess(command, args, options = {}) {
  const logDir = options.logDir;
  ensureLogDir(logDir);
  const stdoutPath = path.join(logDir, options.stdoutFileName);
  const stderrPath = path.join(logDir, options.stderrFileName);
  const stdout = fs.createWriteStream(stdoutPath, { flags: "a" });
  const stderr = fs.createWriteStream(stderrPath, { flags: "a" });
  const child = spawn(command, args, {
    cwd: options.cwd,
    windowsHide: true,
    stdio: ["ignore", "pipe", "pipe"],
    env: options.env && typeof options.env === "object" ? options.env : process.env
  });

  child.stdout.pipe(stdout);
  child.stderr.pipe(stderr);

  child.once("exit", (code, signal) => {
    options.logger.warn(options.exitLogMessage, { code, signal });
    stdout.end();
    stderr.end();
  });

  return {
    child,
    stdoutPath,
    stderrPath
  };
}

function startFirebaseProcess(v2Root, logDir, logger) {
  return startLoggedProcess("cmd.exe", ["/d", "/s", "/c", buildFirebaseCommand(v2Root)], {
    cwd: v2Root,
    env: process.env,
    logDir,
    logger,
    stdoutFileName: "firebase-emulators.out.log",
    stderrFileName: "firebase-emulators.err.log",
    exitLogMessage: "local firebase process exited"
  });
}

function startCollabPreviewProcess(v2Root, logDir, logger) {
  const scriptPath = path.join(v2Root, "tools", "start-collab-preview-stack.mjs");
  if (!fileExists(scriptPath)) {
    throw new Error(`collab preview script not found: ${scriptPath}`);
  }

  const env = {
    ...process.env,
    COLLAB_STATIC_PORT: String(process.env.COLLAB_STATIC_PORT || 5000),
    COLLAB_FUNCTION_PORT: String(process.env.COLLAB_FUNCTION_PORT || LOCAL_COLLAB_PREVIEW_PORT)
  };
  if (process.versions && process.versions.electron) {
    env.ELECTRON_RUN_AS_NODE = "1";
  }

  return startLoggedProcess(process.execPath, [scriptPath], {
    cwd: v2Root,
    env,
    logDir,
    logger,
    stdoutFileName: "collab-preview.out.log",
    stderrFileName: "collab-preview.err.log",
    exitLogMessage: "local collab preview process exited"
  });
}

async function killProcessTree(pid, logger) {
  const safePid = Number(pid);
  if (!Number.isInteger(safePid) || safePid <= 0) return;

  if (process.platform === "win32") {
    await new Promise((resolve) => {
      const killer = spawn("taskkill", ["/pid", String(safePid), "/T", "/F"], {
        windowsHide: true,
        stdio: "ignore"
      });
      killer.once("exit", () => resolve());
      killer.once("error", (error) => {
        logger.warn("taskkill failed", String(error));
        resolve();
      });
    });
    return;
  }

  try {
    process.kill(safePid, "SIGTERM");
  } catch (error) {
    logger.warn("failed to kill local firebase process", String(error));
  }
}

function createLocalWorkspaceRuntime(options = {}) {
  const logger = options.logger || createNoopLogger();
  const getCandidates =
    typeof options.getCandidates === "function"
      ? options.getCandidates
      : () => Array.isArray(options.candidates) ? options.candidates : [];
  const getLogDir =
    typeof options.getLogDir === "function"
      ? options.getLogDir
      : () => path.join(process.cwd(), "tmp", "desktop-shell-local-runtime");

  const state = {
    ensurePromise: null,
    lastReady: null,
    firebaseProcess: null,
    firebaseLogPaths: null,
    collabPreviewProcess: null,
    collabPreviewLogPaths: null,
    staticServer: null,
    staticServerPort: null
  };

  async function ensureReady(rawBaseUrl) {
    const plan = deriveLocalWorkspaceBootstrapPlan(rawBaseUrl);
    if (!plan.enabled) {
      const skipped = {
        enabled: false,
        autoUseEmulator: false,
        reason: plan.reason,
        plan
      };
      state.lastReady = skipped;
      return skipped;
    }

    if (state.ensurePromise) {
      return state.ensurePromise;
    }

    state.ensurePromise = (async () => {
      const roots = findWorkspaceRoots({
        env: process.env,
        candidates: getCandidates()
      });
      if (!roots) {
        throw new Error("desktop-shell 로컬 워크스페이스를 찾지 못했습니다.");
      }

      const logDir = getLogDir();
      ensureLogDir(logDir);

      const result = {
        enabled: true,
        autoUseEmulator: true,
        reason: plan.reason,
        plan: {
          ...plan,
          emulatorPorts: [...DEFAULT_EMULATOR_PORTS]
        },
        roots,
        logDir,
        webServer: {
          owned: false,
          started: false,
          ready: false,
          port: plan.webPort
        },
        emulators: {
          owned: false,
          started: false,
          ready: false,
          ports: [...DEFAULT_EMULATOR_PORTS],
          stdoutPath: "",
          stderrPath: ""
        },
        collabPreview: {
          owned: false,
          started: false,
          ready: false,
          port: plan.collabPreviewPort || LOCAL_COLLAB_PREVIEW_PORT,
          healthUrl: `http://${DEFAULT_HOST}:${plan.collabPreviewPort || LOCAL_COLLAB_PREVIEW_PORT}${COLLAB_PREVIEW_HEALTH_PATH}`,
          stdoutPath: "",
          stderrPath: "",
          configReady: false,
          configIssues: []
        }
      };

      const webAlreadyReady = await isPortReady(plan.webPort, DEFAULT_HOST);
      if (!webAlreadyReady && plan.needsStaticServer) {
        if (!state.staticServer || state.staticServer.listening !== true || state.staticServerPort !== plan.webPort) {
          state.staticServer = createStaticFileServer(roots.workspaceRoot, logger);
          await new Promise((resolve, reject) => {
            state.staticServer.once("error", reject);
            state.staticServer.listen(plan.webPort, DEFAULT_HOST, () => {
              state.staticServer.off("error", reject);
              resolve();
            });
          });
          state.staticServerPort = plan.webPort;
          result.webServer.owned = true;
          result.webServer.started = true;
          logger.info("started local static server", {
            root: roots.workspaceRoot,
            port: plan.webPort
          });
        }
      }

      const emulatorsAlreadyReady = await arePortsReady(DEFAULT_EMULATOR_PORTS, DEFAULT_HOST);
      if (!emulatorsAlreadyReady) {
        if (!state.firebaseProcess || state.firebaseProcess.exitCode !== null) {
          const proc = startFirebaseProcess(roots.v2Root, logDir, logger);
          state.firebaseProcess = proc.child;
          state.firebaseLogPaths = {
            stdoutPath: proc.stdoutPath,
            stderrPath: proc.stderrPath
          };
          result.emulators.owned = true;
          result.emulators.started = true;
          result.emulators.stdoutPath = proc.stdoutPath;
          result.emulators.stderrPath = proc.stderrPath;
          logger.info("started local firebase emulators", {
            v2Root: roots.v2Root,
            stdoutPath: proc.stdoutPath,
            stderrPath: proc.stderrPath
          });
        } else {
          result.emulators.owned = true;
        }
      } else if (state.firebaseLogPaths) {
        result.emulators.stdoutPath = state.firebaseLogPaths.stdoutPath;
        result.emulators.stderrPath = state.firebaseLogPaths.stderrPath;
      }

      if (plan.needsStaticServer) {
        await waitForPort(plan.webPort, { host: DEFAULT_HOST, timeoutMs: STATIC_SERVER_TIMEOUT_MS });
      }
      result.webServer.ready = true;

      await waitForPorts(DEFAULT_EMULATOR_PORTS, { host: DEFAULT_HOST, timeoutMs: EMULATOR_TIMEOUT_MS });
      result.emulators.ready = true;

      if (!result.emulators.stdoutPath && state.firebaseLogPaths) {
        result.emulators.stdoutPath = state.firebaseLogPaths.stdoutPath;
        result.emulators.stderrPath = state.firebaseLogPaths.stderrPath;
      }

      const collabPort = result.collabPreview.port;
      let collabPayload = plan.needsCollabPreview
        ? await probeCollabPreviewHealth({ host: DEFAULT_HOST, port: collabPort, timeoutMs: 1500 })
        : null;

      if (plan.needsCollabPreview && !collabPayload) {
        if (!state.collabPreviewProcess || state.collabPreviewProcess.exitCode !== null) {
          const proc = startCollabPreviewProcess(roots.v2Root, logDir, logger);
          state.collabPreviewProcess = proc.child;
          state.collabPreviewLogPaths = {
            stdoutPath: proc.stdoutPath,
            stderrPath: proc.stderrPath
          };
          result.collabPreview.owned = true;
          result.collabPreview.started = true;
          result.collabPreview.stdoutPath = proc.stdoutPath;
          result.collabPreview.stderrPath = proc.stderrPath;
          logger.info("started local collab preview", {
            v2Root: roots.v2Root,
            stdoutPath: proc.stdoutPath,
            stderrPath: proc.stderrPath,
            healthUrl: result.collabPreview.healthUrl
          });
        } else {
          result.collabPreview.owned = true;
        }
      } else if (state.collabPreviewLogPaths) {
        result.collabPreview.stdoutPath = state.collabPreviewLogPaths.stdoutPath;
        result.collabPreview.stderrPath = state.collabPreviewLogPaths.stderrPath;
      }

      if (plan.needsCollabPreview) {
        collabPayload = collabPayload || await waitForCollabPreview({
          host: DEFAULT_HOST,
          port: collabPort,
          timeoutMs: COLLAB_PREVIEW_TIMEOUT_MS
        });
        result.collabPreview.ready = true;
        result.collabPreview.configReady = collabPayload?.ready === true;
        result.collabPreview.configIssues = Array.isArray(collabPayload?.config?.issues)
          ? collabPayload.config.issues.slice()
          : [];
        if (!result.collabPreview.stdoutPath && state.collabPreviewLogPaths) {
          result.collabPreview.stdoutPath = state.collabPreviewLogPaths.stdoutPath;
          result.collabPreview.stderrPath = state.collabPreviewLogPaths.stderrPath;
        }
      }

      state.lastReady = result;
      return result;
    })();

    try {
      return await state.ensurePromise;
    } finally {
      state.ensurePromise = null;
    }
  }

  async function shutdown() {
    if (state.staticServer) {
      await new Promise((resolve) => {
        try {
          state.staticServer.close(() => resolve());
        } catch (_error) {
          resolve();
        }
      });
      state.staticServer = null;
      state.staticServerPort = null;
    }

    if (state.firebaseProcess && state.firebaseProcess.pid) {
      await killProcessTree(state.firebaseProcess.pid, logger);
      state.firebaseProcess = null;
      state.firebaseLogPaths = null;
    }

    if (state.collabPreviewProcess && state.collabPreviewProcess.pid) {
      await killProcessTree(state.collabPreviewProcess.pid, logger);
      state.collabPreviewProcess = null;
      state.collabPreviewLogPaths = null;
    }
  }

  return {
    ensureReady,
    findWorkspaceRoots,
    getStatus: () => state.lastReady,
    shutdown
  };
}

module.exports = {
  DEFAULT_EMULATOR_PORTS,
  createLocalWorkspaceRuntime,
  deriveLocalWorkspaceBootstrapPlan,
  findWorkspaceRoots
};
