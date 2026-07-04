const fs = require("node:fs");
const http = require("node:http");
const path = require("node:path");
const { spawn } = require("node:child_process");
const { findWorkspaceRoots } = require("./local-workspace-runtime");
const { normalizeBaseUrl, parseUrlSafe } = require("./module-url");

const DEFAULT_HOST = "127.0.0.1";
const LOCAL_STORE_PORTS = Object.freeze([51273, 51274, 51275, 51276, 51277]);
const PAIRING_KEY_RELATIVE_PATH = path.join("OnlineClass", "local-sensitive-store", "pairing-key.txt");
const REQUEST_TIMEOUT_MS = 1200;
const READY_TIMEOUT_MS = 7000;
const READY_INTERVAL_MS = 350;

let ensurePromise = null;
let lastStatus = {
  ok: false,
  status: "idle",
  endpoint: "",
  helperPath: "",
  error: ""
};

function noopLogger() {
  return {
    info() {},
    warn() {},
    error() {}
  };
}

function normalize(value, maxLength = 0) {
  const text = String(value || "").trim();
  return maxLength > 0 ? text.slice(0, maxLength) : text;
}

function getAppDataDir(env = process.env) {
  const explicit = normalize(env.APPDATA);
  if (explicit) return explicit;
  const userProfile = normalize(env.USERPROFILE);
  return userProfile ? path.join(userProfile, "AppData", "Roaming") : "";
}

function getPairingKeyPath(options = {}) {
  const explicitDir = normalize(options.env?.ONLINECLASS_LOCAL_STORE_DIR || process.env.ONLINECLASS_LOCAL_STORE_DIR);
  if (explicitDir) return path.join(explicitDir, "pairing-key.txt");
  const appData = getAppDataDir(options.env || process.env);
  return appData ? path.join(appData, PAIRING_KEY_RELATIVE_PATH) : "";
}

function readPairingKey(options = {}) {
  const keyPath = getPairingKeyPath(options);
  if (!keyPath) return "";
  try {
    return normalize(fs.readFileSync(keyPath, "utf8"), 240);
  } catch (_error) {
    return "";
  }
}

function endpointForPort(port) {
  return `http://${DEFAULT_HOST}:${port}`;
}

function httpGetJson(url, options = {}) {
  const timeoutMs = Number(options.timeoutMs || REQUEST_TIMEOUT_MS);
  const headers = options.headers || {};
  return new Promise((resolve, reject) => {
    const req = http.get(url, { headers, timeout: timeoutMs }, (res) => {
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

async function probeEndpoint(endpoint, pairingKey) {
  const result = await httpGetJson(`${endpoint}/v1/health`, {
    headers: pairingKey ? { "X-OnlineClass-Local-Store-Key": pairingKey } : {}
  });
  if (result.statusCode !== 200 || result.payload?.ok === false) return null;
  if (pairingKey && result.payload?.authorized !== true) return null;
  return {
    ok: true,
    status: "ready",
    endpoint,
    service: normalize(result.payload?.service, 120),
    version: normalize(result.payload?.version, 120),
    dbPath: normalize(result.payload?.dbPath, 500),
    routes: Array.isArray(result.payload?.routes) ? result.payload.routes : []
  };
}

async function probeLocalSensitiveStore(options = {}) {
  const pairingKey = readPairingKey(options);
  if (!pairingKey) {
    return { ok: false, status: "pairing-key-missing", endpoint: "", error: "pairing_key_missing" };
  }
  for (const port of LOCAL_STORE_PORTS) {
    const endpoint = endpointForPort(port);
    try {
      const ready = await probeEndpoint(endpoint, pairingKey);
      if (ready) return ready;
    } catch (_error) {
      // Try the next port.
    }
  }
  return { ok: false, status: "not-running", endpoint: "", error: "local_store_not_running" };
}

function uniqueExistingFiles(candidates = []) {
  const seen = new Set();
  const out = [];
  for (const candidate of candidates) {
    const filePath = normalize(candidate, 1000);
    if (!filePath || seen.has(filePath)) continue;
    seen.add(filePath);
    try {
      if (fs.existsSync(filePath)) out.push(filePath);
    } catch (_error) {
      // Ignore invalid candidate paths.
    }
  }
  return out;
}

function findHelperExecutable(options = {}) {
  const env = options.env || process.env;
  const explicit = normalize(env.ONLINECLASS_LOCAL_STORE_HELPER_EXE);
  const candidates = [];
  if (explicit) candidates.push(explicit);
  if (process.resourcesPath) {
    candidates.push(path.join(process.resourcesPath, "local-sensitive-store", "local-sensitive-store-desktop.exe"));
  }

  const roots = findWorkspaceRoots({
    candidates: [
      process.cwd(),
      __dirname,
      options.appPath,
      path.dirname(process.execPath || "")
    ].filter(Boolean)
  });
  if (roots?.v2Root) {
    candidates.push(path.join(roots.v2Root, "local-sensitive-store-desktop", "src-tauri", "target", "release", "local-sensitive-store-desktop.exe"));
    candidates.push(path.join(roots.v2Root, "local-sensitive-store-desktop", "src-tauri", "target", "debug", "local-sensitive-store-desktop.exe"));
  }

  const localAppData = normalize(env.LOCALAPPDATA);
  if (localAppData) {
    candidates.push(path.join(localAppData, "OnlineClass Local Sensitive Store", "OnlineClass Local Sensitive Store.exe"));
    candidates.push(path.join(localAppData, "Programs", "OnlineClass Local Sensitive Store", "OnlineClass Local Sensitive Store.exe"));
    candidates.push(path.join(localAppData, "Programs", "onlineclass-local-sensitive-store", "OnlineClass Local Sensitive Store.exe"));
  }

  return uniqueExistingFiles(candidates)[0] || "";
}

function startHelperProcess(options = {}) {
  const logger = options.logger || noopLogger();
  const helperPath = findHelperExecutable(options);
  if (!helperPath) {
    return { ok: false, helperPath: "", error: "helper_not_found" };
  }

  try {
    const child = spawn(helperPath, ["--background"], {
      detached: true,
      stdio: "ignore",
      windowsHide: true,
      env: {
        ...process.env,
        ONLINECLASS_LOCAL_STORE_BACKGROUND: "1"
      }
    });
    child.unref();
    logger.info?.("[local-sensitive-store] helper started", helperPath);
    return { ok: true, helperPath, pid: child.pid };
  } catch (error) {
    logger.warn?.("[local-sensitive-store] helper start failed", String(error?.message || error));
    return { ok: false, helperPath, error: String(error?.message || error) };
  }
}

async function waitForReady(options = {}) {
  const timeoutMs = Number(options.timeoutMs || READY_TIMEOUT_MS);
  const startedAt = Date.now();
  let last = await probeLocalSensitiveStore(options);
  while (!last.ok && (Date.now() - startedAt) <= timeoutMs) {
    await new Promise((resolve) => setTimeout(resolve, READY_INTERVAL_MS));
    last = await probeLocalSensitiveStore(options);
  }
  return last;
}

async function ensureLocalSensitiveStoreReady(options = {}) {
  if (ensurePromise) return ensurePromise;
  const logger = options.logger || noopLogger();
  ensurePromise = (async () => {
    const initial = await probeLocalSensitiveStore(options);
    if (initial.ok) {
      lastStatus = { ...initial, helperPath: "" };
      return lastStatus;
    }

    const started = startHelperProcess(options);
    if (!started.ok) {
      lastStatus = {
        ok: false,
        status: "helper-unavailable",
        endpoint: "",
        helperPath: started.helperPath || "",
        error: started.error || "helper_unavailable"
      };
      return lastStatus;
    }

    const ready = await waitForReady(options);
    lastStatus = {
      ...ready,
      helperPath: started.helperPath,
      status: ready.ok ? "ready" : ready.status,
      error: ready.ok ? "" : (ready.error || "local_store_not_ready")
    };
    if (!ready.ok) logger.warn?.("[local-sensitive-store] helper did not become ready", lastStatus.error);
    return lastStatus;
  })().finally(() => {
    ensurePromise = null;
  });
  return ensurePromise;
}

function getLastLocalSensitiveStoreStatus() {
  return { ...lastStatus };
}

function buildAllowedOrigins(baseUrl) {
  const out = new Set([
    "https://classaimate.pages.dev",
    "https://classaimate.netlify.app",
    "http://localhost:5000",
    "http://127.0.0.1:5000",
    "http://localhost:5002",
    "http://127.0.0.1:5002"
  ]);
  try {
    const parsed = new URL(normalizeBaseUrl(baseUrl));
    out.add(parsed.origin);
  } catch (_error) {
    // Ignore invalid user config.
  }
  return out;
}

function isAllowedInitiator(rawInitiator, allowedOrigins) {
  const initiator = normalize(rawInitiator, 500);
  if (!initiator) return true;
  const parsed = parseUrlSafe(initiator);
  if (!parsed) return false;
  if (allowedOrigins.has(parsed.origin)) return true;
  return parsed.hostname === "localhost" || parsed.hostname === "127.0.0.1";
}

function registerLocalSensitiveStoreRequestHeaders(electronSession, options = {}) {
  if (!electronSession?.webRequest?.onBeforeSendHeaders) {
    return { ok: false, error: "web_request_unavailable" };
  }
  const allowedOrigins = buildAllowedOrigins(options.baseUrl);
  const urls = LOCAL_STORE_PORTS.flatMap((port) => [
    `http://127.0.0.1:${port}/v1/*`,
    `http://localhost:${port}/v1/*`
  ]);
  electronSession.webRequest.onBeforeSendHeaders({ urls }, (details, callback) => {
    const headers = { ...(details.requestHeaders || {}) };
    if (headers["X-OnlineClass-Local-Store-Key"] || headers["x-onlineclass-local-store-key"]) {
      callback({ requestHeaders: headers });
      return;
    }
    if (!isAllowedInitiator(details.initiator || details.referrer || "", allowedOrigins)) {
      callback({ requestHeaders: headers });
      return;
    }
    const pairingKey = readPairingKey(options);
    if (pairingKey) {
      headers["X-OnlineClass-Local-Store-Key"] = pairingKey;
    }
    callback({ requestHeaders: headers });
  });
  return { ok: true };
}

module.exports = {
  LOCAL_STORE_PORTS,
  ensureLocalSensitiveStoreReady,
  findHelperExecutable,
  getLastLocalSensitiveStoreStatus,
  getPairingKeyPath,
  probeLocalSensitiveStore,
  readPairingKey,
  registerLocalSensitiveStoreRequestHeaders
};
