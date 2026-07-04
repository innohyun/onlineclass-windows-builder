const { app, BrowserWindow, Menu, dialog, ipcMain, shell, session } = require("electron");
const path = require("path");
const fs = require("fs");
const log = require("electron-log/main");
const { autoUpdater } = require("electron-updater");
const {
  DEFAULT_AUTH_MODE,
  DEFAULT_BASE_URL,
  buildModuleUrl: buildModuleUrlForConfig,
  isLocalDevBaseUrl,
  isLocalhostLikeHost,
  normalizeBaseUrl,
  parseUrlSafe,
  sanitizeAuthMode
} = require("./module-url");
const {
  createLocalWorkspaceRuntime,
  deriveLocalWorkspaceBootstrapPlan
} = require("./local-workspace-runtime");
const {
  ensureLocalSensitiveStoreReady,
  registerLocalSensitiveStoreRequestHeaders
} = require("./local-sensitive-store-runtime");

const CONFIG_FILE_NAME = "desktop-shell-config.json";
const LEGACY_CONFIG_FILE_NAME = "teacher-dashboard-desktop-config.json";
const MODULE_MANIFEST_NAME = "modules.json";
const DEFAULT_START_MODULE = "teacher-dashboard";
const DEFAULT_ZOOM_FACTOR = 1.0;
const MIN_ZOOM_FACTOR = 0.7;
const MAX_ZOOM_FACTOR = 2.0;
const SESSION_PARTITION = "persist:onlineclass";
const LOAD_TIMEOUT_MS = 15000;
const LOCAL_DEV_LOAD_TIMEOUT_MS = 60000;
const MAX_RECOVERY_ATTEMPTS = 2;
const RECOVERY_LOOP_WINDOW_MS = 2 * 60 * 1000;
const RECOVERY_LOOP_LIMIT = 3;
const RECOVERY_PAUSE_MS = 5 * 60 * 1000;
const RECOVERY_RESET_STABILITY_MS = 5000;
const UPDATE_DUPLICATE_SUPPRESS_MS = 15000;
const APP_USER_MODEL_ID_BASE = "com.onlineclass.desktop-shell";

const appState = {
  mainWindow: null,
  moduleWindow: null,
  launcherRequested: false,
  requestedModuleId: "",
  modules: [],
  config: null,
  lastUpdateStatus: "idle",
  health: {
    lastModuleId: null,
    lastTargetUrl: null,
    lastMainFrameNavUrl: null,
    lastDidStartAt: 0,
    lastDidFinishAt: 0,
    lastFailureAt: 0,
    lastFailureReason: null,
    lastFailureCode: null,
    recoveryStage: "idle",
    recoveryAttempts: 0,
    timeoutCount: 0,
    unresponsiveCount: 0,
    renderGoneCount: 0,
    watchdogPausedUntil: 0,
    lastRecoveryFingerprint: null,
    localRuntimeStatus: "idle",
    localRuntimeReason: null,
    localRuntimeWorkspaceRoot: null,
    localRuntimeV2Root: null,
    localRuntimeWebPort: null,
    localRuntimeUseEmulator: false,
    localRuntimeStartedStatic: false,
    localRuntimeStartedEmulators: false,
    localRuntimeCollabPort: null,
    localRuntimeStartedCollabPreview: false,
    localRuntimeCollabPreviewReady: false,
    localRuntimeCollabConfigReady: false,
    localRuntimeCollabConfigIssues: [],
    localRuntimeError: null,
    localSensitiveStoreStatus: "idle",
    localSensitiveStoreEndpoint: null,
    localSensitiveStoreHelperPath: null,
    localSensitiveStoreError: null
  },
  runtime: {
    moduleId: null,
    targetUrl: null,
    loadTimer: null,
    recoveryAttempts: 0,
    lastStartToken: 0,
    lastMainFrameNavUrl: null,
    recoveryHistory: [],
    recoveryPausedUntil: 0,
    autoUpdaterInitialized: false,
    lastUpdateErrorAt: 0,
    lastUpdateErrorMessage: "",
    localSensitiveHeadersRegistered: false
  }
};

const localWorkspaceRuntime = createLocalWorkspaceRuntime({
  logger: log,
  getCandidates: () => [
    process.cwd(),
    app.getAppPath(),
    __dirname,
    path.dirname(process.execPath)
  ],
  getLogDir: () => path.join(app.getPath("userData"), "local-workspace-runtime")
});

function updateLocalSensitiveStoreHealth(status = {}) {
  updateHealth({
    localSensitiveStoreStatus: status.ok ? "ready" : (status.status || "unavailable"),
    localSensitiveStoreEndpoint: status.endpoint || null,
    localSensitiveStoreHelperPath: status.helperPath || null,
    localSensitiveStoreError: status.ok ? null : (status.error || null)
  });
}

function setupLocalSensitiveStoreIntegration() {
  if (appState.runtime.localSensitiveHeadersRegistered) return;
  const cfg = appState.config || loadConfig();
  const headerResult = registerLocalSensitiveStoreRequestHeaders(session.fromPartition(SESSION_PARTITION), {
    baseUrl: cfg.baseUrl,
    logger: log
  });
  appState.runtime.localSensitiveHeadersRegistered = headerResult.ok === true;
  if (!headerResult.ok) {
    updateLocalSensitiveStoreHealth({
      ok: false,
      status: "header-unavailable",
      error: headerResult.error || "header_unavailable"
    });
  }
}

async function refreshLocalSensitiveStoreStatus(reason = "manual") {
  updateHealth({
    localSensitiveStoreStatus: "starting",
    localSensitiveStoreError: null
  });
  try {
    const status = await ensureLocalSensitiveStoreReady({
      appPath: app.getAppPath(),
      logger: log
    });
    updateLocalSensitiveStoreHealth(status);
    return status;
  } catch (error) {
    const status = {
      ok: false,
      status: "failed",
      error: `${reason}:${String(error?.message || error)}`
    };
    updateLocalSensitiveStoreHealth(status);
    return status;
  }
}

function now() {
  return Date.now();
}

function resolveWindowIconPath() {
  if (process.platform !== "win32") return undefined;
  const candidates = [
    path.join(__dirname, "..", "build", "icon.ico"),
    process.execPath
  ];
  for (const candidate of candidates) {
    const value = String(candidate || "").trim();
    if (!value) continue;
    try {
      if (fs.existsSync(value)) return value;
    } catch (_error) {
      // ignore invalid path candidate
    }
  }
  return undefined;
}

function buildDesktopSafeUserAgent() {
  const raw = String(app.userAgentFallback || "").trim();
  if (!raw) return "";
  return raw.replace(/\sElectron\/[^\s]+/gi, "").trim();
}

function sanitizeZoomFactor(raw) {
  const value = Number(raw);
  if (!Number.isFinite(value)) return DEFAULT_ZOOM_FACTOR;
  return Math.max(MIN_ZOOM_FACTOR, Math.min(MAX_ZOOM_FACTOR, Math.round(value * 100) / 100));
}

function getZoomStepFromWheelDelta(deltaY) {
  const value = Number(deltaY);
  if (!Number.isFinite(value) || value === 0) return 0;
  return value < 0 ? 0.1 : -0.1;
}

function hasCtrlOrCmdModifier(input) {
  if (!input) return false;
  if (input.control || input.meta) return true;
  const modifiers = Array.isArray(input.modifiers) ? input.modifiers.map((m) => String(m).toLowerCase()) : [];
  return modifiers.includes("control") || modifiers.includes("meta") || modifiers.includes("command");
}

function getWheelDeltaY(input) {
  const candidates = [
    input?.deltaY,
    input?.wheelDeltaY,
    input?.wheelTicksY,
    input?.deltaYInLines
  ];
  for (const raw of candidates) {
    const num = Number(raw);
    if (Number.isFinite(num) && num !== 0) return num;
  }
  return 0;
}

function resolveLoadTimeoutMs() {
  const baseUrl = String(appState.config?.baseUrl || "");
  if (isLocalDevBaseUrl(baseUrl)) {
    return LOCAL_DEV_LOAD_TIMEOUT_MS;
  }
  return LOAD_TIMEOUT_MS;
}

function buildRecoveryFingerprint(reason, rawUrl) {
  const safeReason = String(reason || "unknown");
  const parsed = parseUrlSafe(rawUrl);
  if (!parsed) return `${safeReason}|unknown`;
  const host = String(parsed.hostname || "").toLowerCase() || "unknown-host";

  // load-timeout은 redirect/path 변동이 잦아 origin+module 기준으로 묶어 루프를 누적한다.
  if (safeReason === "load-timeout") {
    const moduleId = String(appState.runtime?.moduleId || "unknown-module").toLowerCase();
    return `${safeReason}|${host}|${moduleId}`;
  }

  const pathname = String(parsed.pathname || "/").toLowerCase();
  return `${safeReason}|${host}${pathname}`;
}

function markRecoveryLoop(reason, rawUrl) {
  const ts = now();
  const fingerprint = buildRecoveryFingerprint(reason, rawUrl);
  const history = Array.isArray(appState.runtime.recoveryHistory) ? appState.runtime.recoveryHistory : [];
  const fresh = history
    .filter((item) => item && Number.isFinite(item.ts) && (ts - item.ts) <= RECOVERY_LOOP_WINDOW_MS)
    .concat({ ts, fingerprint });
  appState.runtime.recoveryHistory = fresh;

  const sameCount = fresh.filter((item) => item.fingerprint === fingerprint).length;
  if (sameCount >= RECOVERY_LOOP_LIMIT) {
    appState.runtime.recoveryPausedUntil = ts + RECOVERY_PAUSE_MS;
    updateHealth({
      watchdogPausedUntil: appState.runtime.recoveryPausedUntil,
      lastRecoveryFingerprint: fingerprint
    });
    return { paused: true, fingerprint, sameCount };
  }

  updateHealth({ lastRecoveryFingerprint: fingerprint });
  return { paused: false, fingerprint, sameCount };
}

function isRecoveryPaused() {
  const until = Number(appState.runtime.recoveryPausedUntil || 0);
  if (!until) return false;
  if (now() >= until) {
    appState.runtime.recoveryPausedUntil = 0;
    updateHealth({ watchdogPausedUntil: 0 });
    return false;
  }
  return true;
}

function isLauncherArg(rawArg) {
  const arg = String(rawArg || "").trim().toLowerCase();
  return arg === "--launcher" || arg === "--settings";
}

function parseCliArgValue(argName, argv = process.argv) {
  const args = Array.isArray(argv) ? argv : [];
  const target = `--${String(argName || "").trim().toLowerCase()}`;
  if (!target || target === "--") return "";
  for (let i = 0; i < args.length; i += 1) {
    const raw = String(args[i] || "").trim();
    if (!raw) continue;
    const lower = raw.toLowerCase();
    if (lower.startsWith(`${target}=`)) {
      return String(raw.slice(raw.indexOf("=") + 1) || "").trim();
    }
    if (lower === target) {
      const next = String(args[i + 1] || "").trim();
      if (next && !next.startsWith("--")) return next;
    }
  }
  return "";
}

function isLauncherModeRequested() {
  return process.argv.some((arg) => isLauncherArg(arg));
}

function parseRequestedModuleId(argv = process.argv) {
  return parseCliArgValue("module", argv);
}

function sanitizeAppUserModelId(raw) {
  return String(raw || "")
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9.-]/g, "")
    .replace(/\.+/g, ".")
    .replace(/^\.+|\.+$/g, "");
}

function moduleIdToAppUserModelId(moduleId) {
  const normalized = String(moduleId || "").trim().toLowerCase();
  if (!normalized) return APP_USER_MODEL_ID_BASE;
  if (normalized === "teacher-dashboard") return `${APP_USER_MODEL_ID_BASE}.teacher-dashboard`;
  if (normalized === "team-hub") return `${APP_USER_MODEL_ID_BASE}.team-hub`;
  if (normalized === "yearbook-index" || normalized === "yearbook-lesson-plan") {
    return `${APP_USER_MODEL_ID_BASE}.yearbook`;
  }
  if (normalized === "seat-admin") return `${APP_USER_MODEL_ID_BASE}.seat`;
  return `${APP_USER_MODEL_ID_BASE}.${normalized.replace(/[^a-z0-9.-]/g, "-")}`;
}

function resolveRuntimeAppUserModelId(argv = process.argv) {
  const explicit = sanitizeAppUserModelId(parseCliArgValue("app-id", argv));
  if (explicit && explicit.startsWith(`${APP_USER_MODEL_ID_BASE}.`)) {
    return explicit;
  }
  if ((Array.isArray(argv) ? argv : []).some((arg) => isLauncherArg(arg))) {
    return `${APP_USER_MODEL_ID_BASE}.launcher`;
  }
  const requestedModuleId = parseRequestedModuleId(argv);
  if (requestedModuleId) return moduleIdToAppUserModelId(requestedModuleId);
  return APP_USER_MODEL_ID_BASE;
}

function resolveStartupModuleId() {
  const moduleIds = new Set(appState.modules.map((m) => m.id));
  const requested = String(appState.requestedModuleId || "").trim();
  if (requested) {
    if (moduleIds.has(requested)) {
      return requested;
    }
    log.warn("Unknown --module argument, fallback to configured startModule", requested);
  }
  if (moduleIds.has(appState.config?.startModule)) {
    return appState.config.startModule;
  }
  return DEFAULT_START_MODULE;
}

function isInternalAuthNavigation(rawUrl) {
  try {
    const parsed = parseUrlSafe(rawUrl);
    if (!parsed) return false;
    const host = String(parsed.hostname || "").toLowerCase();
    const pathname = String(parsed.pathname || "");

    if (host === "accounts.google.com") return true;
    if (host.endsWith(".firebaseapp.com") && pathname.startsWith("/__/auth/")) return true;
    if (host.endsWith(".web.app") && pathname.startsWith("/__/auth/")) return true;
    if (host.endsWith("googleapis.com")) return true;
    if (host.endsWith("gstatic.com")) return true;
    if (host.endsWith("googleusercontent.com")) return true;
    return false;
  } catch (_error) {
    return false;
  }
}

function isInAppNavigation(rawUrl) {
  const parsed = parseUrlSafe(rawUrl);
  if (!parsed) return false;

  const protocol = String(parsed.protocol || "").toLowerCase();
  if (protocol !== "http:" && protocol !== "https:") return false;

  const origins = new Set();
  try {
    origins.add(new URL(appState.config?.baseUrl || DEFAULT_BASE_URL).origin);
  } catch (_error) {
    // ignore
  }
  try {
    if (appState.runtime?.targetUrl) {
      origins.add(new URL(appState.runtime.targetUrl).origin);
    }
  } catch (_error) {
    // ignore
  }

  if (origins.has(parsed.origin)) return true;

  const host = String(parsed.hostname || "").toLowerCase();
  if (host === "classaimate.netlify.app") return true;
  if (isLocalhostLikeHost(host)) return true;

  return false;
}

function shouldResetRecoveryOnFinish(rawUrl) {
  const parsed = parseUrlSafe(rawUrl);
  if (!parsed) return false;
  const protocol = String(parsed.protocol || "").toLowerCase();
  if (protocol !== "http:" && protocol !== "https:") {
    return false;
  }
  if (!isInAppNavigation(parsed.toString())) {
    return false;
  }

  // did-fail-load 직후에도 did-frame-finish-load가 이어질 수 있어
  // 짧은 안정 구간이 확보되기 전에는 복구 카운터를 유지한다.
  const lastFailureAt = Number(appState.health?.lastFailureAt || 0);
  if (lastFailureAt > 0 && (now() - lastFailureAt) < RECOVERY_RESET_STABILITY_MS) {
    return false;
  }

  return true;
}

function readJsonSafe(filePath) {
  try {
    if (!fs.existsSync(filePath)) return null;
    const raw = fs.readFileSync(filePath, "utf8");
    return JSON.parse(raw);
  } catch (error) {
    log.warn("readJsonSafe failed", filePath, String(error));
    return null;
  }
}

function writeJsonSafe(filePath, data) {
  const dir = path.dirname(filePath);
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(filePath, JSON.stringify(data, null, 2), "utf8");
}

function sanitizeWindowBounds(bounds) {
  const width = Number(bounds?.width || 1440);
  const height = Number(bounds?.height || 960);
  const x = Number.isFinite(Number(bounds?.x)) ? Number(bounds.x) : undefined;
  const y = Number.isFinite(Number(bounds?.y)) ? Number(bounds.y) : undefined;
  const out = {
    width: Math.max(1100, Math.min(2200, Math.round(width))),
    height: Math.max(700, Math.min(1600, Math.round(height)))
  };
  if (Number.isFinite(x)) out.x = Math.round(x);
  if (Number.isFinite(y)) out.y = Math.round(y);
  return out;
}

function getDefaultConfig() {
  return {
    baseUrl: DEFAULT_BASE_URL,
    allowLocalhostBaseUrl: false,
    tenantId: "",
    authMode: DEFAULT_AUTH_MODE,
    startModule: DEFAULT_START_MODULE,
    openExternalLinks: true,
    zoomFactor: DEFAULT_ZOOM_FACTOR,
    lastGoodUrlByModule: {},
    windowBounds: { width: 1440, height: 960 }
  };
}

function sanitizeConfig(input) {
  const defaults = getDefaultConfig();
  const safe = input && typeof input === "object" ? input : {};

  let baseUrl = defaults.baseUrl;
  try {
    baseUrl = normalizeBaseUrl(safe.baseUrl || defaults.baseUrl);
  } catch {
    baseUrl = defaults.baseUrl;
  }

  const startModule = String(safe.startModule || defaults.startModule).trim() || defaults.startModule;

  return {
    baseUrl,
    allowLocalhostBaseUrl: safe.allowLocalhostBaseUrl === true,
    tenantId: String(safe.tenantId || "").trim(),
    authMode: sanitizeAuthMode(safe.authMode),
    startModule,
    openExternalLinks: safe.openExternalLinks !== false,
    zoomFactor: sanitizeZoomFactor(safe.zoomFactor),
    lastGoodUrlByModule:
      safe.lastGoodUrlByModule && typeof safe.lastGoodUrlByModule === "object"
        ? safe.lastGoodUrlByModule
        : {},
    windowBounds: sanitizeWindowBounds(safe.windowBounds)
  };
}

function getConfigPath() {
  return path.join(app.getPath("userData"), CONFIG_FILE_NAME);
}

function findLegacyConfigPathCandidates() {
  const appData = app.getPath("appData");
  return [
    path.join(appData, "com.onlineclass.teacher-dashboard-desktop", LEGACY_CONFIG_FILE_NAME),
    path.join(appData, "Teacher Dashboard Desktop", LEGACY_CONFIG_FILE_NAME),
    path.join(appData, "Teacher Dashboard Desktop Launcher", LEGACY_CONFIG_FILE_NAME)
  ];
}

function migrateLegacyConfigIfNeeded() {
  const currentPath = getConfigPath();
  if (fs.existsSync(currentPath)) return;

  const candidates = findLegacyConfigPathCandidates();
  for (const candidate of candidates) {
    const legacy = readJsonSafe(candidate);
    if (!legacy) continue;

    const migrated = sanitizeConfig({
      baseUrl: legacy.baseUrl,
      tenantId: legacy.tenantId,
      authMode: legacy.authMode,
      openExternalLinks: legacy.openExternalLinks,
      startModule: DEFAULT_START_MODULE,
      lastGoodUrlByModule: {},
      windowBounds: { width: 1440, height: 960 }
    });

    writeJsonSafe(currentPath, migrated);
    log.info("Migrated legacy config", candidate, "->", currentPath);
    return;
  }
}

function loadConfig() {
  migrateLegacyConfigIfNeeded();
  const cfg = readJsonSafe(getConfigPath());
  let safe = sanitizeConfig(cfg);

  // Installed app policy:
  // - default: localhost baseUrl는 production으로 자동 복구
  // - exception: 런처에서 사용자가 명시 저장한 localhost(baseUrl+allowLocalhostBaseUrl=true)는 유지
  if (app.isPackaged && isLocalDevBaseUrl(safe.baseUrl) && safe.allowLocalhostBaseUrl !== true) {
    safe = sanitizeConfig({
      ...safe,
      baseUrl: DEFAULT_BASE_URL
    });
    writeJsonSafe(getConfigPath(), safe);
    log.info("Reset localhost baseUrl to production default in packaged app");
  }

  appState.config = safe;
  return safe;
}

function saveConfig(nextConfig) {
  const safe = sanitizeConfig({
    ...nextConfig,
    allowLocalhostBaseUrl: isLocalDevBaseUrl(nextConfig?.baseUrl)
  });
  appState.config = safe;
  writeJsonSafe(getConfigPath(), safe);
  return safe;
}

function loadModules() {
  const manifestPath = path.join(app.getAppPath(), MODULE_MANIFEST_NAME);
  const parsed = readJsonSafe(manifestPath);
  const rows = Array.isArray(parsed?.modules) ? parsed.modules : [];

  const modules = rows
    .map((m) => ({
      id: String(m?.id || "").trim(),
      label: String(m?.label || "").trim(),
      path: String(m?.path || "").trim(),
      requiresTenant: Boolean(m?.requiresTenant)
    }))
    .filter((m) => m.id && m.label && m.path);

  if (!modules.length) {
    throw new Error("modules.json에 유효한 모듈이 없습니다.");
  }

  appState.modules = modules;
  return modules;
}

function findModuleById(moduleId) {
  return appState.modules.find((m) => m.id === moduleId) || null;
}

function buildModuleUrl(moduleId, cfg = appState.config, options = {}) {
  const moduleInfo = findModuleById(moduleId);
  if (!moduleInfo) {
    throw new Error(`알 수 없는 모듈: ${moduleId}`);
  }

  const safeConfig = sanitizeConfig(cfg || getDefaultConfig());

  return {
    moduleInfo,
    url: buildModuleUrlForConfig(moduleInfo, safeConfig, options)
  };
}

async function ensureLocalWorkspaceReady(cfg = appState.config) {
  const plan = deriveLocalWorkspaceBootstrapPlan(cfg?.baseUrl || DEFAULT_BASE_URL);
  if (!plan.enabled) {
    updateHealth({
      localRuntimeStatus: plan.reason === "unsupported-local-port" ? "skipped" : "idle",
      localRuntimeReason: plan.reason,
      localRuntimeWorkspaceRoot: null,
      localRuntimeV2Root: null,
      localRuntimeWebPort: plan.webPort || null,
      localRuntimeUseEmulator: false,
      localRuntimeStartedStatic: false,
      localRuntimeStartedEmulators: false,
      localRuntimeCollabPort: plan.collabPreviewPort || null,
      localRuntimeStartedCollabPreview: false,
      localRuntimeCollabPreviewReady: false,
      localRuntimeCollabConfigReady: false,
      localRuntimeCollabConfigIssues: [],
      localRuntimeError: null
    });
    return {
      enabled: false,
      autoUseEmulator: false,
      reason: plan.reason,
      plan
    };
  }

  updateHealth({
    localRuntimeStatus: "starting",
    localRuntimeReason: plan.reason,
    localRuntimeWorkspaceRoot: null,
    localRuntimeV2Root: null,
    localRuntimeWebPort: plan.webPort || null,
    localRuntimeUseEmulator: true,
    localRuntimeStartedStatic: false,
    localRuntimeStartedEmulators: false,
    localRuntimeCollabPort: plan.collabPreviewPort || null,
    localRuntimeStartedCollabPreview: false,
    localRuntimeCollabPreviewReady: false,
    localRuntimeCollabConfigReady: false,
    localRuntimeCollabConfigIssues: [],
    localRuntimeError: null
  });

  try {
    const ready = await localWorkspaceRuntime.ensureReady(plan.normalizedBaseUrl);
    updateHealth({
      localRuntimeStatus: "ready",
      localRuntimeReason: ready.reason || plan.reason,
      localRuntimeWorkspaceRoot: ready.roots?.workspaceRoot || null,
      localRuntimeV2Root: ready.roots?.v2Root || null,
      localRuntimeWebPort: ready.plan?.webPort || plan.webPort || null,
      localRuntimeUseEmulator: ready.autoUseEmulator === true,
      localRuntimeStartedStatic: ready.webServer?.started === true,
      localRuntimeStartedEmulators: ready.emulators?.started === true,
      localRuntimeCollabPort: ready.collabPreview?.port || plan.collabPreviewPort || null,
      localRuntimeStartedCollabPreview: ready.collabPreview?.started === true,
      localRuntimeCollabPreviewReady: ready.collabPreview?.ready === true,
      localRuntimeCollabConfigReady: ready.collabPreview?.configReady === true,
      localRuntimeCollabConfigIssues: Array.isArray(ready.collabPreview?.configIssues)
        ? ready.collabPreview.configIssues
        : [],
      localRuntimeError: null
    });
    return ready;
  } catch (error) {
    const message = String(error?.message || error);
    updateHealth({
      localRuntimeStatus: "failed",
      localRuntimeReason: plan.reason,
      localRuntimeWorkspaceRoot: null,
      localRuntimeV2Root: null,
      localRuntimeWebPort: plan.webPort || null,
      localRuntimeUseEmulator: true,
      localRuntimeStartedStatic: false,
      localRuntimeStartedEmulators: false,
      localRuntimeCollabPort: plan.collabPreviewPort || null,
      localRuntimeStartedCollabPreview: false,
      localRuntimeCollabPreviewReady: false,
      localRuntimeCollabConfigReady: false,
      localRuntimeCollabConfigIssues: [],
      localRuntimeError: message
    });
    throw error;
  }
}

function broadcastToMain(channel, payload) {
  if (!appState.mainWindow || appState.mainWindow.isDestroyed()) return;
  appState.mainWindow.webContents.send(channel, payload);
}

function updateHealth(patch) {
  appState.health = {
    ...appState.health,
    ...patch
  };
  broadcastToMain("desktop:health-updated", appState.health);
}

function clearLoadTimeout() {
  if (appState.runtime.loadTimer) {
    clearTimeout(appState.runtime.loadTimer);
    appState.runtime.loadTimer = null;
  }
}

function persistConfigPatch(patch) {
  const next = saveConfig({
    ...appState.config,
    ...patch
  });
  appState.config = next;
  broadcastToMain("desktop:config-updated", next);
  return next;
}

function setWindowZoomFactor(win, zoomFactor, options = {}) {
  if (!win || win.isDestroyed()) return sanitizeZoomFactor(appState.config?.zoomFactor);

  const opts = options || {};
  const safeFactor = sanitizeZoomFactor(zoomFactor);

  try {
    win.webContents.setZoomFactor(safeFactor);
  } catch (error) {
    log.warn("setZoomFactor failed", String(error));
  }

  if (opts.persist !== false) {
    persistConfigPatch({ zoomFactor: safeFactor });
  }

  return safeFactor;
}

function adjustWindowZoom(win, delta) {
  if (!win || win.isDestroyed()) return sanitizeZoomFactor(appState.config?.zoomFactor);
  const current = sanitizeZoomFactor(win.webContents.getZoomFactor());
  return setWindowZoomFactor(win, current + delta);
}

function resetWindowZoom(win) {
  return setWindowZoomFactor(win, DEFAULT_ZOOM_FACTOR);
}

function hasShiftModifier(input) {
  if (!input) return false;
  if (input.shift) return true;
  const modifiers = Array.isArray(input.modifiers) ? input.modifiers.map((m) => String(m).toLowerCase()) : [];
  return modifiers.includes("shift");
}

function reloadWindow(win, options = {}) {
  if (!win || win.isDestroyed()) return false;
  const ignoreCache = options.ignoreCache === true;
  try {
    if (ignoreCache && typeof win.webContents.reloadIgnoringCache === "function") {
      win.webContents.reloadIgnoringCache();
    } else {
      win.webContents.reload();
    }
    return true;
  } catch (error) {
    log.warn("reloadWindow failed", String(error));
    return false;
  }
}

function getActiveContentWindow() {
  const focused = BrowserWindow.getFocusedWindow();
  if (focused && !focused.isDestroyed()) return focused;
  if (appState.moduleWindow && !appState.moduleWindow.isDestroyed()) return appState.moduleWindow;
  if (appState.mainWindow && !appState.mainWindow.isDestroyed()) return appState.mainWindow;
  return null;
}

function showLauncherAndFocus() {
  if (!appState.mainWindow || appState.mainWindow.isDestroyed()) {
    createMainWindow();
  }
  if (!appState.mainWindow || appState.mainWindow.isDestroyed()) return;
  appState.mainWindow.show();
  appState.mainWindow.focus();
}

function setupApplicationMenu() {
  const template = [
    {
      label: "도구",
      submenu: [
        {
          label: "확대",
          accelerator: "CmdOrCtrl+=",
          click: () => {
            const win = getActiveContentWindow();
            if (win) adjustWindowZoom(win, 0.1);
          }
        },
        {
          label: "축소",
          accelerator: "CmdOrCtrl+-",
          click: () => {
            const win = getActiveContentWindow();
            if (win) adjustWindowZoom(win, -0.1);
          }
        },
        {
          label: "기본 크기",
          accelerator: "CmdOrCtrl+0",
          click: () => {
            const win = getActiveContentWindow();
            if (win) resetWindowZoom(win);
          }
        }
      ]
    }
  ];

  if (process.platform === "darwin") {
    template.unshift({
      label: app.name,
      submenu: [{ role: "about" }, { type: "separator" }, { role: "quit" }]
    });
  }

  Menu.setApplicationMenu(Menu.buildFromTemplate(template));
}

function attachZoomHandlers(win) {
  if (!win || win.isDestroyed()) return;

  win.webContents.on("before-input-event", (event, input) => {
    const type = String(input?.type || "").toLowerCase();
    if (type === "keydown") {
      const key = String(input?.key || "").toLowerCase();
      const ctrlOrCmd = hasCtrlOrCmdModifier(input);
      const shift = hasShiftModifier(input);

      if (key === "f5" || (ctrlOrCmd && key === "r")) {
        event.preventDefault();
        reloadWindow(win, { ignoreCache: shift });
        return;
      }

      if (!ctrlOrCmd) return;

      if (key === "+" || key === "=" || key === "add") {
        event.preventDefault();
        adjustWindowZoom(win, 0.1);
        return;
      }

      if (key === "-" || key === "_" || key === "subtract") {
        event.preventDefault();
        adjustWindowZoom(win, -0.1);
        return;
      }

      if (key === "0") {
        event.preventDefault();
        resetWindowZoom(win);
      }
      return;
    }

    if (type === "mousewheel") {
      const ctrlOrCmd = hasCtrlOrCmdModifier(input);
      if (!ctrlOrCmd) return;
      const zoomStep = getZoomStepFromWheelDelta(getWheelDeltaY(input));
      if (zoomStep !== 0) {
        event.preventDefault();
        adjustWindowZoom(win, zoomStep);
      }
    }
  });

  // 일부 환경에서 Ctrl+휠은 before-input-event 대신 zoom-changed만 들어온다.
  win.webContents.on("zoom-changed", (event, zoomDirection) => {
    event.preventDefault();
    if (zoomDirection === "in") {
      adjustWindowZoom(win, 0.1);
      return;
    }
    if (zoomDirection === "out") {
      adjustWindowZoom(win, -0.1);
    }
  });
}

function sendUpdateStatus(kind, detail = "") {
  const text = detail ? `${kind}: ${detail}` : kind;
  appState.lastUpdateStatus = text;
  broadcastToMain("desktop:update-status", text);
  log.info("[update]", text);
}

function makeRecoveryScript(reason) {
  const safeReason = JSON.stringify(String(reason || "unknown"));
  return `(() => {
    try {
      const old = document.getElementById('__desktop_shell_diag');
      if (old) old.remove();
      const wrap = document.createElement('div');
      wrap.id = '__desktop_shell_diag';
      wrap.style.cssText = 'position:fixed;left:12px;right:12px;bottom:12px;z-index:2147483647;background:rgba(15,23,42,.95);color:#e2e8f0;border:1px solid rgba(148,163,184,.35);border-radius:10px;padding:10px 12px;font-size:12px;line-height:1.5;font-family:Segoe UI,Apple SD Gothic Neo,sans-serif;box-shadow:0 10px 24px rgba(0,0,0,.35);';
      const title = document.createElement('div');
      title.textContent = '설치형 복구 모드: 화면 복구를 시도했습니다.';
      title.style.cssText = 'font-weight:700;margin-bottom:4px;';
      wrap.appendChild(title);
      const body = document.createElement('div');
      body.textContent = '복구 사유: ' + ${safeReason};
      body.style.cssText = 'opacity:.9;margin-bottom:8px;';
      wrap.appendChild(body);

      const row = document.createElement('div');
      row.style.cssText = 'display:flex;gap:6px;flex-wrap:wrap;';
      const mk = (label, fn) => { const b = document.createElement('button'); b.textContent = label; b.style.cssText='border:1px solid #334155;background:#0f172a;color:#e2e8f0;border-radius:8px;padding:4px 10px;cursor:pointer;'; b.onclick = fn; return b; };
      row.appendChild(mk('새로고침', () => window.location.reload()));
      row.appendChild(mk('홈으로', () => window.location.href = '/'));
      wrap.appendChild(row);

      (document.body || document.documentElement).appendChild(wrap);
    } catch (_) {}
  })();`;
}

async function recoverModule(reason, details = {}) {
  const win = appState.moduleWindow;
  if (!win || win.isDestroyed()) {
    showLauncher();
    return;
  }

  const timeoutMs = resolveLoadTimeoutMs();
  const currentUrl = String(win.webContents.getURL() || "");
  const targetUrl = String(appState.runtime.targetUrl || currentUrl || "");
  const isMainFrameLoading = typeof win.webContents.isLoadingMainFrame === "function"
    ? win.webContents.isLoadingMainFrame()
    : win.webContents.isLoading();

  if (reason !== "manual-recover" && isRecoveryPaused()) {
    log.warn("skip recoverModule during watchdog pause", {
      reason,
      pausedUntil: appState.runtime.recoveryPausedUntil
    });
    updateHealth({ recoveryStage: "paused" });
    return;
  }

  if (reason === "load-timeout" && !isMainFrameLoading) {
    log.info("skip load-timeout recovery because main frame is no longer loading", {
      currentUrl
    });
    clearLoadTimeout();
    return;
  }

  const loopState = reason === "manual-recover"
    ? { paused: false, fingerprint: "manual-recover", sameCount: 1 }
    : markRecoveryLoop(reason, targetUrl || currentUrl);
  if (reason !== "manual-recover" && loopState.paused) {
    log.warn("watchdog paused after repeated recoveries", {
      reason,
      fingerprint: loopState.fingerprint,
      sameCount: loopState.sameCount,
      pausedUntil: appState.runtime.recoveryPausedUntil
    });
    updateHealth({ recoveryStage: "paused" });
    return;
  }

  const attempt = appState.runtime.recoveryAttempts + 1;
  appState.runtime.recoveryAttempts = attempt;
  updateHealth({
    lastFailureAt: now(),
    lastFailureReason: reason,
    lastFailureCode: details.errorCode || null,
    recoveryAttempts: attempt
  });

  log.warn("recoverModule", {
    reason,
    details,
    attempt,
    targetUrl,
    currentUrl,
    isMainFrameLoading,
    timeoutMs,
    fingerprint: loopState.fingerprint
  });

  if (reason === "load-timeout") {
    updateHealth({ timeoutCount: appState.health.timeoutCount + 1 });
  }
  if (reason === "unresponsive") {
    updateHealth({ unresponsiveCount: appState.health.unresponsiveCount + 1 });
  }
  if (reason === "render-process-gone") {
    updateHealth({ renderGoneCount: appState.health.renderGoneCount + 1 });
  }

  if (attempt === 1) {
    updateHealth({ recoveryStage: "reloadIgnoringCache" });
    try {
      await win.webContents.executeJavaScript(makeRecoveryScript(reason), true).catch(() => undefined);
      win.webContents.reloadIgnoringCache();
      return;
    } catch (error) {
      log.error("recovery reloadIgnoringCache failed", String(error));
    }
  }

  if (attempt === 2) {
    if (isLocalDevBaseUrl(appState.config?.baseUrl)) {
      appState.runtime.recoveryPausedUntil = now() + RECOVERY_PAUSE_MS;
      updateHealth({
        recoveryStage: "paused",
        watchdogPausedUntil: appState.runtime.recoveryPausedUntil
      });
      log.warn("skip recreateWindow recovery in localhost mode", {
        pausedUntil: appState.runtime.recoveryPausedUntil
      });
      return;
    }
    updateHealth({ recoveryStage: "recreateWindow" });
    try {
      await openModuleWindow(appState.runtime.moduleId, {
        explicitUrl: appState.runtime.targetUrl,
        forceRecreate: true,
        keepRecoveryAttempts: true
      });
      return;
    } catch (error) {
      log.error("recovery recreate failed", String(error));
    }
  }

  if (attempt > MAX_RECOVERY_ATTEMPTS) {
    updateHealth({ recoveryStage: "fallbackLauncher" });
    if (appState.moduleWindow && !appState.moduleWindow.isDestroyed()) {
      appState.moduleWindow.close();
    }
    if (appState.launcherRequested) {
      showLauncherAndFocus();
    }
    await dialog.showMessageBox({
      type: "warning",
      title: "복구 필요",
      message: appState.launcherRequested
        ? "화면 복구에 실패해 런처로 돌아왔습니다."
        : "화면 복구에 실패했습니다.",
      detail: appState.launcherRequested
        ? "다시 열기 버튼으로 재시도하거나 네트워크/로그인 상태를 확인하세요."
        : "설정이 필요하면 앱을 `--launcher` 옵션으로 실행해 런처를 여세요."
    });
    if (!appState.launcherRequested) {
      app.quit();
    }
  }
}

function armLoadTimeout(win, token, meta = {}) {
  clearLoadTimeout();
  const timeoutMs = resolveLoadTimeoutMs();
  const navUrl = String(meta?.navUrl || appState.runtime.lastMainFrameNavUrl || appState.runtime.targetUrl || "");
  appState.runtime.loadTimer = setTimeout(() => {
    const sameToken = token === appState.runtime.lastStartToken;
    const focusedWindowAlive = appState.moduleWindow && !appState.moduleWindow.isDestroyed() && appState.moduleWindow === win;
    if (!sameToken || !focusedWindowAlive) {
      return;
    }
    // Google/Firebase 인증 페이지는 사용자 상호작용 대기가 길 수 있어
    // load-timeout 자동복구를 적용하지 않는다.
    const currentUrl = String(win.webContents.getURL() || "");
    if (isInternalAuthNavigation(currentUrl) || isInternalAuthNavigation(navUrl)) {
      log.info("skip load-timeout recovery during auth flow", currentUrl || navUrl);
      return;
    }

    const isMainFrameLoading = typeof win.webContents.isLoadingMainFrame === "function"
      ? win.webContents.isLoadingMainFrame()
      : win.webContents.isLoading();
    if (!isMainFrameLoading) {
      log.info("skip load-timeout recovery because main frame load already settled", {
        currentUrl,
        navUrl
      });
      return;
    }
    void recoverModule("load-timeout", {
      timeoutMs,
      navUrl,
      currentUrl,
      isMainFrameLoading
    });
  }, timeoutMs);
}

function persistModuleWindowBounds() {
  if (!appState.moduleWindow || appState.moduleWindow.isDestroyed()) return;
  const bounds = appState.moduleWindow.getBounds();
  persistConfigPatch({
    windowBounds: {
      width: bounds.width,
      height: bounds.height,
      x: bounds.x,
      y: bounds.y
    }
  });
}

function attachModuleWindowEvents(win) {
  attachZoomHandlers(win);

  win.webContents.on("did-start-navigation", (_event, url, isInPlace, isMainFrame) => {
    if (!isMainFrame || isInPlace) return;
    const token = now();
    const navUrl = String(url || "");
    appState.runtime.lastStartToken = token;
    appState.runtime.lastMainFrameNavUrl = navUrl;
    updateHealth({
      lastDidStartAt: token,
      recoveryStage: "loading",
      lastModuleId: appState.runtime.moduleId,
      lastTargetUrl: appState.runtime.targetUrl,
      lastMainFrameNavUrl: navUrl
    });
    const timeoutMs = resolveLoadTimeoutMs();
    log.info("main-frame did-start-navigation", { url: navUrl, timeoutMs });
    armLoadTimeout(win, token, { navUrl });
  });

  win.webContents.on("did-frame-finish-load", (_event, isMainFrame) => {
    if (!isMainFrame) return;
    clearLoadTimeout();
    const finishedUrl = String(win.webContents.getURL() || appState.runtime.lastMainFrameNavUrl || "");
    const shouldResetRecovery = shouldResetRecoveryOnFinish(finishedUrl);
    if (shouldResetRecovery) {
      appState.runtime.recoveryAttempts = 0;
      appState.runtime.recoveryHistory = [];
      appState.runtime.recoveryPausedUntil = 0;
      appState.runtime.targetUrl = finishedUrl || appState.runtime.targetUrl;
    } else {
      log.warn("main-frame finish without stable in-app URL; keep recovery counters", {
        finishedUrl,
        recoveryAttempts: appState.runtime.recoveryAttempts
      });
    }
    setWindowZoomFactor(win, appState.config?.zoomFactor, { persist: false });
    updateHealth({
      lastDidFinishAt: now(),
      recoveryStage: shouldResetRecovery ? "idle" : "recovering",
      recoveryAttempts: appState.runtime.recoveryAttempts,
      watchdogPausedUntil: appState.runtime.recoveryPausedUntil,
      lastMainFrameNavUrl: shouldResetRecovery ? (finishedUrl || null) : appState.runtime.lastMainFrameNavUrl
    });
    log.info("main-frame did-frame-finish-load", {
      url: finishedUrl,
      recoveryReset: shouldResetRecovery
    });

    const moduleId = appState.runtime.moduleId;
    if (moduleId && shouldResetRecovery) {
      const next = saveConfig({
        ...appState.config,
        lastGoodUrlByModule: {
          ...appState.config.lastGoodUrlByModule,
          [moduleId]: finishedUrl || appState.runtime.targetUrl
        }
      });
      appState.config = next;
      broadcastToMain("desktop:config-updated", next);
    }
  });

  win.webContents.on("did-fail-load", (_event, errorCode, errorDescription, validatedURL, isMainFrame) => {
    if (!isMainFrame) return;
    if (errorCode === -3) return;
    log.warn("main-frame did-fail-load", {
      errorCode,
      errorDescription,
      validatedURL
    });
    void recoverModule("did-fail-load", {
      errorCode,
      errorDescription,
      validatedURL
    });
  });

  win.webContents.on("render-process-gone", (_event, details) => {
    void recoverModule("render-process-gone", {
      reason: details?.reason || "unknown",
      exitCode: details?.exitCode ?? null
    });
  });

  win.on("unresponsive", () => {
    void recoverModule("unresponsive");
  });

  win.on("resized", persistModuleWindowBounds);
  win.on("moved", persistModuleWindowBounds);

  win.on("closed", () => {
    clearLoadTimeout();
    appState.moduleWindow = null;
    appState.runtime = {
      moduleId: null,
      targetUrl: null,
      loadTimer: null,
      recoveryAttempts: 0,
      lastStartToken: 0,
      lastMainFrameNavUrl: null,
      recoveryHistory: [],
      recoveryPausedUntil: 0,
      autoUpdaterInitialized: appState.runtime.autoUpdaterInitialized,
      lastUpdateErrorAt: appState.runtime.lastUpdateErrorAt,
      lastUpdateErrorMessage: appState.runtime.lastUpdateErrorMessage
    };
    if (appState.launcherRequested) {
      showLauncherAndFocus();
      return;
    }
    app.quit();
  });

  win.webContents.setWindowOpenHandler(({ url }) => {
    const parsed = parseUrlSafe(url);
    if (!parsed) return { action: "allow" };
    const protocol = String(parsed.protocol || "").toLowerCase();

    if (protocol === "mailto:" || protocol === "tel:") {
      shell.openExternal(parsed.toString()).catch((err) => log.warn("openExternal failed", String(err)));
      return { action: "deny" };
    }

    if (protocol !== "http:" && protocol !== "https:") {
      return { action: "allow" };
    }

    // Firebase/Google 인증 URL은 외부 브라우저로 이탈하면 로그인 상태가 끊기므로
    // 반드시 앱 내부 창에서 처리한다.
    if (isInternalAuthNavigation(url)) {
      return {
        action: "allow",
        overrideBrowserWindowOptions: {
          title: "로그인 - Google 계정",
          width: 920,
          height: 760,
          autoHideMenuBar: true,
          parent: win,
          modal: false,
          webPreferences: {
            partition: SESSION_PARTITION,
            contextIsolation: true,
            sandbox: true,
            nodeIntegration: false,
            webSecurity: true
          }
        }
      };
    }

    // 같은 서비스 도메인 링크는 앱 내부 같은 창에서 이동한다.
    if (isInAppNavigation(url)) {
      setImmediate(() => {
        if (!win.isDestroyed()) {
          win.loadURL(parsed.toString()).catch((err) => log.warn("in-app navigation failed", String(err)));
        }
      });
      return { action: "deny" };
    }

    const shouldExternal = appState.config?.openExternalLinks !== false;
    if (shouldExternal) {
      shell.openExternal(parsed.toString()).catch((err) => log.warn("openExternal failed", String(err)));
      return { action: "deny" };
    }
    return { action: "allow" };
  });
}

function getModuleWindowBounds() {
  const b = sanitizeWindowBounds(appState.config?.windowBounds);
  return {
    width: b.width,
    height: b.height,
    x: b.x,
    y: b.y,
    minWidth: 1100,
    minHeight: 700
  };
}

async function openModuleWindow(moduleId, options = {}) {
  const moduleInfo = findModuleById(moduleId);
  if (!moduleInfo) {
    throw new Error("모듈을 찾을 수 없습니다.");
  }

  const cfg = appState.config || loadConfig();
  if (moduleInfo.requiresTenant && !cfg.tenantId.trim()) {
    throw new Error("이 모듈은 tenantId가 필요합니다.");
  }

  setupLocalSensitiveStoreIntegration();
  void refreshLocalSensitiveStoreStatus("open-module");

  const localRuntime = await ensureLocalWorkspaceReady(cfg);
  const targetUrl =
    options.explicitUrl ||
    buildModuleUrl(moduleId, cfg, {
      forceUseEmulator: localRuntime.autoUseEmulator === true
    }).url;
  const shouldResetRecovery = options.keepRecoveryAttempts !== true;

  if (options.forceRecreate && appState.moduleWindow && !appState.moduleWindow.isDestroyed()) {
    appState.moduleWindow.destroy();
    appState.moduleWindow = null;
  }

  if (appState.moduleWindow && !appState.moduleWindow.isDestroyed()) {
    appState.runtime.moduleId = moduleId;
    appState.runtime.targetUrl = targetUrl;
    appState.runtime.lastMainFrameNavUrl = targetUrl;
    if (shouldResetRecovery) {
      appState.runtime.recoveryAttempts = 0;
      appState.runtime.recoveryHistory = [];
      appState.runtime.recoveryPausedUntil = 0;
      updateHealth({
        recoveryAttempts: 0,
        watchdogPausedUntil: 0
      });
    }
    setWindowZoomFactor(appState.moduleWindow, cfg.zoomFactor, { persist: false });
    appState.moduleWindow.loadURL(targetUrl);
    appState.moduleWindow.show();
    appState.moduleWindow.focus();
    hideLauncher();
    return targetUrl;
  }

  const bounds = getModuleWindowBounds();
  const win = new BrowserWindow({
    title: `OnlineClass - ${moduleInfo.label}`,
    icon: resolveWindowIconPath(),
    width: bounds.width,
    height: bounds.height,
    x: bounds.x,
    y: bounds.y,
    minWidth: bounds.minWidth,
    minHeight: bounds.minHeight,
    autoHideMenuBar: true,
    show: false,
    webPreferences: {
      partition: SESSION_PARTITION,
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      webSecurity: true
    }
  });

  const safeUa = buildDesktopSafeUserAgent();
  if (safeUa) {
    try {
      win.webContents.setUserAgent(safeUa);
    } catch (error) {
      log.warn("module window setUserAgent failed", String(error));
    }
  }
  setWindowZoomFactor(win, cfg.zoomFactor, { persist: false });

  appState.moduleWindow = win;
  appState.runtime.moduleId = moduleId;
  appState.runtime.targetUrl = targetUrl;
  appState.runtime.lastMainFrameNavUrl = targetUrl;
  if (shouldResetRecovery) {
    appState.runtime.recoveryAttempts = 0;
    appState.runtime.recoveryHistory = [];
    appState.runtime.recoveryPausedUntil = 0;
    updateHealth({
      recoveryAttempts: 0,
      watchdogPausedUntil: 0
    });
  }

  attachModuleWindowEvents(win);

  await win.loadURL(targetUrl);
  win.show();
  hideLauncher();
  return targetUrl;
}

function hideLauncher() {
  if (appState.mainWindow && !appState.mainWindow.isDestroyed()) {
    appState.mainWindow.hide();
  }
}

function showLauncher() {
  showLauncherAndFocus();
}

function createMainWindow() {
  const win = new BrowserWindow({
    title: "OnlineClass Desktop Launcher",
    icon: resolveWindowIconPath(),
    width: 1024,
    height: 820,
    minWidth: 900,
    minHeight: 680,
    autoHideMenuBar: true,
    show: false,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      webSecurity: true
    }
  });

  const safeUa = buildDesktopSafeUserAgent();
  if (safeUa) {
    try {
      win.webContents.setUserAgent(safeUa);
    } catch (error) {
      log.warn("main window setUserAgent failed", String(error));
    }
  }
  setWindowZoomFactor(win, appState.config?.zoomFactor, { persist: false });
  attachZoomHandlers(win);

  appState.mainWindow = win;
  win.loadFile(path.join(__dirname, "renderer", "index.html"));

  win.on("closed", () => {
    appState.mainWindow = null;
    if (appState.moduleWindow && !appState.moduleWindow.isDestroyed()) {
      appState.moduleWindow.close();
    }
  });
}

function registerIpcHandlers() {
  ipcMain.handle("desktop:getBootstrapData", () => {
    const cfg = appState.config || loadConfig();
    return {
      appVersion: app.getVersion(),
      config: cfg,
      modules: appState.modules,
      health: appState.health,
      updateStatus: appState.lastUpdateStatus
    };
  });

  ipcMain.handle("desktop:saveConfig", (_event, nextConfig) => {
    const saved = saveConfig(nextConfig);
    broadcastToMain("desktop:config-updated", saved);
    return saved;
  });

  ipcMain.handle("desktop:openModule", async (_event, moduleId) => {
    const targetUrl = await openModuleWindow(moduleId);
    return {
      ok: true,
      moduleId,
      targetUrl
    };
  });

  ipcMain.handle("desktop:healthSnapshot", () => appState.health);

  ipcMain.handle("desktop:recoverNow", async () => {
    await recoverModule("manual-recover");
    return appState.health;
  });

  ipcMain.handle("desktop:openExternal", async (_event, target) => {
    const parsed = new URL(String(target));
    await shell.openExternal(parsed.toString());
    return true;
  });

  ipcMain.handle("desktop:showLauncher", () => {
    showLauncherAndFocus();
    return true;
  });

  ipcMain.handle("desktop:getZoom", () => sanitizeZoomFactor(appState.config?.zoomFactor));

  ipcMain.handle("desktop:setZoom", (_event, zoomFactor) => {
    const active = getActiveContentWindow();
    if (!active) return sanitizeZoomFactor(appState.config?.zoomFactor);
    return setWindowZoomFactor(active, zoomFactor);
  });
}

function setupAutoUpdater() {
  if (appState.runtime.autoUpdaterInitialized) {
    return;
  }
  appState.runtime.autoUpdaterInitialized = true;

  if (!app.isPackaged) {
    sendUpdateStatus("dev-mode", "자동업데이트는 패키징 빌드에서만 동작합니다.");
    return;
  }

  log.initialize();
  autoUpdater.logger = log;
  autoUpdater.autoDownload = true;
  autoUpdater.autoInstallOnAppQuit = true;

  autoUpdater.on("checking-for-update", () => sendUpdateStatus("checking"));
  autoUpdater.on("update-available", (info) => sendUpdateStatus("available", info?.version || "new"));
  autoUpdater.on("update-not-available", () => sendUpdateStatus("not-available"));
  autoUpdater.on("download-progress", (progress) => {
    const pct = Number(progress?.percent || 0).toFixed(1);
    sendUpdateStatus("downloading", `${pct}%`);
  });
  autoUpdater.on("update-downloaded", async (info) => {
    sendUpdateStatus("downloaded", info?.version || "new");
    const result = await dialog.showMessageBox({
      type: "info",
      title: "업데이트 준비 완료",
      message: "새 버전이 다운로드되었습니다.",
      detail: "지금 재시작하면 업데이트가 적용됩니다.",
      buttons: ["지금 재시작", "나중에"],
      defaultId: 0,
      cancelId: 1
    });

    if (result.response === 0) {
      autoUpdater.quitAndInstall();
    }
  });
  const isDuplicateUpdateError = (signature) => {
    const nowTs = now();
    const lastSignature = String(appState.runtime.lastUpdateErrorMessage || "");
    const lastAt = Number(appState.runtime.lastUpdateErrorAt || 0);
    if (lastSignature === signature && (nowTs - lastAt) <= UPDATE_DUPLICATE_SUPPRESS_MS) {
      return true;
    }
    appState.runtime.lastUpdateErrorMessage = signature;
    appState.runtime.lastUpdateErrorAt = nowTs;
    return false;
  };

  const handleUpdaterError = (error) => {
    const message = String(error?.message || error || "");
    const isMissingChannel = message.includes("latest.yml") && message.includes("404");
    if (isMissingChannel) {
      if (isDuplicateUpdateError("missing-latest-yml")) {
        return;
      }
      log.warn("[update] channel file missing (latest.yml):", message);
      sendUpdateStatus("not-configured", "업데이트 서버 미설정");
      return;
    }
    if (isDuplicateUpdateError(`error:${message}`)) {
      return;
    }
    sendUpdateStatus("error", message);
  };

  autoUpdater.on("error", handleUpdaterError);

  autoUpdater.checkForUpdates().catch((error) => {
    handleUpdaterError(error);
  });
}

async function bootstrap() {
  appState.launcherRequested = isLauncherModeRequested();
  appState.requestedModuleId = parseRequestedModuleId();
  appState.config = loadConfig();
  loadModules();
  setupApplicationMenu();
  registerIpcHandlers();
  setupAutoUpdater();
  setupLocalSensitiveStoreIntegration();
  void refreshLocalSensitiveStoreStatus("startup");

  const startModuleId = resolveStartupModuleId();

  if (appState.launcherRequested) {
    showLauncherAndFocus();
    return;
  }

  if (!startModuleId || !findModuleById(startModuleId)) {
    await dialog.showMessageBox({
      type: "warning",
      title: "시작 모듈 확인 필요",
      message: "시작 모듈을 찾지 못했습니다.",
      detail: "설정을 바꾸려면 앱을 `--launcher` 옵션으로 실행하세요."
    });
    app.quit();
    return;
  }

  try {
    await openModuleWindow(startModuleId);
  } catch (error) {
    log.warn("start module auto-open failed", String(error));
    await dialog.showMessageBox({
      type: "warning",
      title: "시작 실패",
      message: "시작 모듈을 열지 못했습니다.",
      detail: "설정이 필요하면 앱을 `--launcher` 옵션으로 실행하세요."
    });
    app.quit();
  }
}

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});

app.on("before-quit", () => {
  void localWorkspaceRuntime.shutdown().catch((error) => {
    log.warn("local workspace runtime shutdown failed", String(error));
  });
});

app.on("activate", () => {
  if (appState.moduleWindow && !appState.moduleWindow.isDestroyed()) {
    appState.moduleWindow.show();
    appState.moduleWindow.focus();
    return;
  }

  if (appState.launcherRequested) {
    showLauncherAndFocus();
    return;
  }

  const startModuleId = resolveStartupModuleId();

  if (startModuleId && findModuleById(startModuleId)) {
    void openModuleWindow(startModuleId).catch((error) => {
      log.warn("activate start module failed", String(error));
    });
  }
});

app.whenReady().then(() => {
  if (process.platform === "win32") {
    app.setAppUserModelId(resolveRuntimeAppUserModelId(process.argv));
  }
  void bootstrap();
});
