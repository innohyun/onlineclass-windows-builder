import { invoke } from "@tauri-apps/api/core";
import { isDeskRestoreBlocked } from "./desk-restore-lock";

export type DesktopPreferences = {
  ok: boolean;
  startWithWindows: boolean;
  keepRunningOnClose: boolean;
  autostartError?: string | null;
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
  backupVerified?: boolean;
  backupLocation: string;
  backupLatest: string;
  appVersion: string;
};

type Options = {
  onDisconnected(): Promise<void>;
  onAuthorizeBrowser(): Promise<void>;
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

export function isMacDesktop(platform = navigator.platform) {
  return /mac/i.test(platform);
}

export function renderSettingsPlatform(platform = navigator.platform) {
  const mac = isMacDesktop(platform);
  text("settingsAutostartLabel", mac ? "Mac 로그인 시 자동 실행" : "Windows 시작 시 자동 실행");
  text("settingsAutostartDescription", mac
    ? "기본은 꺼짐입니다. 켜면 이 사용자 계정의 로그인 항목에 등록합니다."
    : "PC를 켜면 자동 수거와 백업을 준비합니다.");
  text("settingsCloseDescription", mac
    ? "창을 닫아도 백그라운드에서 실행하며 메뉴 막대에서 다시 열 수 있습니다."
    : "창을 닫아도 백그라운드에서 자동 수거와 백업을 이어갑니다.");
  text("settingsCredentialStorageText", mac
    ? "교사 계정 인증정보는 macOS 키체인에 안전하게 보관"
    : "교사 계정 정보는 Windows에서 암호화 보관");
}

export function renderSettingsDashboard(view: SettingsDashboardView, platform = navigator.platform) {
  mountSettingsLayout();
  const mac = isMacDesktop(platform);
  element("settingsConnectedContent").hidden = !view.connected;
  const authPanel = element("deviceAuthPanel");
  authPanel.hidden = view.connected && !(authPanel.dataset.state === "connected" && authPanel.dataset.completeDismissed !== "true");
  text("settingsTenantText", view.tenantLabel || "연결된 학급 없음");
  text("settingsAccountText", view.accountLabel || "-");
  text("settingsBackupLocationText", view.backupLocation || "-");
  text("settingsBackupLatestText", view.backupLatest || "-");
  text("settingsAppVersionFooter", view.appVersion ? `설치 버전 ${view.appVersion}` : "설치 버전 확인 전");

  if (!view.connected) {
    setBadge("PC 연결 필요", "warning");
    text("settingsConnectionDescription", "교사 로그인으로 이 PC를 연결하세요. 기존 자료는 유지됩니다.");
  } else if (view.needsReconnect) {
    setBadge("재연결 필요", "warning");
    text("settingsConnectionDescription", "브라우저 로그인 정보가 만료되었습니다. 교사 로그인으로 다시 연결해 주세요.");
  } else {
    setBadge("정상 연결", "ok");
    text("settingsConnectionDescription", "이 PC는 교사 계정과 연결되어 있습니다. 웹에서 자료를 보려면 현재 브라우저를 승인하세요.");
  }

  if (!view.backupOk) {
    setBackupBadge("확인 필요", "error");
    text("settingsBackupDescription", "백업과 기기 간 동기화를 보류합니다. 로컬 자료는 계속 사용할 수 있습니다. 폴더 연결과 접근 권한을 확인해 주세요.");
  } else if (!view.backupConfigured) {
    setBackupBadge("설정 필요", "warning");
    text("settingsBackupDescription", mac ? "이 Mac에서 사용할 백업 폴더를 선택해 주세요. OneDrive 설치는 필수가 아닙니다." : "학교 OneDrive 안에 백업 폴더를 선택해 주세요.");
  } else if (view.backupVerified === false) {
    setBackupBadge("첫 백업 필요", "warning");
    text("settingsBackupDescription", "폴더 설정은 완료되었습니다. 첫 백업을 만든 뒤 결과를 확인해 주세요.");
  } else {
    setBackupBadge("정상", "ok");
    text("settingsBackupDescription", mac ? "선택한 폴더에 자동 백업하고 있습니다." : "학교 계정 OneDrive에 자동 백업하고 있습니다.");
  }
}

function mountSettingsLayout() {
  const content = element("settingsConnectedContent");
  let grid = document.getElementById("settingsDashboardGrid");
  if (!grid) { grid = document.createElement("div"); grid.id = "settingsDashboardGrid"; grid.className = "settings-dashboard-grid"; content.prepend(grid); }
  const panels = [document.getElementById("settingsConnectionPanel"), document.getElementById("settingsBehaviorTitle")?.closest(".settings-wide-panel"), document.getElementById("settingsBackupPanel"), document.getElementById("deskHideHomeTitles")?.closest("section")];
  for (const panel of panels) if (panel && panel.parentElement !== grid) grid.append(panel);
  const oldGrid = content.querySelector(".settings-primary-grid");
  if (oldGrid && !oldGrid.children.length) oldGrid.remove();
  const dialog = element<HTMLDialogElement>("settingsDisconnectDialog");
  if (!dialog.querySelector(".settings-reconnect-note")) {
    const note = document.createElement("p"); note.className = "settings-reconnect-note";
    note.textContent = "다시 사용하려면 교사 로그인으로 이 PC를 연결하세요. 연결 해제는 자료 삭제가 아닙니다.";
    dialog.querySelector("aside")?.after(note);
  }
}

function confirmDisconnect() {
  const dialog = element<HTMLDialogElement>("settingsDisconnectDialog");
  dialog.returnValue = "";
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
  mountSettingsLayout();
  renderSettingsPlatform();
  const startWithWindows = element<HTMLInputElement>("settingsStartWithWindows");
  const keepRunningOnClose = element<HTMLInputElement>("settingsKeepRunningOnClose");
  let autostartUnverified = false;

  function lockPreferences(locked: boolean) {
    startWithWindows.disabled = locked;
    keepRunningOnClose.disabled = locked;
  }

  function renderPreferences(result: DesktopPreferences) {
    if (!result?.ok || typeof result.startWithWindows !== "boolean" || typeof result.keepRunningOnClose !== "boolean") {
      throw new Error(result?.error || "desktop_preferences_invalid_response");
    }
    startWithWindows.checked = result.startWithWindows;
    keepRunningOnClose.checked = result.keepRunningOnClose;
    autostartUnverified = Boolean(result.autostartError);
    startWithWindows.indeterminate = autostartUnverified;
    setPreferenceStatus(result.autostartError
      ? `자동 실행 등록을 확인하지 못했습니다. 스위치를 다시 설정해 주세요: ${result.autostartError}`
      : "", result.autostartError ? "error" : "neutral");
  }

  async function loadPreferences() {
    lockPreferences(true);
    startWithWindows.indeterminate = true;
    keepRunningOnClose.indeterminate = true;
    setPreferenceStatus("앱 동작 설정을 확인하고 있습니다.");
    try {
      const result = await invoke<DesktopPreferences>("get_desktop_preferences");
      renderPreferences(result);
      keepRunningOnClose.indeterminate = false;
      lockPreferences(false);
    } catch (error) {
      setPreferenceStatus(`앱 동작 설정을 확인하지 못했습니다: ${String((error as Error)?.message || error)}`, "error");
    }
  }

  async function savePreference(input: HTMLInputElement, key: "startWithWindows" | "keepRunningOnClose") {
    const previous = !input.checked;
    lockPreferences(true);
    setPreferenceStatus("설정을 저장하고 있습니다.");
    try {
      const result = await invoke<DesktopPreferences>("set_desktop_preference", { key, enabled: input.checked });
      renderPreferences(result);
      if (!result.autostartError) setPreferenceStatus("앱 동작 설정을 저장했습니다.", "ok");
    } catch (error) {
      input.checked = previous;
      if (key === "startWithWindows") autostartUnverified = true;
      startWithWindows.indeterminate = autostartUnverified;
      setPreferenceStatus(`설정을 저장하지 못했습니다: ${String((error as Error)?.message || error)}`, "error");
    } finally {
      lockPreferences(false);
    }
  }

  startWithWindows.addEventListener("change", () => void savePreference(startWithWindows, "startWithWindows"));
  keepRunningOnClose.addEventListener("change", () => void savePreference(keepRunningOnClose, "keepRunningOnClose"));

  element<HTMLButtonElement>("settingsOpenTeacherButton").addEventListener("click", async () => {
    if (isDeskRestoreBlocked()) { setPreferenceStatus("복원 작업을 마칠 때까지 연결 승인을 시작할 수 없습니다.", "error"); return; }
    const button = element<HTMLButtonElement>("settingsOpenTeacherButton");
    button.disabled = true;
    setPreferenceStatus("현재 브라우저 연결 승인을 시작하고 있습니다.");
    try {
      await options.onAuthorizeBrowser();
    } finally {
      button.disabled = isDeskRestoreBlocked();
    }
  });

  element<HTMLButtonElement>("settingsDisconnectButton").addEventListener("click", async () => {
    if (isDeskRestoreBlocked()) { setPreferenceStatus("복원 작업을 마칠 때까지 PC 연결을 해제할 수 없습니다.", "error"); return; }
    if (!await confirmDisconnect()) return;
    if (isDeskRestoreBlocked()) { setPreferenceStatus("복원 작업 중에는 PC 연결을 유지합니다.", "error"); return; }
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
      button.disabled = isDeskRestoreBlocked();
      button.textContent = "이 PC 연결 해제";
    }
  });

  window.addEventListener("desk:restore-lock-changed", () => {
    for (const id of ["settingsOpenTeacherButton", "settingsDisconnectButton"]) element<HTMLButtonElement>(id).disabled = isDeskRestoreBlocked();
  });

  void loadPreferences();
  return Object.freeze({ loadPreferences });
}
