import { renderSettingsDashboard } from "./settings-dashboard";

type SettingsPreviewState = "normal" | "disconnected" | "pending" | "error" | "backup-unconfigured";

function element<T extends HTMLElement>(id: string) {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element: ${id}`);
  return found as T;
}

function text(id: string, value: string) {
  element(id).textContent = value;
}

function showAuth(state: SettingsPreviewState) {
  element("settingsConnectedContent").hidden = true;
  const panel = element("deviceAuthPanel");
  panel.hidden = false;
  panel.dataset.state = state === "error" ? "error" : state === "pending" ? "pending" : "idle";
  element("deviceAuthWait").hidden = state !== "pending";
  element<HTMLButtonElement>("deviceAuthStart").hidden = state === "pending";
  element<HTMLButtonElement>("deviceAuthReopen").hidden = state !== "pending";
  if (state === "pending") {
    text("deviceAuthTitle", "브라우저 승인 대기 중");
    text("deviceAuthDescription", "열린 웹페이지에서 교사 로그인 후 이 PC 연결을 승인하세요.");
    text("deviceAuthMeta", "승인 요청은 10분 뒤 자동으로 만료됩니다.");
  } else if (state === "error") {
    text("deviceAuthTitle", "브라우저 연결을 시작하지 못했습니다");
    text("deviceAuthDescription", "인터넷 연결을 확인한 뒤 다시 시도해 주세요.");
    text("deviceAuthMeta", "페어링 키나 수동 코드는 필요하지 않습니다.");
  } else {
    text("deviceAuthTitle", "교사 로그인으로 이 PC 연결");
    text("deviceAuthDescription", "브라우저에서 ClassAimate 교사 계정으로 로그인하고 연결을 승인하세요. 페어링 키는 필요하지 않습니다.");
    text("deviceAuthMeta", "승인 요청은 10분 뒤 자동으로 만료됩니다.");
  }
}

export function initSettingsDashboardPreview() {
  const params = new URLSearchParams(window.location.search);
  const state = (params.get("settingsState") || "normal") as SettingsPreviewState;
  const normal = state === "normal" || state === "backup-unconfigured";

  text("homeTenantLabel", "수영초등학교 5학년 1반");
  text("homeConnectionText", normal ? "연결됨" : "연결 필요");
  text("homeBackupText", state === "backup-unconfigured" ? "아직 없음" : "어제 오후 5:58");

  if (normal) {
    renderSettingsDashboard({
      connected: true,
      tenantLabel: "수영초등학교 5학년 1반",
      accountLabel: "innohyun@suyeong.es.kr",
      backupConfigured: state !== "backup-unconfigured",
      backupOk: true,
      backupLocation: state === "backup-unconfigured" ? "아직 선택하지 않음" : "학교 OneDrive · OnlineClassLocalBackups",
      backupLatest: state === "backup-unconfigured" ? "아직 없음" : "어제 오후 5:58",
      appVersion: "0.2.26",
    });
  } else {
    showAuth(state);
    text("settingsAppVersionFooter", "앱 v0.2.26");
  }

  for (const id of ["settingsStartWithWindows", "settingsKeepRunningOnClose"]) {
    element<HTMLInputElement>(id).addEventListener("change", () => {
      text("settingsPreferenceStatus", "앱 동작 설정을 저장했습니다.");
      element("settingsPreferenceStatus").className = "settings-inline-status is-ok";
    });
  }

  element("settingsOpenTeacherButton").addEventListener("click", () => {
    showAuth("pending");
    text("settingsPreferenceStatus", "현재 브라우저 연결 승인을 시작했습니다.");
    element("settingsPreferenceStatus").className = "settings-inline-status";
  });

  element("settingsDisconnectButton").addEventListener("click", () => {
    element<HTMLDialogElement>("settingsDisconnectDialog").showModal();
  });
  element<HTMLDialogElement>("settingsDisconnectDialog").addEventListener("close", (event) => {
    const dialog = event.currentTarget as HTMLDialogElement;
    if (dialog.returnValue === "confirm") showAuth("disconnected");
  });

  document.querySelector<HTMLButtonElement>('[data-app-view="settings"] [data-action="choose-backup-folder"]')?.addEventListener("click", (event) => {
    event.stopImmediatePropagation();
    text("settingsBackupDescription", "백업 폴더 선택창을 열었습니다.");
  }, { capture: true });
  element("deviceAuthStart").addEventListener("click", (event) => {
    event.stopImmediatePropagation();
    showAuth("pending");
  }, { capture: true });
  element("deviceAuthReopen").addEventListener("click", (event) => {
    event.stopImmediatePropagation();
    text("deviceAuthMeta", "브라우저 승인 페이지를 다시 열었습니다.");
  }, { capture: true });
}
