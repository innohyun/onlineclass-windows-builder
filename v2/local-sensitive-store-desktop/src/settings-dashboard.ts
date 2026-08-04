import { invoke } from "@tauri-apps/api/core";

export type DesktopPreferences = {
  ok: boolean;
  startWithWindows: boolean;
  keepRunningOnClose: boolean;
  error?: string;
};

type DisconnectResult = {
  ok: boolean;
  connected?: boolean;
  localDataPreserved?: boolean;
  error?: string;
};

export type SettingsDashboardView = {
  connected: boolean;
  needsReconnect?: boolean;
  tenantLabel: string;
  accountLabel: string;
  backupConfigured: boolean;
  backupOk: boolean;
  backupLocation: string;
  backupLatest: string;
  appVersion: string;
};

type Options = {
  onDisconnected(): Promise<void>;
};

function element<T extends HTMLElement>(id: string) {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element: ${id}`);
  return found as T;
}

function text(id: string, value: string) {
  element(id).textContent = value;
}

function setBadge(label: string, tone: "ok" | "warning" | "error") {
  const badge = element("settingsConnectionBadge");
  badge.textContent = label;
  badge.className = `settings-badge is-${tone}`;
}

function setBackupBadge(label: string, tone: "ok" | "warning" | "error") {
  const badge = element("settingsBackupBadge");
  badge.textContent = label;
  badge.className = `settings-badge is-${tone}`;
}

function setPreferenceStatus(message: string, tone: "neutral" | "ok" | "error" = "neutral") {
  const status = element("settingsPreferenceStatus");
  status.textContent = message;
  status.className = `settings-inline-status${tone === "neutral" ? "" : ` is-${tone}`}`;
}

export function renderSettingsDashboard(view: SettingsDashboardView) {
  element("settingsConnectedContent").hidden = !view.connected;
  element("deviceAuthPanel").hidden = view.connected;
  text("settingsTenantText", view.tenantLabel || "연결된 학급 없음");
  text("settingsAccountText", view.accountLabel || "-");
  text("settingsBackupLocationText", view.backupLocation || "-");
  text("settingsBackupLatestText", view.backupLatest || "-");
  text("settingsAppVersionFooter", `앱 v${view.appVersion || "-"}`);

  if (view.connected && view.needsReconnect) {
    setBadge("재연결 필요", "warning");
    text("settingsConnectionDescription", "브라우저 로그인 정보가 만료되었습니다. 교사 로그인으로 다시 연결해 주세요.");
  } else {
    setBadge("정상 연결", "ok");
    text("settingsConnectionDescription", "교사 계정으로 안전하게 연결되어 있습니다. 별도의 페어링 키가 필요하지 않습니다.");
  }

  if (!view.backupOk) {
    setBackupBadge("확인 필요", "error");
    text("settingsBackupDescription", "OneDrive 연결 또는 백업 폴더 권한을 확인해 주세요.");
  } else if (!view.backupConfigured) {
    setBackupBadge("설정 필요", "warning");
    text("settingsBackupDescription", "학교 OneDrive 안에 백업 폴더를 선택해 주세요.");
  } else {
    setBackupBadge("정상", "ok");
    text("settingsBackupDescription", "학교 계정 OneDrive에 자동 백업하고 있습니다.");
  }
}

function confirmDisconnect() {
  const dialog = element<HTMLDialogElement>("settingsDisconnectDialog");
  return new Promise<boolean>((resolve) => {
    const settle = () => {
      dialog.removeEventListener("close", settle);
      resolve(dialog.returnValue === "confirm");
    };
    dialog.addEventListener("close", settle);
    dialog.showModal();
  });
}

export function initSettingsDashboard(options: Options) {
  const startWithWindows = element<HTMLInputElement>("settingsStartWithWindows");
  const keepRunningOnClose = element<HTMLInputElement>("settingsKeepRunningOnClose");

  async function loadPreferences() {
    try {
      const result = await invoke<DesktopPreferences>("get_desktop_preferences");
      startWithWindows.checked = result.startWithWindows !== false;
      keepRunningOnClose.checked = result.keepRunningOnClose !== false;
      setPreferenceStatus("");
    } catch (error) {
      setPreferenceStatus(`앱 동작 설정을 확인하지 못했습니다: ${String((error as Error)?.message || error)}`, "error");
    }
  }

  async function savePreference(input: HTMLInputElement, key: "startWithWindows" | "keepRunningOnClose") {
    const previous = !input.checked;
    input.disabled = true;
    setPreferenceStatus("설정을 저장하고 있습니다.");
    try {
      const result = await invoke<DesktopPreferences>("set_desktop_preference", { key, enabled: input.checked });
      if (!result?.ok) throw new Error(result?.error || "desktop_preference_failed");
      startWithWindows.checked = result.startWithWindows !== false;
      keepRunningOnClose.checked = result.keepRunningOnClose !== false;
      setPreferenceStatus("앱 동작 설정을 저장했습니다.", "ok");
    } catch (error) {
      input.checked = previous;
      setPreferenceStatus(`설정을 저장하지 못했습니다: ${String((error as Error)?.message || error)}`, "error");
    } finally {
      input.disabled = false;
    }
  }

  startWithWindows.addEventListener("change", () => void savePreference(startWithWindows, "startWithWindows"));
  keepRunningOnClose.addEventListener("change", () => void savePreference(keepRunningOnClose, "keepRunningOnClose"));

  element<HTMLButtonElement>("settingsOpenTeacherButton").addEventListener("click", async () => {
    setPreferenceStatus("교사 설정을 열고 있습니다.");
    const result = await invoke<{ ok?: boolean; error?: string }>("open_teacher_data_security_settings")
      .catch((error) => ({ ok: false, error: String(error) }));
    setPreferenceStatus(result?.ok ? "브라우저에서 교사 설정을 열었습니다." : `교사 설정을 열지 못했습니다: ${result?.error || "open_failed"}`, result?.ok ? "ok" : "error");
  });

  element<HTMLButtonElement>("settingsDisconnectButton").addEventListener("click", async () => {
    if (!await confirmDisconnect()) return;
    const button = element<HTMLButtonElement>("settingsDisconnectButton");
    button.disabled = true;
    button.textContent = "연결 해제 중";
    setPreferenceStatus("로그인 연결을 안전하게 해제하고 있습니다.");
    try {
      const result = await invoke<DisconnectResult>("disconnect_local_store");
      if (!result?.ok || result.localDataPreserved !== true) throw new Error(result?.error || "disconnect_failed");
      await options.onDisconnected();
      setPreferenceStatus("연결을 해제했습니다. 이 PC의 저장 자료와 백업은 그대로 유지됩니다.", "ok");
    } catch (error) {
      setPreferenceStatus(`연결을 해제하지 못했습니다: ${String((error as Error)?.message || error)}`, "error");
    } finally {
      button.disabled = false;
      button.textContent = "이 PC 연결 해제";
    }
  });

  void loadPreferences();
  return Object.freeze({ loadPreferences });
}
