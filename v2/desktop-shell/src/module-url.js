const DEFAULT_BASE_URL = "https://classaimate.netlify.app/";
const DEFAULT_AUTH_MODE = "auto";
const LOCAL_DESKTOP_WEB_PORTS = new Set([5000, 5002]);
const LOCAL_COLLAB_PREVIEW_PORT = 8888;

function parseUrlSafe(rawUrl) {
  try {
    return new URL(String(rawUrl || ""));
  } catch (_error) {
    return null;
  }
}

function isLocalhostLikeHost(hostname) {
  const host = String(hostname || "").trim().toLowerCase();
  if (!host) return false;
  if (host === "localhost" || host === "127.0.0.1" || host === "::1") return true;
  return false;
}

function isLocalDevBaseUrl(rawUrl) {
  const parsed = parseUrlSafe(rawUrl);
  if (!parsed) return false;
  return isLocalhostLikeHost(parsed.hostname);
}

function sanitizeAuthMode(raw) {
  return String(raw || "").toLowerCase() === "auto" ? "auto" : "redirect";
}

function ensureTrailingSlash(pathname) {
  if (!pathname) return "/";
  return pathname.endsWith("/") ? pathname : `${pathname}/`;
}

function normalizeBaseUrl(raw) {
  const input = String(raw || "").trim() || DEFAULT_BASE_URL;
  const parsed = new URL(input);
  if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
    throw new Error("Base URL은 http/https만 허용됩니다.");
  }

  parsed.search = "";
  parsed.hash = "";

  const cleanedPath = (parsed.pathname || "/").replace(/\/teacher-dashboard(?:\/index\.html)?\/?$/i, "/");
  parsed.pathname = ensureTrailingSlash(cleanedPath);

  return parsed.toString();
}

function getExplicitPort(rawUrl) {
  const parsed = parseUrlSafe(rawUrl);
  if (!parsed) return null;
  const value = Number.parseInt(String(parsed.port || "").trim(), 10);
  if (!Number.isInteger(value) || value <= 0) return null;
  return value;
}

function deriveLocalWorkspaceBootstrapPlan(rawBaseUrl) {
  const normalizedBaseUrl = normalizeBaseUrl(rawBaseUrl || DEFAULT_BASE_URL);
  const parsed = parseUrlSafe(normalizedBaseUrl);
  if (!parsed || !isLocalhostLikeHost(parsed.hostname)) {
    return {
      enabled: false,
      autoUseEmulator: false,
      reason: "non-local-base-url",
      host: parsed?.hostname || "",
      webPort: getExplicitPort(normalizedBaseUrl),
      normalizedBaseUrl
    };
  }

  const explicitPort = getExplicitPort(normalizedBaseUrl);
  if (!LOCAL_DESKTOP_WEB_PORTS.has(explicitPort || 0)) {
    return {
      enabled: false,
      autoUseEmulator: false,
      reason: "unsupported-local-port",
      host: parsed.hostname,
      webPort: explicitPort,
      normalizedBaseUrl
    };
  }

  return {
    enabled: true,
    autoUseEmulator: true,
    reason: "local-workspace-runtime",
    host: parsed.hostname,
    webPort: explicitPort,
    needsStaticServer: explicitPort === 5000,
    needsCollabPreview: true,
    collabPreviewPort: LOCAL_COLLAB_PREVIEW_PORT,
    normalizedBaseUrl
  };
}

function buildModuleUrl(moduleInfo, cfg, options = {}) {
  if (!moduleInfo || !moduleInfo.path) {
    throw new Error("모듈 경로가 필요합니다.");
  }

  const safeBaseUrl = normalizeBaseUrl(cfg?.baseUrl || DEFAULT_BASE_URL);
  const url = new URL(String(moduleInfo.path), safeBaseUrl);
  const tenantId = String(cfg?.tenantId || "").trim();

  if (tenantId) {
    url.searchParams.set("tenantId", tenantId);
  } else {
    url.searchParams.delete("tenantId");
  }

  url.searchParams.set("desktop", "1");
  url.searchParams.set("authMode", sanitizeAuthMode(cfg?.authMode || DEFAULT_AUTH_MODE));
  url.searchParams.set("source", "desktop-shell");

  if (options.forceUseEmulator === true) {
    url.searchParams.set("useEmulator", "true");
  } else if (options.forceUseEmulator === false) {
    url.searchParams.delete("useEmulator");
  }

  return url.toString();
}

module.exports = {
  DEFAULT_AUTH_MODE,
  DEFAULT_BASE_URL,
  deriveLocalWorkspaceBootstrapPlan,
  buildModuleUrl,
  ensureTrailingSlash,
  getExplicitPort,
  isLocalDevBaseUrl,
  isLocalhostLikeHost,
  LOCAL_COLLAB_PREVIEW_PORT,
  normalizeBaseUrl,
  parseUrlSafe,
  sanitizeAuthMode
};
