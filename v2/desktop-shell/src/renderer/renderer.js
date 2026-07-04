(function () {
  const api = window.desktopShell;

  const els = {
    appVersion: document.getElementById("appVersion"),
    updateStatus: document.getElementById("updateStatus"),
    baseUrl: document.getElementById("baseUrl"),
    tenantId: document.getElementById("tenantId"),
    authMode: document.getElementById("authMode"),
    startModule: document.getElementById("startModule"),
    openExternalLinks: document.getElementById("openExternalLinks"),
    btnSave: document.getElementById("btnSave"),
    btnOpenStart: document.getElementById("btnOpenStart"),
    btnRecover: document.getElementById("btnRecover"),
    moduleList: document.getElementById("moduleList"),
    healthBox: document.getElementById("healthBox"),
    logBox: document.getElementById("logBox")
  };

  const state = {
    config: null,
    modules: [],
    health: null,
    unsubscribers: []
  };

  function nowText() {
    return new Date().toLocaleTimeString();
  }

  function log(msg) {
    const next = `[${nowText()}] ${msg}`;
    els.logBox.textContent = els.logBox.textContent
      ? `${next}\n${els.logBox.textContent}`
      : next;
  }

  function normalizeBaseUrl(raw) {
    const text = String(raw || "").trim();
    if (!text) throw new Error("Base URL이 비어 있습니다.");
    const parsed = new URL(text);
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
      throw new Error("Base URL은 http/https만 허용됩니다.");
    }
    parsed.search = "";
    parsed.hash = "";
    parsed.pathname = (parsed.pathname || "/").replace(
      /\/teacher-dashboard(?:\/index\.html)?\/?$/i,
      "/",
    );
    if (!parsed.pathname.endsWith("/")) parsed.pathname += "/";
    return parsed.toString();
  }

  function renderStartModuleSelect() {
    els.startModule.innerHTML = "";
    state.modules.forEach((moduleInfo) => {
      const option = document.createElement("option");
      option.value = moduleInfo.id;
      option.textContent = moduleInfo.label;
      els.startModule.appendChild(option);
    });

    if (state.config && state.modules.some((m) => m.id === state.config.startModule)) {
      els.startModule.value = state.config.startModule;
    }
  }

  function renderModules() {
    els.moduleList.innerHTML = "";
    state.modules.forEach((moduleInfo) => {
      const row = document.createElement("div");
      row.className = "module-item";

      const left = document.createElement("div");
      left.className = "module-meta";

      const label = document.createElement("div");
      label.className = "label";
      label.textContent = moduleInfo.label;

      const path = document.createElement("div");
      path.className = "path";
      path.textContent = moduleInfo.path;

      left.appendChild(label);
      left.appendChild(path);
      if (moduleInfo.requiresTenant) {
        const badge = document.createElement("span");
        badge.className = "badge";
        badge.textContent = "tenant 필요";
        left.appendChild(badge);
      }

      const btn = document.createElement("button");
      btn.className = "btn ghost";
      btn.type = "button";
      btn.textContent = "열기";
      btn.addEventListener("click", async () => {
        try {
          await saveConfig({ silent: true });
          const result = await api.openModule(moduleInfo.id);
          log(`모듈 실행: ${moduleInfo.label} (${result.targetUrl})`);
        } catch (error) {
          log(`모듈 실행 실패 (${moduleInfo.label}): ${String(error)}`);
        }
      });

      row.appendChild(left);
      row.appendChild(btn);
      els.moduleList.appendChild(row);
    });
  }

  function renderConfig() {
    const cfg = state.config;
    els.baseUrl.value = cfg.baseUrl;
    els.tenantId.value = cfg.tenantId || "";
    els.authMode.value = cfg.authMode === "auto" ? "auto" : "redirect";
    els.openExternalLinks.checked = cfg.openExternalLinks !== false;
    if (state.modules.some((m) => m.id === cfg.startModule)) {
      els.startModule.value = cfg.startModule;
    }
  }

  function renderHealth() {
    els.healthBox.textContent = JSON.stringify(state.health || {}, null, 2);
  }

  function collectConfig() {
    const baseUrl = normalizeBaseUrl(els.baseUrl.value);
    const tenantId = String(els.tenantId.value || "").trim();
    const authMode = String(els.authMode.value || "").trim() === "auto" ? "auto" : "redirect";
    const startModule = String(els.startModule.value || "").trim();

    if (!state.modules.some((m) => m.id === startModule)) {
      throw new Error("시작 모듈이 유효하지 않습니다.");
    }

    return {
      ...state.config,
      baseUrl,
      tenantId,
      authMode,
      startModule,
      openExternalLinks: !!els.openExternalLinks.checked
    };
  }

  async function saveConfig(options) {
    const opts = options || {};
    const next = collectConfig();
    const saved = await api.saveConfig(next);
    state.config = saved;
    renderConfig();
    if (!opts.silent) {
      log("설정 저장 완료");
    }
    return saved;
  }

  function bindButtons() {
    els.btnSave.addEventListener("click", async () => {
      try {
        await saveConfig();
      } catch (error) {
        log(`설정 저장 실패: ${String(error)}`);
      }
    });

    els.btnOpenStart.addEventListener("click", async () => {
      try {
        const saved = await saveConfig({ silent: true });
        const result = await api.openModule(saved.startModule);
        log(`시작 모듈 실행: ${result.targetUrl}`);
      } catch (error) {
        log(`시작 모듈 실행 실패: ${String(error)}`);
      }
    });

    els.btnRecover.addEventListener("click", async () => {
      try {
        const snapshot = await api.recoverNow();
        state.health = snapshot;
        renderHealth();
        log("수동 복구 실행 요청 완료");
      } catch (error) {
        log(`복구 요청 실패: ${String(error)}`);
      }
    });
  }

  function bindSubscriptions() {
    state.unsubscribers.push(
      api.onUpdateStatus((text) => {
        els.updateStatus.textContent = `업데이트 상태: ${text}`;
      }),
    );

    state.unsubscribers.push(
      api.onHealthUpdated((health) => {
        state.health = health;
        renderHealth();
      }),
    );

    state.unsubscribers.push(
      api.onConfigUpdated((cfg) => {
        state.config = cfg;
        renderConfig();
      }),
    );
  }

  async function init() {
    const data = await api.getBootstrapData();
    state.config = data.config;
    state.modules = data.modules;
    state.health = data.health;

    els.appVersion.textContent = `v${data.appVersion}`;
    els.updateStatus.textContent = `업데이트 상태: ${data.updateStatus || "idle"}`;

    renderStartModuleSelect();
    renderModules();
    renderConfig();
    renderHealth();

    bindButtons();
    bindSubscriptions();

    log("런처 로드 완료");
  }

  window.addEventListener("DOMContentLoaded", () => {
    init().catch((error) => {
      log(`초기화 실패: ${String(error)}`);
    });
  });
})();
