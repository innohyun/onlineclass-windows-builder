type HealthPreviewState = "normal" | "checking" | "service-error" | "reconnect" | "sync-error" | "backup-unconfigured" | "backup-error";
type HealthTone = "ok" | "warning" | "error" | "checking";

function byId<T extends HTMLElement>(id: string) {
  const element = document.getElementById(id);
  if (!element) throw new Error(`missing element: ${id}`);
  return element as T;
}

function text(id: string, value: string) {
  byId(id).textContent = value;
}

function badge(id: string, value: string, tone: Exclude<HealthTone, "checking"> | "warning") {
  const element = byId(id);
  element.textContent = value;
  element.className = `status-badge badge-${tone}`;
}

function tone(id: string, value: HealthTone) {
  const element = byId(id);
  element.classList.remove("is-ok", "is-warning", "is-error", "is-checking");
  element.classList.add(`is-${value}`);
  if (id === "summaryCard") {
    const icon = element.querySelector<HTMLElement>(".health-summary-icon i");
    if (icon) icon.className = value === "error"
      ? "fa-solid fa-triangle-exclamation"
      : value === "warning"
        ? "fa-solid fa-exclamation"
        : value === "checking"
          ? "fa-solid fa-rotate"
          : "fa-solid fa-check";
  }
}

function hidden(id: string, value: boolean) {
  byId(id).hidden = value;
}

function renderNormal() {
  tone("summaryCard", "ok");
  text("summaryTitle", "모든 기능이 정상입니다");
  text("summaryDescription", "민감기록 저장, 임시 기록 수거, 백업이 안전하게 작동하고 있습니다.");
  text("healthCheckedText", "오늘 오후 2:24");
  text("summaryTenantText", "수영초등학교 5학년 1반");
  text("summarySyncText", "오늘 오후 2:18");
  text("summaryBackupText", "어제 오후 5:58");
  text("summaryPendingText", "0건");

  tone("connectionCard", "ok");
  badge("connectionBadge", "정상", "ok");
  text("connectionTitle", "수영초등학교 5학년 1반 연결됨");
  text("connectionMetaText", "교사 로그인으로 안전하게 연결되어 있습니다.");
  text("connectionModeText", "웹 로그인 승인");
  text("connectionAccountText", "innohyun@suyeong.es.kr");
  text("connectionCheckText", "오늘 오전 11:24");
  text("healthConnectionAction", "교사 설정 열기");

  tone("syncCard", "ok");
  badge("syncBadge", "정상", "ok");
  text("cloudSyncText", "현재 가져올 임시 기록이 없습니다.");
  text("syncImportedCount", "14");
  text("syncPendingCount", "0");
  text("syncFailedCount", "0");
  text("syncServerCount", "14");
  text("cloudSyncMetaText", "오늘 오후 2:18");
  text("syncModeText", "이 PC에 직접 저장");
  text("syncStatus", "새 기록이 생기면 백그라운드에서 자동으로 수거합니다.");
  hidden("healthSyncSettingsAction", true);

  tone("healthBackupCard", "ok");
  badge("healthBackupBadge", "정상", "ok");
  text("healthBackupText", "학교 OneDrive에 자동 백업하고 있습니다.");
  text("healthBackupLatestText", "어제 오후 5:58");
  text("healthBackupNextText", "오늘 오후 5:58");
  text("healthBackupMediaText", "21개 · 누락 0개");
  hidden("healthBackupRunAction", false);
  hidden("healthBackupFolderAction", true);

  document.querySelectorAll<HTMLButtonElement>('.health-view button[data-action="refresh-status"], .health-view button[data-action="run-sync"], .health-view button[data-action="run-backup"], .health-view button[data-app-view-target="backup"]').forEach((button) => { button.disabled = false; });
}

function renderState(state: HealthPreviewState) {
  renderNormal();
  if (state === "normal") return;
  if (state === "checking") {
    tone("summaryCard", "checking");
    text("summaryTitle", "상태를 확인하고 있습니다");
    text("summaryDescription", "PC 연결, 자동 수거, 백업 상태를 차례로 확인합니다.");
    text("healthCheckedText", "확인 중");
    for (const id of ["connectionCard", "syncCard", "healthBackupCard"]) tone(id, "checking");
    badge("connectionBadge", "확인 중", "warning");
    badge("syncBadge", "확인 중", "warning");
    badge("healthBackupBadge", "확인 중", "warning");
    return;
  }
  if (state === "service-error") {
    tone("summaryCard", "error");
    text("summaryTitle", "로컬 앱 상태를 확인해야 합니다");
    text("summaryDescription", "DBHelper가 실행 중인지 확인한 뒤 상태 새로고침을 눌러 주세요.");
    for (const id of ["connectionCard", "syncCard", "healthBackupCard"]) tone(id, "error");
    badge("connectionBadge", "확인 불가", "error");
    badge("syncBadge", "확인 불가", "error");
    badge("healthBackupBadge", "확인 불가", "error");
    return;
  }
  if (state === "reconnect") {
    tone("summaryCard", "warning");
    text("summaryTitle", "PC 재연결이 필요합니다");
    text("summaryDescription", "교사 로그인 정보가 만료되어 자동 수거가 멈춰 있습니다.");
    tone("connectionCard", "warning");
    badge("connectionBadge", "재연결 필요", "warning");
    text("connectionMetaText", "브라우저에서 교사 로그인 후 이 PC 연결을 다시 승인해 주세요.");
    text("healthConnectionAction", "다시 연결하기");
    tone("syncCard", "warning");
    badge("syncBadge", "일시 중지", "warning");
    text("cloudSyncText", "PC 재연결 전까지 자동 수거가 멈춰 있습니다.");
    hidden("healthSyncSettingsAction", false);
    document.querySelectorAll<HTMLButtonElement>('.health-view button[data-action="run-sync"]').forEach((button) => { button.disabled = true; });
    return;
  }
  if (state === "sync-error") {
    tone("summaryCard", "error");
    text("summaryTitle", "자동 수거를 확인해 주세요");
    text("summaryDescription", "마지막 수거에서 실패하거나 충돌한 기록이 있습니다.");
    tone("syncCard", "error");
    badge("syncBadge", "확인 필요", "error");
    text("cloudSyncText", "마지막 수거에서 확인이 필요한 항목이 있습니다.");
    text("syncFailedCount", "2");
    text("syncStatus", "실패 1건 · 충돌 1건입니다. 다시 수거해 주세요.");
    return;
  }
  tone("summaryCard", state === "backup-error" ? "error" : "warning");
  tone("healthBackupCard", state === "backup-error" ? "error" : "warning");
  hidden("healthBackupRunAction", true);
  hidden("healthBackupFolderAction", false);
  if (state === "backup-unconfigured") {
    text("summaryTitle", "백업 폴더 설정이 필요합니다");
    text("summaryDescription", "로컬 저장과 자동 수거는 정상이며 백업 폴더만 선택하면 됩니다.");
    badge("healthBackupBadge", "설정 필요", "warning");
    text("healthBackupText", "학교 OneDrive 안에 백업 폴더를 선택해 주세요.");
    text("healthBackupLatestText", "아직 없음");
    text("healthBackupNextText", "폴더 설정 후 자동 실행");
    text("healthBackupMediaText", "-");
    text("healthBackupFolderAction", "백업 폴더 선택");
    return;
  }
  text("summaryTitle", "백업 폴더를 확인해 주세요");
  text("summaryDescription", "OneDrive 연결 또는 폴더 권한을 확인한 뒤 다시 시도하세요.");
  badge("healthBackupBadge", "확인 필요", "error");
  text("healthBackupText", "설정된 백업 폴더에 접근할 수 없습니다.");
  text("healthBackupMediaText", "첨부 누락 여부 확인 필요");
  text("healthBackupFolderAction", "백업 폴더 다시 선택");
}

export function initHealthDashboardPreview() {
  const state = (new URLSearchParams(window.location.search).get("healthState") || "normal") as HealthPreviewState;
  text("homeTenantLabel", "수영초등학교 5학년 1반");
  text("homeConnectionText", "연결됨");
  text("homeBackupText", "어제 오후 5:58");
  renderState(state);

  document.addEventListener("click", (event) => {
    const target = event.target as HTMLElement | null;
    if (!target?.closest(".health-view")) return;
    const connectionButton = target.closest<HTMLButtonElement>("#healthConnectionAction");
    const actionButton = target.closest<HTMLButtonElement>("button[data-action]");
    const viewButton = target.closest<HTMLButtonElement>('button[data-app-view-target="backup"]');
    if (viewButton) return;
    const action = connectionButton ? "health-connection" : actionButton?.dataset.action;
    if (!action) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    if (action === "refresh-status") {
      renderState(state);
      text("healthCheckedText", "방금");
    } else if (action === "run-sync") {
      text("cloudSyncText", "방금 수거를 완료했습니다.");
      text("cloudSyncMetaText", "방금");
      text("syncStatus", "가져올 새 기록이 없습니다.");
    } else if (action === "run-backup") {
      text("healthBackupText", "방금 백업을 안전하게 마쳤습니다.");
      text("healthBackupLatestText", "방금");
    } else if (action === "choose-backup-folder") {
      renderNormal();
      text("healthBackupText", "학교 OneDrive 백업 폴더를 연결했습니다.");
    } else if (action === "open-settings" || action === "health-connection") {
      text("connectionMetaText", "교사 설정 화면에서 연결 상태를 확인할 수 있습니다.");
    }
  }, true);
}
