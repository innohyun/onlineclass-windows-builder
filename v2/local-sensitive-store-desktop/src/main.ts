import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import "./vendor/fontawesome/css/fontawesome.min.css";
import "./vendor/fontawesome/css/solid.min.css";
import "./styles.css";
import "./home-dashboard.css";
import "./data-explorer.css";
import "./student-timeline.css";
import "./backup-restore.css";
import "./shared-archive.css";
import "./archive-board-explorer.css";
import "./work-note-reader.css";
import "./device-sync-conflicts.css";
import "./local-reader-tutorial.css";
import "./health-dashboard.css";
import "./settings-dashboard.css";
import "./desktop-shell.css";
import { initSharedArchive } from "./shared-archive";
import { initHomeDashboard, loadHomeOverview, renderHomeStatus } from "./home-dashboard";
import { createDeviceAuthorizationController, type DeviceAuthorizationResult } from "./device-authorization";
import { initDataExplorer } from "./data-explorer";
import { initStudentTimeline } from "./student-timeline";
import { confirmBackupRestore } from "./backup-restore-confirmation";
import { initBackupRestorePreview } from "./backup-restore-preview";
import { initSharedArchivePreview } from "./shared-archive-preview";
import { initArchiveBoardExplorer } from "./archive-board-explorer";
import { initWorkNoteReader } from "./work-note-reader";
import { initDeviceSyncConflicts } from "./device-sync-conflicts";
import { initHealthDashboardPreview } from "./health-dashboard-preview";
import { initSettingsDashboard, renderSettingsDashboard } from "./settings-dashboard";
import { initSettingsDashboardPreview } from "./settings-dashboard-preview";
import { loadDeviceSyncStatus, renderDeviceSyncStatus, runDeviceSyncNow } from "./device-sync-ui";
import { initDesktopShell } from "./desktop-shell";
import type { BackupDiscovery, BackupItem, BackupPreview, BackupSource, BackupStatus, CommandResult } from "./backup-types";

declare const __APP_VERSION__: string;

const APP_VERSION = String(__APP_VERSION__ || "").trim() || "0.0.0";
const designPreview = new URLSearchParams(window.location.search).get("designPreview");

type ServiceStatus = {
  ok: boolean;
  service: string;
  version: string;
  pcName?: string;
  os?: string;
  arch?: string;
  host: string;
  port: number;
  endpoint: string;
  dataDir: string;
  dbPath: string;
  keyPath: string;
  pairingKey: string;
  error?: string;
};

type CloudSyncStatus = {
  ok: boolean;
  connected: boolean;
  tenantId?: string;
  uid?: string;
  accountEmail?: string;
  accountDisplayName?: string;
  tenantName?: string;
  observationStorageMode?: string;
  lastRunAtMs?: number;
  lastSyncAtMs?: number;
  lastImported?: number;
  lastDeleted?: number;
  lastMarked?: number;
  lastPending?: number;
  lastFailed?: number;
  lastConflicts?: number;
  lastError?: string;
  lastErrorCode?: string;
  credentialMissing?: boolean;
  credentialStorage?: string;
  needsReconnect?: boolean;
  reconnectMessage?: string;
};

type DeviceConnectionStatus = DeviceAuthorizationResult & {
  connected?: boolean;
  uid?: string;
  connectedAtMs?: number;
};

type BadgeTone = "ok" | "warning" | "error" | "neutral";
type ActionName = "open-settings" | "open-data-directory" | "refresh-status" | "run-sync" | "run-device-sync" | "repair-device-sync" | "run-backup" | "choose-backup-folder" | "restore-backup";

let serviceSnapshot: ServiceStatus | null = null;
let serviceLoadError = "";
let cloudSyncSnapshot: CloudSyncStatus | null = null;
let deviceConnectionSnapshot: DeviceConnectionStatus | null = null;
let cloudSyncLoadError = "";
let backupSnapshot: BackupStatus | null = null;
let backupLoadError = "";
let backupList: BackupItem[] = [];
let selectedBackupManifestPath = "";
let backupPreview: BackupPreview | null = null;
let backupRestoreMessage = "";
let backupRestoreTone: BadgeTone = "neutral";
const busyActions = new Set<ActionName>();
let sharedArchive = { refresh: async () => undefined as void };

function byId<T extends HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`missing element: ${id}`);
  return el as T;
}

function setText(id: string, value: string) {
  byId(id).textContent = value || "-";
}

function renderAppVersion() {
  setText("appVersionBadge", `앱 v${APP_VERSION}`);
  setText("appVersionText", APP_VERSION);
}

function setBadge(id: string, label: string, tone: BadgeTone) {
  const el = byId<HTMLSpanElement>(id);
  el.textContent = label;
  el.className = `status-badge badge-${tone}`;
}

function setHealthPanelState(id: "connectionCard" | "syncCard" | "healthBackupCard", tone: "ok" | "warning" | "error" | "checking") {
  const panel = byId<HTMLElement>(id);
  panel.classList.remove("is-ok", "is-warning", "is-error", "is-checking");
  panel.classList.add(`is-${tone}`);
}

function setHidden(id: string, hidden: boolean) {
  byId(id).hidden = hidden;
}

function numberText(value?: number) {
  return String(Number(value || 0) || 0);
}

function numeric(value?: number) {
  return Number(value || 0) || 0;
}

function actionButtons(action: ActionName) {
  return Array.from(document.querySelectorAll<HTMLButtonElement>(`button[data-action="${action}"]`));
}

function setBackupFolderActionLabels(label: string) {
  actionButtons("choose-backup-folder").forEach((button) => {
    button.textContent = button.closest(".backup-restore-actions") ? "백업 폴더에서 다시 찾기" : label;
  });
}

function refreshActionStates() {
  const syncUnavailable = !cloudSyncSnapshot?.connected || isCredentialMissing(cloudSyncSnapshot);
  const backupUnavailable = !backupSnapshot?.configured || !currentBackupTenantId();
  const restoreUnavailable = !selectedBackupManifestPath || !currentBackupTenantId();
  const disabledByAction: Partial<Record<ActionName, boolean>> = {
    "run-sync": syncUnavailable,
    "run-backup": backupUnavailable,
    "restore-backup": restoreUnavailable,
  };
  (["open-settings", "open-data-directory", "refresh-status", "run-sync", "run-backup", "choose-backup-folder", "restore-backup"] as ActionName[]).forEach((action) => {
    actionButtons(action).forEach((button) => {
      button.disabled = busyActions.has(action) || Boolean(disabledByAction[action]);
    });
  });
}

function setActionBusy(action: ActionName, busy: boolean) {
  if (busy) busyActions.add(action);
  else busyActions.delete(action);
  refreshActionStates();
}

function waitForPaint() {
  return new Promise<void>((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
  });
}

async function copyText(value: string) {
  const trimmed = String(value || "").trim();
  if (!trimmed || trimmed === "-") return false;
  try {
    await navigator.clipboard.writeText(trimmed);
    return true;
  } catch (_) {
    return false;
  }
}

function copyTargetValue(id: string) {
  const target = byId<HTMLElement>(id);
  if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) {
    return target.value;
  }
  return target.textContent || "";
}

function formatDateTime(ms?: number) {
  const value = Number(ms || 0) || 0;
  if (!value) return "-";
  const date = new Date(value);
  const now = new Date();
  const startOfDay = (input: Date) => new Date(input.getFullYear(), input.getMonth(), input.getDate()).getTime();
  const dayDiff = Math.round((startOfDay(now) - startOfDay(date)) / 86400000);
  const hour = date.getHours();
  const minute = String(date.getMinutes()).padStart(2, "0");
  const timeLabel = `${hour < 12 ? "오전" : "오후"} ${hour % 12 || 12}:${minute}`;
  if (dayDiff === 0) return `오늘 ${timeLabel}`;
  if (dayDiff === 1) return `어제 ${timeLabel}`;
  return `${date.toLocaleDateString("ko-KR")} ${timeLabel}`;
}

function formatBackupDateTime(ms?: number) {
  const value = Number(ms || 0) || 0;
  if (!value) return "-";
  const date = new Date(value);
  const hour = date.getHours();
  const minute = String(date.getMinutes()).padStart(2, "0");
  return `${date.getFullYear()}년 ${date.getMonth() + 1}월 ${date.getDate()}일 ${hour < 12 ? "오전" : "오후"} ${hour % 12 || 12}:${minute}`;
}

function currentBackupTenantId() {
  return byId<HTMLInputElement>("backupTenantInput").value.trim();
}

function tenantLabel(status?: Pick<CloudSyncStatus, "tenantName" | "tenantId"> | null) {
  return status?.tenantName || status?.tenantId || "연결된 학급 없음";
}

function accountLabel(status?: Pick<CloudSyncStatus, "accountEmail" | "accountDisplayName" | "uid"> | null) {
  return status?.accountEmail || status?.accountDisplayName || status?.uid || "-";
}

function normalizeStorageMode(value?: string) {
  const mode = String(value || "").trim();
  if (mode === "hybrid_firestore_local_keep_remote") return mode;
  if (mode === "hybrid_firestore_local") return mode;
  if (mode === "local_sqlite") return mode;
  if (mode === "firestore") return mode;
  return "";
}

function cloudSyncModeLabel(value?: string) {
  const mode = normalizeStorageMode(value);
  if (mode === "hybrid_firestore_local_keep_remote") return "PC로 옮기고 서버에는 처리완료 표시";
  if (mode === "hybrid_firestore_local") return "PC로 옮긴 뒤 서버 임시본 삭제";
  if (mode === "local_sqlite") return "이 PC에 직접 저장";
  if (mode === "firestore") return "서버에만 저장";
  return "저장 방식 확인 중";
}

function isCredentialMissing(status?: CloudSyncStatus | null) {
  return status?.credentialMissing === true
    || status?.needsReconnect === true
    || status?.lastErrorCode === "credential_missing"
    || String(status?.lastError || "").startsWith("keyring_get_failed:");
}

function reconnectMessage(status?: CloudSyncStatus | null) {
  return status?.reconnectMessage
    || "브라우저 로그인 정보가 만료되어 자동 수거가 멈춰 있습니다. 아래 다시 연결하기를 누르면 교사 설정 화면으로 이동합니다.";
}

function credentialStorageLabel(value?: string) {
  const storage = String(value || "");
  if (storage.includes("windows_dpapi_file")) return "자동 연결(암호화 보관)";
  if (storage.includes("macos_file")) return "자동 연결(로컬 보조 보관)";
  return "자동 연결";
}

function latestSyncTime(status?: CloudSyncStatus | null) {
  return status?.lastSyncAtMs || status?.lastRunAtMs || 0;
}

function latestBackupTime(status?: BackupStatus | null) {
  return status?.latestBackup?.createdAtMs || status?.lastRunAtMs || 0;
}

function countFrom(counts: Record<string, number> | undefined, keys: string[]) {
  if (!counts) return 0;
  for (const key of keys) {
    const value = Number(counts[key] || 0) || 0;
    if (value) return value;
  }
  return 0;
}

function backupObservationCount(counts?: Record<string, number>) {
  return countFrom(counts, ["lesson_observations", "observationCount"]);
}

function backupPrivateDetailCount(counts?: Record<string, number>) {
  return countFrom(counts, ["student_private_details", "studentPrivateDetailCount"]);
}

function backupCounselingCount(counts?: Record<string, number>) {
  return countFrom(counts, ["teacher_counseling_sessions", "teacherCounselingSessionCount"]);
}

function backupCareCount(counts?: Record<string, number>) {
  return backupObservationCount(counts) + backupCounselingCount(counts) + backupPrivateDetailCount(counts);
}

function backupMathDailyCount(counts?: Record<string, number>) {
  return [
    ["math_daily_attempts", "mathDailyAttemptCount"],
    ["math_daily_student_profiles", "mathDailyProfileCount"],
    ["math_daily_review_sessions", "mathDailyReviewSessionCount"],
    ["math_daily_assignments", "mathDailyAssignmentCount"],
    ["math_daily_assignment_results", "mathDailyAssignmentResultCount"],
    ["math_daily_cache_runs", "mathDailyCacheRunCount"],
  ].reduce((sum, keys) => sum + countFrom(counts, keys), 0);
}

function backupBoardSnapshotCount(counts?: Record<string, number>) {
  return countFrom(counts, ["board_post_snapshots", "boardSnapshotCount"]);
}

function backupBoardMediaCount(counts?: Record<string, number>, media?: BackupItem["media"] | BackupPreview["media"]) {
  const count = countFrom(counts, ["board_media_files", "boardMediaCount"]);
  if (count) return count;
  const records = Array.isArray(media?.records) ? media.records.length : 0;
  return records;
}

function backupArchiveCount(counts?: Record<string, number>) {
  return countFrom(counts, ["sharedArchiveCount"]);
}

function backupAttendanceCount(counts?: Record<string, number>) {
  return [
    ["attendance_records", "attendanceRecordCount"],
    ["attendance_nais_checks", "attendanceNaisCheckCount"],
    ["attendance_document_requests", "attendanceDocumentRequestCount"],
  ].reduce((sum, keys) => sum + countFrom(counts, keys), 0);
}

function backupEvalCount(counts?: Record<string, number>) {
  return [
    ["eval_assignments", "evalAssignmentCount"],
    ["eval_results", "evalResultCount"],
  ].reduce((sum, keys) => sum + countFrom(counts, keys), 0);
}

function backupStudentRecordCount(counts?: Record<string, number>) {
  return [
    ["student_record_draft_sets", "studentRecordDraftSetCount"],
    ["student_record_drafts", "studentRecordDraftCount"],
  ].reduce((sum, keys) => sum + countFrom(counts, keys), 0);
}

function backupLearningCount(counts?: Record<string, number>) {
  return backupMathDailyCount(counts) + backupEvalCount(counts);
}

function backupSourcePcName(source?: BackupSource) {
  return String(source?.pcName || "").trim() || "PC 정보 없음";
}

function backupSourceRelation(source?: BackupSource) {
  const sourcePc = String(source?.pcName || "").trim().toLowerCase();
  const currentPc = String(serviceSnapshot?.pcName || "").trim().toLowerCase();
  if (!sourcePc) return "PC 정보 없음";
  if (currentPc && sourcePc === currentPc) return "이 PC";
  return "다른 PC";
}

function backupEnvironmentText(source?: BackupSource) {
  const parts: string[] = [];
  const appVersion = String(source?.appVersion || "").trim();
  const serviceVersion = String(source?.serviceVersion || "").trim();
  const os = String(source?.os || "").trim();
  const arch = String(source?.arch || "").trim();
  if (appVersion) parts.push(`앱 v${appVersion}`);
  if (serviceVersion) parts.push(`서비스 ${serviceVersion}`);
  if (os || arch) parts.push([os, arch].filter(Boolean).join(" "));
  return parts.join(" · ") || "환경 정보 없음";
}

function backupSourceSummary(source?: BackupSource) {
  return `${backupSourceRelation(source)} · ${backupSourcePcName(source)} · ${backupEnvironmentText(source)}`;
}

function backupSourceListText(source?: BackupSource) {
  const os = String(source?.os || "").trim() || "운영체제 정보 없음";
  return `${backupSourceRelation(source)} · ${backupSourcePcName(source)} · ${os}`;
}

function backupFolderLabel(status: BackupStatus) {
  if (!status.configured) return "-";
  const folder = String(status.tenantBackupDir || status.backupRootDir || "").toLowerCase();
  if (folder.includes("onedrive")) return "학교 OneDrive · OnlineClassLocalBackups";
  if (folder.includes("google drive")) return "Google Drive · OnlineClassLocalBackups";
  if (folder.includes("dropbox")) return "Dropbox · OnlineClassLocalBackups";
  if (folder.includes("icloud")) return "iCloud Drive · OnlineClassLocalBackups";
  return "선택한 백업 폴더 · OnlineClassLocalBackups";
}

function backupRowSummary(backup: BackupItem) {
  const counts = backup.counts || {};
  const media = backup.media || {};
  const parts = [
    `관찰·상담 ${numberText(backupCareCount(counts))}건`,
    `출결·증빙 ${numberText(backupAttendanceCount(counts))}건`,
    `평가·학습 ${numberText(backupLearningCount(counts))}건`,
    `학생부 ${numberText(backupStudentRecordCount(counts))}건`,
    `게시판 ${numberText(backupBoardSnapshotCount(counts))}건`,
    `첨부 ${numberText(backupBoardMediaCount(counts, media))}개`,
    `보관본 ${numberText(backupArchiveCount(counts))}개`,
  ];
  return parts.join(" · ");
}

function normalizeBackupList(items: unknown): BackupItem[] {
  if (!Array.isArray(items)) return [];
  const seen = new Set<string>();
  const out: BackupItem[] = [];
  for (const item of items) {
    const backup = item as BackupItem;
    const manifestPath = String(backup?.manifestPath || "").trim();
    if (!manifestPath || seen.has(manifestPath)) continue;
    seen.add(manifestPath);
    out.push(backup);
  }
  return out.sort((a, b) => numeric(b.createdAtMs) - numeric(a.createdAtMs));
}

function escapeHtml(value: unknown) {
  return String(value || "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function setBackupRestoreMessage(message: string, tone: BadgeTone = "neutral") {
  backupRestoreMessage = message;
  backupRestoreTone = tone;
}

function renderSummary() {
  const summaryCard = byId<HTMLElement>("summaryCard");
  const pending = numeric(cloudSyncSnapshot?.lastPending);
  const failed = numeric(cloudSyncSnapshot?.lastFailed) + numeric(cloudSyncSnapshot?.lastConflicts);
  const backupMedia = backupSnapshot?.latestBackup?.media || backupSnapshot?.lastResult?.media;
  const backupHasError = backupSnapshot?.ok === false
    || numeric(backupMedia?.failed) > 0
    || numeric(backupMedia?.missing) > 0
    || Boolean(backupLoadError);
  const backupConfigured = backupSnapshot?.configured === true;

  let tone: "is-ok" | "is-warning" | "is-error" | "is-checking" = "is-checking";
  let title = "상태 확인 중";
  let description = "로컬 저장소, 자동 수거, 백업 상태를 확인하고 있습니다.";

  if (serviceLoadError || serviceSnapshot?.ok === false) {
    tone = "is-error";
    title = "로컬 앱 상태를 확인해야 합니다.";
    description = "PC 설치본 DBHelper가 정상 실행 중인지 확인한 뒤 상태 확인을 눌러 주세요.";
  } else if ((!cloudSyncSnapshot && !deviceConnectionSnapshot && !cloudSyncLoadError) || (!backupSnapshot && !backupLoadError)) {
    tone = "is-checking";
  } else if (!deviceConnectionSnapshot?.connected && (!cloudSyncSnapshot?.connected || isCredentialMissing(cloudSyncSnapshot))) {
    tone = "is-warning";
    title = "재연결이 필요합니다.";
    description = "브라우저 로그인 정보가 만료되어 자동 수거가 멈춰 있습니다. 다시 연결하면 수거가 재개됩니다.";
  } else if (cloudSyncLoadError || failed || backupHasError) {
    tone = "is-error";
    title = "확인이 필요한 문제가 있습니다.";
    description = "아래 카드의 실패 항목과 해결 안내를 확인하세요.";
  } else if (!backupConfigured) {
    tone = "is-warning";
    title = "백업 폴더 설정이 필요합니다.";
    description = "기록 저장과 자동 수거는 가능하지만 클라우드 폴더 백업은 아직 설정되지 않았습니다.";
  } else {
    tone = "is-ok";
    title = "모든 기능이 정상입니다";
    description = "민감기록 저장, 임시 기록 수거, 백업이 안전하게 작동하고 있습니다.";
  }

  summaryCard.className = `health-summary ${tone}`;
  setText("summaryTitle", title);
  setText("summaryDescription", description);
  setText("summaryTenantText", tenantLabel(deviceConnectionSnapshot?.connected ? deviceConnectionSnapshot : cloudSyncSnapshot));
  setText("summarySyncText", formatDateTime(latestSyncTime(cloudSyncSnapshot)));
  setText("summaryBackupText", formatDateTime(latestBackupTime(backupSnapshot)));
  setText("summaryPendingText", `${numberText(pending)}건`);
  setText("healthCheckedText", tone === "is-checking" ? "확인 중" : formatDateTime(Date.now()));
  const summaryIcon = summaryCard.querySelector<HTMLElement>(".health-summary-icon i");
  if (summaryIcon) {
    summaryIcon.className = tone === "is-error"
      ? "fa-solid fa-triangle-exclamation"
      : tone === "is-warning"
        ? "fa-solid fa-exclamation"
        : tone === "is-checking"
          ? "fa-solid fa-rotate"
          : "fa-solid fa-check";
  }
  renderHomeStatus({
    connected: deviceConnectionSnapshot?.connected === true || (cloudSyncSnapshot?.connected === true && !isCredentialMissing(cloudSyncSnapshot)),
    healthy: tone === "is-ok" || (tone === "is-warning" && backupSnapshot?.configured !== true),
    storeReady: serviceSnapshot?.ok === true,
    tenantLabel: tenantLabel(deviceConnectionSnapshot?.connected ? deviceConnectionSnapshot : cloudSyncSnapshot),
    syncAtMs: latestSyncTime(cloudSyncSnapshot),
    backupAtMs: latestBackupTime(backupSnapshot),
    pending,
  });
  const connection = deviceConnectionSnapshot?.connected ? deviceConnectionSnapshot : cloudSyncSnapshot;
  const connected = deviceConnectionSnapshot?.connected === true || cloudSyncSnapshot?.connected === true;
  renderSettingsDashboard({
    connected,
    needsReconnect: isCredentialMissing(cloudSyncSnapshot),
    tenantLabel: tenantLabel(connection),
    accountLabel: accountLabel(connection),
    backupConfigured: backupSnapshot?.configured === true,
    backupOk: !backupLoadError && backupSnapshot?.ok !== false,
    backupLocation: backupSnapshot ? backupFolderLabel(backupSnapshot) : "확인 중",
    backupLatest: backupSnapshot ? formatDateTime(latestBackupTime(backupSnapshot)) : "확인 중",
    appVersion: APP_VERSION,
  });
}

async function loadStatus() {
  serviceLoadError = "";
  const status = await invoke<ServiceStatus>("get_service_status");
  serviceSnapshot = status;
  const statusDot = byId<HTMLSpanElement>("statusDot");

  statusDot.classList.toggle("is-ok", status.ok);
  statusDot.classList.toggle("is-error", !status.ok);
  setText("statusText", status.ok ? "실행 중" : `시작 실패: ${status.error || "unknown"}`);
  setText("endpointText", status.endpoint);
  setText("dbPathText", status.dbPath);
  setText("dataDirText", status.dataDir);
  setText("serviceVersionText", status.version);
  setText("servicePortText", status.port ? String(status.port) : "-");
  renderSummary();
}

async function loadDeviceConnectionStatus() {
  const result = await invoke<DeviceConnectionStatus>("get_device_connection_status");
  deviceConnectionSnapshot = result;
  if (result.connected) {
    const tenantInput = byId<HTMLInputElement>("backupTenantInput");
    if (!tenantInput.value.trim() && result.tenantId) tenantInput.value = result.tenantId;
    deviceAuthorization.render({ ...result, status: "connected" });
    setBadge("connectionBadge", "정상", "ok");
    setText("connectionTitle", `${result.tenantName || result.tenantId || "학급"} 연결됨`);
    setText("connectionMetaText", "교사 로그인으로 승인된 브라우저가 이 PC의 로컬 저장소를 안전하게 사용합니다.");
    setText("connectionModeText", "웹 로그인 승인");
    setText("connectionAccountText", result.accountEmail || result.accountDisplayName || "교사 계정");
    setText("connectionCheckText", formatDateTime(result.connectedAtMs));
    setHealthPanelState("connectionCard", "ok");
    setText("healthConnectionAction", "교사 설정 열기");
  }
  renderSummary();
}

function renderServiceLoadError(error: unknown) {
  serviceLoadError = String((error as Error)?.message || error || "status_failed");
  const statusDot = byId<HTMLSpanElement>("statusDot");
  statusDot.classList.remove("is-ok");
  statusDot.classList.add("is-error");
  setText("statusText", `상태 조회 실패: ${serviceLoadError}`);
  renderSummary();
}

function renderConnectionStatus(status?: CloudSyncStatus | null) {
  if (!status?.connected) {
    setHealthPanelState("connectionCard", "warning");
    setBadge("connectionBadge", "연결 전", "warning");
    setText("connectionTitle", "교사 설정 연결이 필요합니다.");
    setText("connectionMetaText", "교사 설정 화면에서 이 PC 자동 연결을 실행하면 수거와 백업 학급 정보가 연결됩니다.");
    setText("connectionModeText", "자동 연결 대기");
    setText("connectionAccountText", "-");
    setText("connectionCheckText", "-");
    setText("healthConnectionAction", "PC 연결하기");
    return;
  }

  const tenant = tenantLabel(status);
  if (isCredentialMissing(status)) {
    setHealthPanelState("connectionCard", "warning");
    setBadge("connectionBadge", "재연결 필요", "warning");
    setText("connectionTitle", `${tenant} 연결을 다시 해야 합니다.`);
    setText("connectionMetaText", reconnectMessage(status));
    setText("connectionModeText", "자동 연결 만료");
    setText("connectionAccountText", accountLabel(status));
    setText("connectionCheckText", formatDateTime(latestSyncTime(status)));
    setText("healthConnectionAction", "다시 연결하기");
    return;
  }

  setHealthPanelState("connectionCard", "ok");
  setBadge("connectionBadge", "정상", "ok");
  setText("connectionTitle", "정상 작동 중입니다.");
  setText("connectionMetaText", "이 PC에서 민감기록을 저장하고, 임시 기록을 자동 수거합니다.");
  setText("connectionModeText", credentialStorageLabel(status.credentialStorage));
  setText("connectionAccountText", accountLabel(status));
  setText("connectionCheckText", formatDateTime(latestSyncTime(status)));
  setText("healthConnectionAction", "교사 설정 열기");
}

function renderCloudSync(status: CloudSyncStatus | null) {
  cloudSyncSnapshot = status;
  if (!status?.connected) {
    renderConnectionStatus(null);
    setBadge("syncBadge", "연결 전", "warning");
    setText("cloudSyncText", "교사 설정 화면에서 이 PC 자동 연결을 실행해 주세요.");
    setText("cloudSyncMetaText", "-");
    setText("syncModeText", "-");
    setText("syncImportedCount", "0");
    setText("syncPendingCount", "0");
    setText("syncFailedCount", "0");
    setText("syncServerCount", "0");
    setText("syncStatus", "자동 연결 후 임시 기록 수거가 백그라운드에서 실행됩니다.");
    setHealthPanelState("syncCard", "warning");
    setHidden("healthSyncSettingsAction", false);
    renderSummary();
    refreshActionStates();
    return;
  }

  const imported = numeric(status.lastImported);
  const deleted = numeric(status.lastDeleted);
  const marked = numeric(status.lastMarked);
  const pending = numeric(status.lastPending);
  const failed = numeric(status.lastFailed);
  const conflicts = numeric(status.lastConflicts);
  const serverProcessed = deleted + marked;
  const modeLabel = cloudSyncModeLabel(status.observationStorageMode);
  const hasFailure = status.ok === false || failed > 0 || conflicts > 0 || Boolean(status.lastError);

  renderConnectionStatus(status);
  setText("cloudSyncMetaText", formatDateTime(latestSyncTime(status)));
  setText("syncModeText", modeLabel);
  setText("syncImportedCount", numberText(imported));
  setText("syncPendingCount", numberText(pending));
  setText("syncFailedCount", numberText(failed + conflicts));
  setText("syncServerCount", numberText(serverProcessed));

  const tenantInput = byId<HTMLInputElement>("backupTenantInput");
  if (!tenantInput.value.trim() && status.tenantId) tenantInput.value = status.tenantId;

  if (isCredentialMissing(status)) {
    setHealthPanelState("syncCard", "warning");
    setBadge("syncBadge", "재연결 필요", "warning");
    setText("cloudSyncText", `${tenantLabel(status)} 자동 수거가 멈춰 있습니다.`);
    setText("syncStatus", reconnectMessage(status));
    setHidden("healthSyncSettingsAction", false);
  } else if (hasFailure) {
    setHealthPanelState("syncCard", "error");
    setBadge("syncBadge", "확인 필요", "error");
    setText("cloudSyncText", "마지막 수거에서 확인이 필요한 항목이 있습니다.");
    setText(
      "syncStatus",
      status.lastError
        ? `문제: ${status.lastError}`
        : `실패 ${failed}건 · 충돌 ${conflicts}건을 확인하세요.`,
    );
    setHidden("healthSyncSettingsAction", true);
  } else {
    setHealthPanelState("syncCard", "ok");
    setBadge("syncBadge", "정상", "ok");
    setText("cloudSyncText", imported || serverProcessed || pending ? "마지막 수거 결과를 확인했습니다." : "현재 가져올 임시 기록이 없습니다.");
    setText("syncStatus", `${modeLabel} 방식으로 처리합니다.`);
    setHidden("healthSyncSettingsAction", true);
  }

  renderSummary();
  refreshActionStates();
}

async function loadCloudSyncStatus() {
  cloudSyncLoadError = "";
  const status = await invoke<CloudSyncStatus>("get_cloud_sync_status");
  renderCloudSync(status);
}

function renderCloudSyncLoadError(error: unknown) {
  cloudSyncLoadError = String((error as Error)?.message || error || "cloud_sync_failed");
  cloudSyncSnapshot = null;
  renderConnectionStatus(null);
  setBadge("syncBadge", "오류", "error");
  setText("cloudSyncText", "자동 수거 상태를 확인하지 못했습니다.");
  setText("syncStatus", `문제: 자동 수거 상태 조회 실패. 원인: ${cloudSyncLoadError}. 해결: 상태 확인을 다시 눌러 주세요.`);
  setHealthPanelState("syncCard", "error");
  setHidden("healthSyncSettingsAction", true);
  renderSummary();
  refreshActionStates();
}

async function runCloudSyncNow() {
  setActionBusy("run-sync", true);
  setText("syncStatus", "임시 기록을 이 PC로 수거하는 중입니다.");
  try {
    const status = await invoke<CloudSyncStatus>("run_cloud_sync");
    renderCloudSync(status);
    await loadBackupStatus();
  } catch (error) {
    cloudSyncLoadError = String((error as Error)?.message || error || "cloud_sync_failed");
    setBadge("syncBadge", "오류", "error");
    setText("syncStatus", `문제: 임시 기록 수거 실패. 원인: ${cloudSyncLoadError}. 해결: 다시 연결하기 후 재시도하세요.`);
    setHealthPanelState("syncCard", "error");
    renderSummary();
  } finally {
    setActionBusy("run-sync", false);
  }
}

function renderBackupStatus(status: BackupStatus) {
  backupSnapshot = status;
  if (!status?.ok) {
    backupList = [];
    selectedBackupManifestPath = "";
    backupPreview = null;
    setBadge("backupBadge", "오류", "error");
    setBadge("healthBackupBadge", "확인 필요", "error");
    setHealthPanelState("healthBackupCard", "error");
    setText("backupFolderText", "-");
    setText("backupLatestText", "-");
    setText("backupNextText", "-");
    setText("backupMediaText", "-");
    setText("healthBackupText", "설정된 백업 폴더에 접근할 수 없습니다.");
    setText("healthBackupLatestText", "-");
    setText("healthBackupNextText", "-");
    setText("healthBackupMediaText", "첨부 누락 여부 확인 필요");
    setText(
      "backupStatus",
      `문제: 설정된 백업 폴더에 접근할 수 없습니다. 원인: ${status?.error || "unknown"}. 해결: OneDrive 로그인 상태와 폴더 위치를 확인한 뒤 백업 폴더를 다시 선택하세요.`,
    );
    setBackupFolderActionLabels("백업 폴더 다시 선택");
    setHidden("healthBackupRunAction", true);
    setHidden("healthBackupFolderAction", false);
    renderBackupRestorePanel();
    renderSummary();
    refreshActionStates();
    return;
  }

  backupList = normalizeBackupList(backupList.length ? backupList : status.backups || (status.latestBackup ? [status.latestBackup] : []));
  if (!backupList.some((backup) => backup.manifestPath === selectedBackupManifestPath)) {
    selectedBackupManifestPath = backupList[0]?.manifestPath || "";
    backupPreview = null;
  }

  const media = status.latestBackup?.media || status.lastResult?.media || {};
  const copied = numeric(media.copied);
  const skipped = numeric(media.skipped);
  const missing = numeric(media.missing);
  const failed = numeric(media.failed);
  const folder = backupFolderLabel(status);

  setText("backupFolderText", folder);
  setText("backupLatestText", formatDateTime(latestBackupTime(status)));
  setText("backupNextText", status.configured ? formatDateTime(status.nextRunAtMs) : "-");
  setText("backupMediaText", `${numberText(copied + skipped)}개 · 누락 ${numberText(missing)}개${failed ? ` · 실패 ${numberText(failed)}개` : ""}`);
  setText("healthBackupLatestText", formatDateTime(latestBackupTime(status)));
  setText("healthBackupNextText", status.configured ? formatDateTime(status.nextRunAtMs) : "-");
  setText("healthBackupMediaText", `${numberText(copied + skipped)}개 · 누락 ${numberText(missing)}개${failed ? ` · 실패 ${numberText(failed)}개` : ""}`);

  if (!status.configured) {
    setHealthPanelState("healthBackupCard", "warning");
    setBadge("backupBadge", "자동 백업 설정 필요", "warning");
    setBadge("healthBackupBadge", "설정 필요", "warning");
    setText("backupStatus", "백업 폴더를 선택하면 하루 1회 자동 백업됩니다.");
    setText("healthBackupText", "학교 OneDrive 안에 백업 폴더를 선택해 주세요.");
    setBackupFolderActionLabels("백업 폴더 선택");
    setHidden("healthBackupRunAction", true);
    setHidden("healthBackupFolderAction", false);
  } else if (failed > 0 || missing > 0) {
    setHealthPanelState("healthBackupCard", "error");
    setBadge("backupBadge", "첨부파일 백업 확인 필요", "error");
    setBadge("healthBackupBadge", "확인 필요", "error");
    setText("backupStatus", "첨부파일 일부가 누락되거나 백업되지 않았습니다. 백업 폴더 접근 권한과 남은 용량을 확인하세요.");
    setText("healthBackupText", "첨부파일 일부가 누락되거나 백업되지 않았습니다.");
    setBackupFolderActionLabels("백업 폴더 변경");
    setHidden("healthBackupRunAction", true);
    setHidden("healthBackupFolderAction", false);
  } else {
    setHealthPanelState("healthBackupCard", "ok");
    setBadge("backupBadge", "자동 백업 정상", "ok");
    setBadge("healthBackupBadge", "정상", "ok");
    setText("backupStatus", "마지막 자동 백업과 첨부파일 복사를 정상적으로 마쳤습니다.");
    setText("healthBackupText", "학교 OneDrive에 자동 백업하고 있습니다.");
    setBackupFolderActionLabels("백업 폴더 변경");
    setHidden("healthBackupRunAction", false);
    setHidden("healthBackupFolderAction", true);
  }

  renderBackupRestorePanel();
  renderSummary();
  refreshActionStates();
}

function renderBackupRestorePanel() {
  const listEl = byId<HTMLElement>("backupList");
  const selected = backupList.find((backup) => backup.manifestPath === selectedBackupManifestPath) || null;
  const configured = backupSnapshot?.configured === true;
  const tenantId = currentBackupTenantId();

  if (!tenantId) {
    setBadge("backupRestoreBadge", "학급 필요", "warning");
    setText("backupRestoreStatus", "학급 ID가 연결되면 백업 목록과 복원 미리보기를 확인할 수 있습니다.");
    listEl.innerHTML = `<p class="backup-list-empty">먼저 학급 ID를 입력하거나 다시 연결하기로 학급을 연결해 주세요.</p>`;
  } else if (!configured && !backupList.length) {
    setBadge("backupRestoreBadge", "폴더 필요", "warning");
    setText("backupRestoreStatus", "새 PC에서는 OneDrive 안의 백업 폴더를 선택하면 복원 후보를 찾습니다.");
    listEl.innerHTML = `<p class="backup-list-empty">백업 폴더가 아직 설정되지 않았습니다.</p>`;
  } else if (!backupList.length) {
    setBadge("backupRestoreBadge", "백업 없음", "warning");
    setText("backupRestoreStatus", "선택한 폴더에서 이 학급의 백업 manifest를 찾지 못했습니다.");
    listEl.innerHTML = `<p class="backup-list-empty">복원 가능한 백업이 없습니다.</p>`;
  } else {
    setBadge("backupRestoreBadge", backupRestoreTone === "ok" ? "완료" : backupPreview?.ok ? "미리보기" : "선택됨", backupRestoreTone);
    setText(
      "backupRestoreStatus",
      backupRestoreMessage
        || (backupPreview?.ok
          ? `${formatBackupDateTime(backupPreview.createdAtMs || selected?.createdAtMs)} 백업을 선택했습니다.`
          : "복원할 백업을 선택하면 미리보기를 불러옵니다."),
    );
    listEl.innerHTML = backupList.map((backup, index) => {
      const isSelected = backup.manifestPath === selectedBackupManifestPath;
      return `
        <button class="backup-list-row${isSelected ? " is-selected" : ""}" type="button" data-backup-index="${index}" aria-pressed="${isSelected}">
          <span class="backup-list-row__radio" aria-hidden="true"></span>
          <span class="backup-list-row__device" aria-hidden="true"><i class="fa-solid fa-desktop"></i></span>
          <span class="backup-list-row__main">
            <strong class="backup-list-row__time">
              <span>${escapeHtml(formatBackupDateTime(backup.createdAtMs))}</span>
              ${index === 0 ? `<span class="backup-list-row__badge">최신</span>` : ""}
            </strong>
            <span class="backup-list-row__meta">${escapeHtml(backupSourceListText(backup.source))}</span>
            <span class="backup-list-row__counts">${escapeHtml(backupRowSummary(backup))}</span>
          </span>
        </button>
      `;
    }).join("");
  }

  const counts = backupPreview?.counts || selected?.counts || {};
  const media = backupPreview?.media || selected?.media || {};
  const source = backupPreview?.source || selected?.source;
  setText("backupPreviewCare", backupList.length ? `${numberText(backupCareCount(counts))}건` : "-");
  setText("backupPreviewAttendance", backupList.length ? `${numberText(backupAttendanceCount(counts))}건` : "-");
  setText("backupPreviewLearning", backupList.length ? `${numberText(backupLearningCount(counts))}건` : "-");
  setText("backupPreviewStudentRecord", backupList.length ? `${numberText(backupStudentRecordCount(counts))}건` : "-");
  setText("backupPreviewBoard", backupList.length ? `${numberText(backupBoardSnapshotCount(counts))}건` : "-");
  setText("backupPreviewAttachments", backupList.length ? `${numberText(backupBoardMediaCount(counts, media))}개` : "-");
  setText("backupPreviewArchives", backupList.length ? `${numberText(backupArchiveCount(counts))}개` : "-");
  const detailsEl = byId<HTMLElement>("backupPreviewDetails");
  if (!backupList.length) {
    detailsEl.innerHTML = `<p class="restore-preview-empty">복원할 백업을 선택하면 PC 정보와 상세 건수가 표시됩니다.</p>`;
  } else {
    detailsEl.innerHTML = `
      <div class="restore-preview-source">
        <i class="fa-solid fa-desktop" aria-hidden="true"></i>
        <strong>${escapeHtml(`${backupSourcePcName(source)} / ${backupSourceRelation(source)} / ${backupEnvironmentText(source)}`)}</strong>
        <span>${escapeHtml(formatBackupDateTime(backupPreview?.createdAtMs || selected?.createdAtMs))} 백업</span>
      </div>
    `;
  }
  refreshActionStates();
}

async function loadBackupList(tenantId: string) {
  const payload = await invoke<{ ok?: boolean; backups?: BackupItem[]; error?: string }>("list_local_backups", {
    tenantId,
    limit: 10,
  });
  backupList = normalizeBackupList(payload?.backups || []);
  if (!backupList.some((backup) => backup.manifestPath === selectedBackupManifestPath)) {
    selectedBackupManifestPath = backupList[0]?.manifestPath || "";
    backupPreview = null;
  }
}

async function loadBackupStatus() {
  backupLoadError = "";
  const tenantId = currentBackupTenantId();
  if (!tenantId) {
    backupList = [];
    selectedBackupManifestPath = "";
    backupPreview = null;
    renderBackupStatus({ ok: true, configured: false });
    return;
  }
  const status = await invoke<BackupStatus>("get_backup_status", { tenantId });
  await loadBackupList(tenantId).catch(() => {
    backupList = normalizeBackupList(status.backups || (status.latestBackup ? [status.latestBackup] : []));
  });
  renderBackupStatus(status);
  if (selectedBackupManifestPath) {
    void loadBackupPreview(selectedBackupManifestPath);
  }
}

function renderBackupLoadError(error: unknown) {
  backupLoadError = String((error as Error)?.message || error || "backup_failed");
  backupList = [];
  selectedBackupManifestPath = "";
  backupPreview = null;
  renderBackupStatus({ ok: false, configured: false, error: backupLoadError });
}

function applyBackupDiscovery(discovery: BackupDiscovery, selectedFolder: string) {
  const tenants = Array.isArray(discovery.tenants) ? discovery.tenants : [];
  const currentTenant = currentBackupTenantId();
  const connectedTenant = cloudSyncSnapshot?.tenantId || "";
  const detected = tenants.find((tenant) => tenant.tenantId === currentTenant)
    || tenants.find((tenant) => tenant.tenantId === connectedTenant)
    || tenants[0]
    || null;
  const tenantInput = byId<HTMLInputElement>("backupTenantInput");
  if (!tenantInput.value.trim() && detected?.tenantId) {
    tenantInput.value = detected.tenantId;
  }
  if (detected?.backups?.length) {
    backupList = normalizeBackupList(detected.backups);
    selectedBackupManifestPath = backupList[0]?.manifestPath || "";
    backupPreview = null;
  }
  const rootDir = discovery.backupRootDir || selectedFolder;
  if (tenants.length > 1 && detected?.tenantId) {
    setBackupRestoreMessage(`백업 폴더에서 ${tenants.length}개 학급을 찾았습니다. 연결된 학급의 백업을 선택했습니다.`, "warning");
  } else if (detected?.tenantId) {
    setBackupRestoreMessage("연결된 학급의 백업 폴더를 찾았습니다.", "ok");
  } else {
    setBackupRestoreMessage("백업 root 폴더를 설정했습니다. 새 백업을 만들면 이곳에 표시됩니다.", "neutral");
  }
  return rootDir;
}

async function chooseBackupFolder() {
  setActionBusy("choose-backup-folder", true);
  setText("backupStatus", "백업 폴더 선택 창을 여는 중입니다.");
  await waitForPaint();
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "OnlineClass 로컬 백업을 저장할 클라우드 동기화 폴더 선택",
    });
    if (!selected || Array.isArray(selected)) return;
    setText("backupStatus", "선택한 폴더에서 기존 백업과 학급 정보를 확인하는 중입니다. 클라우드 동기화 폴더는 시간이 걸릴 수 있습니다.");
    await waitForPaint();
    const discovery = await invoke<BackupDiscovery>("discover_backup_tenants", { folderPath: selected });
    const folderPath = applyBackupDiscovery(discovery || { ok: false }, selected);
    const tenantId = currentBackupTenantId();
    if (!tenantId) {
      setText("backupStatus", "학급 ID를 찾지 못했습니다. 학급 ID를 입력한 뒤 백업 폴더를 다시 선택해 주세요.");
      renderBackupRestorePanel();
      return;
    }
    setText("backupStatus", "백업 폴더 설정을 저장하는 중입니다.");
    await waitForPaint();
    const status = await invoke<BackupStatus>("set_backup_folder", { tenantId, folderPath });
    setText("backupStatus", "백업 목록을 새로 불러오는 중입니다.");
    await waitForPaint();
    await loadBackupList(tenantId).catch(() => undefined);
    renderBackupStatus(status);
    await loadDeviceSyncStatus().catch(() => undefined);
    if (status?.ok) {
      setText("backupStatus", "백업 폴더 설정을 완료했습니다. 필요하면 지금 백업을 눌러 새 백업을 만들 수 있습니다.");
    }
    if (selectedBackupManifestPath) {
      void loadBackupPreview(selectedBackupManifestPath);
    }
  } catch (error) {
    renderBackupStatus({ ok: false, configured: false, error: String((error as Error)?.message || error) });
  } finally {
    setActionBusy("choose-backup-folder", false);
  }
}

async function loadBackupPreview(manifestPath: string) {
  const tenantId = currentBackupTenantId();
  if (!tenantId || !manifestPath) return;
  try {
    const preview = await invoke<BackupPreview>("preview_local_backup_restore", { tenantId, manifestPath });
    if (!preview?.ok) {
      backupPreview = null;
      setBackupRestoreMessage(`미리보기 실패: ${preview?.error || "backup_restore_preview_failed"}`, "error");
    } else if (selectedBackupManifestPath === manifestPath) {
      backupPreview = preview;
      if (backupRestoreTone !== "ok") setBackupRestoreMessage("", "neutral");
    }
  } catch (error) {
    backupPreview = null;
    setBackupRestoreMessage(`미리보기 실패: ${String((error as Error)?.message || error)}`, "error");
  }
  renderBackupRestorePanel();
}

function selectBackupManifest(manifestPath: string) {
  const safePath = String(manifestPath || "").trim();
  if (!safePath || safePath === selectedBackupManifestPath) return;
  selectedBackupManifestPath = safePath;
  backupPreview = null;
  setBackupRestoreMessage("", "neutral");
  renderBackupRestorePanel();
  void loadBackupPreview(safePath);
}

async function runBackupNow() {
  const tenantId = currentBackupTenantId();
  if (!tenantId) {
    setText("backupStatus", "먼저 학급 ID를 입력하거나 다시 연결하기로 학급을 연결해 주세요.");
    return;
  }
  setActionBusy("run-backup", true);
  setText("backupStatus", "백업을 생성하고 첨부파일을 동기화하는 중입니다.");
  try {
    const result = await invoke<BackupStatus>("run_local_backup", { tenantId });
    if (!result?.ok) {
      renderBackupStatus(result || { ok: false, configured: false, error: "backup_failed" });
      return;
    }
    setBackupRestoreMessage("새 백업을 만들었습니다. 필요하면 이 백업을 다른 PC에서 복원할 수 있습니다.", "ok");
    await loadBackupStatus();
  } catch (error) {
    renderBackupStatus({ ok: false, configured: false, error: String((error as Error)?.message || error) });
  } finally {
    setActionBusy("run-backup", false);
  }
}

async function restoreSelectedBackup() {
  const tenantId = currentBackupTenantId();
  const manifestPath = selectedBackupManifestPath;
  if (!tenantId || !manifestPath) {
    setBackupRestoreMessage("복원할 백업을 먼저 선택해 주세요.", "warning");
    renderBackupRestorePanel();
    return;
  }
  const selected = backupList.find((backup) => backup.manifestPath === manifestPath);
  const sourceText = backupSourceSummary(backupPreview?.source || selected?.source);
  const ok = await confirmBackupRestore({
    date: formatBackupDateTime(selected?.createdAtMs),
    source: sourceText,
    summary: "선택한 백업의 자료와 첨부파일을 현재 PC에 병합합니다.",
  });
  if (!ok) return;
  setActionBusy("restore-backup", true);
  setBackupRestoreMessage("현재 상태 보호 백업을 만든 뒤 선택한 백업을 병합하는 중입니다.", "neutral");
  renderBackupRestorePanel();
  try {
    const result = await invoke<{ ok?: boolean; imported?: number; mediaRestored?: number; mediaMissing?: number; safetyBackup?: object; error?: string }>("restore_local_backup", {
      tenantId,
      manifestPath,
    });
    if (!result?.ok) {
      const error = String(result?.error || "backup_restore_failed");
      setBackupRestoreMessage(
        error.startsWith("pre_restore_backup_failed:")
          ? "현재 상태 보호 백업을 만들지 못해 복원을 시작하지 않았습니다. OneDrive 연결과 남은 용량을 확인한 뒤 다시 시도하세요."
          : `복원 실패: ${error}`,
        "error",
      );
    } else {
      setBackupRestoreMessage(`보호 백업 후 복원 완료: DB 반영 ${numberText(result.imported)}건, 첨부 복원 ${numberText(result.mediaRestored)}개${numeric(result.mediaMissing) ? `, 누락 ${numberText(result.mediaMissing)}개` : ""}.`, "ok");
      await loadBackupStatus();
      await loadDeviceSyncStatus().catch(() => undefined);
    }
  } catch (error) {
    setBackupRestoreMessage(`복원 실패: ${String((error as Error)?.message || error)}`, "error");
  } finally {
    setActionBusy("restore-backup", false);
    renderBackupRestorePanel();
  }
}

async function refreshAll() {
  setActionBusy("refresh-status", true);
  try {
    await loadStatus().catch(renderServiceLoadError);
    await loadCloudSyncStatus().catch(renderCloudSyncLoadError);
    await loadDeviceConnectionStatus().catch(() => undefined);
    await desktopShell.refreshConnection().catch(() => undefined);
    await loadBackupStatus().catch(renderBackupLoadError);
    await loadDeviceSyncStatus().catch(() => renderDeviceSyncStatus(null));
    await loadHomeOverview(currentBackupTenantId());
  } finally {
    setActionBusy("refresh-status", false);
  }
}

const deviceAuthorization = createDeviceAuthorizationController({
  setText,
  setActionBusy: (busy) => setActionBusy("open-settings", busy),
  onConnected: refreshAll,
  onStartFailure: (message) => setText("connectionMetaText", message),
  showSettings: () => document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="settings"]')?.click(),
});

function bindUi() {
  document.querySelectorAll<HTMLButtonElement>("button[data-copy-target]").forEach((button) => {
    button.addEventListener("click", async () => {
      const targetId = button.dataset.copyTarget || "";
      const copied = await copyText(copyTargetValue(targetId));
      const original = button.textContent || "복사";
      button.textContent = copied ? "복사됨" : "복사 실패";
      window.setTimeout(() => { button.textContent = original; }, 1_200);
    });
  });

  actionButtons("open-settings").forEach((button) => button.addEventListener("click", () => void deviceAuthorization.start()));
  actionButtons("open-data-directory").forEach((button) => button.addEventListener("click", async () => {
    setActionBusy("open-data-directory", true);
    try {
      const result = await invoke<{ ok?: boolean; error?: string }>("open_local_data_directory");
      if (result?.ok === false) throw new Error(result.error || "local_data_directory_open_failed");
      setText("homeHealthText", "저장 위치를 열었습니다");
    } catch (error) {
      setText("homeHealthText", `저장 위치 열기 실패: ${String((error as Error)?.message || error)}`);
    } finally {
      setActionBusy("open-data-directory", false);
    }
  }));
  byId<HTMLButtonElement>("healthConnectionAction").addEventListener("click", () => {
    const connected = deviceConnectionSnapshot?.connected === true
      || (cloudSyncSnapshot?.connected === true && !isCredentialMissing(cloudSyncSnapshot));
    if (connected) {
      document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="settings"]')?.click();
      return;
    }
    void deviceAuthorization.start();
  });
  byId<HTMLButtonElement>("deviceAuthStart").addEventListener("click", () => void deviceAuthorization.start());
  byId<HTMLButtonElement>("deviceAuthReopen").addEventListener("click", () => void deviceAuthorization.reopen());
  actionButtons("refresh-status").forEach((button) => button.addEventListener("click", refreshAll));
  actionButtons("run-sync").forEach((button) => button.addEventListener("click", runCloudSyncNow));
  actionButtons("run-device-sync").forEach((button) => button.addEventListener("click", () => void runDeviceSyncNow(async () => {
    await Promise.all([
      loadBackupStatus(),
      dataExplorer.refresh(),
      sharedArchive.refresh(),
      loadHomeOverview(currentBackupTenantId()),
    ]);
  })));
  actionButtons("repair-device-sync").forEach((button) => button.addEventListener("click", () => void deviceAuthorization.start()));
  actionButtons("run-backup").forEach((button) => button.addEventListener("click", runBackupNow));
  actionButtons("restore-backup").forEach((button) => button.addEventListener("click", restoreSelectedBackup));
  actionButtons("choose-backup-folder").forEach((button) => {
    button.addEventListener("click", () => {
      chooseBackupFolder().catch((error) => {
        renderBackupStatus({ ok: false, configured: false, error: String((error as Error)?.message || error) });
      });
    });
  });

  byId<HTMLElement>("backupList").addEventListener("click", (event) => {
    const target = event.target as HTMLElement | null;
    const row = target?.closest<HTMLButtonElement>("[data-backup-index]");
    if (!row) return;
    const backup = backupList[Number(row.dataset.backupIndex || 0)];
    selectBackupManifest(backup?.manifestPath || "");
  });

  byId<HTMLInputElement>("backupTenantInput").addEventListener("change", () => {
    backupList = [];
    selectedBackupManifestPath = "";
    backupPreview = null;
    setBackupRestoreMessage("", "neutral");
    loadBackupStatus().catch(renderBackupLoadError);
  });
}

const desktopShell = initDesktopShell();
initArchiveBoardExplorer();
initWorkNoteReader();
const dataExplorer = initDataExplorer({ getTenantId: currentBackupTenantId });
initDeviceSyncConflicts({ getTenantId: currentBackupTenantId });
const studentTimeline = initStudentTimeline({ getTenantId: currentBackupTenantId });
initHomeDashboard({
  onViewChange(view, context) {
    if (view === "data") void dataExplorer.open({ group: context.group, sectionKey: context.sectionKey, hasAttachment: context.attachment });
    if (view === "students") void studentTimeline.open();
  },
  onSearch(query) {
    void dataExplorer.open({ query });
  },
});
bindUi();
if (designPreview !== "settings") {
  initSettingsDashboard({ onDisconnected: refreshAll, onAuthorizeBrowser: () => deviceAuthorization.start() });
}
renderAppVersion();
if (designPreview === "archive") initSharedArchivePreview();
else sharedArchive = initSharedArchive({ getTenantId: currentBackupTenantId });
if (designPreview === "auth") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="settings"]')?.click();
  deviceAuthorization.render({ ok: true, status: "pending", expiresAtMs: Date.now() + 10 * 60 * 1000 });
} else if (designPreview === "data") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="data"]')?.click();
} else if (designPreview === "students") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="students"]')?.click();
} else if (designPreview === "backup") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="backup"]')?.click();
  initBackupRestorePreview();
} else if (designPreview === "archive") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="archive"]')?.click();
} else if (designPreview === "health") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="health"]')?.click();
  initHealthDashboardPreview();
} else if (designPreview === "settings") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="settings"]')?.click();
  initSettingsDashboardPreview();
} else {
  refreshAll().catch((error) => {
    renderServiceLoadError(error);
  });
}
