import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import "./styles.css";

declare const __APP_VERSION__: string;

const TEACHER_SETTINGS_URL = "https://classaimate.pages.dev/teacher-dashboard/tenant-settings";
const APP_VERSION = String(__APP_VERSION__ || "").trim() || "0.0.0";

type ServiceStatus = {
  ok: boolean;
  service: string;
  version: string;
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

type BackupStatus = {
  ok: boolean;
  configured: boolean;
  tenantId?: string;
  backupRootDir?: string;
  tenantBackupDir?: string;
  lastRunAtMs?: number;
  nextRunAtMs?: number;
  latestBackup?: {
    backupId?: string;
    createdAtMs?: number;
    manifestPath?: string;
    counts?: Record<string, number>;
    media?: {
      copied?: number;
      skipped?: number;
      missing?: number;
      failed?: number;
      bytes?: number;
    };
  } | null;
  lastResult?: {
    ok?: boolean;
    media?: {
      copied?: number;
      skipped?: number;
      missing?: number;
      failed?: number;
      bytes?: number;
    };
  } | null;
  backups?: BackupItem[];
  error?: string;
};

type BackupItem = {
  ok?: boolean;
  tenantId?: string;
  backupId?: string;
  createdAtMs?: number;
  manifestPath?: string;
  dbPath?: string;
  counts?: Record<string, number>;
  media?: {
    records?: unknown[];
    copied?: number;
    skipped?: number;
    missing?: number;
    failed?: number;
    bytes?: number;
  };
};

type BackupPreview = {
  ok: boolean;
  tenantId?: string;
  backupId?: string;
  manifestPath?: string;
  createdAtMs?: number;
  counts?: Record<string, number>;
  media?: {
    records?: unknown[];
    copied?: number;
    skipped?: number;
    missing?: number;
    failed?: number;
  };
  error?: string;
};

type BackupDiscovery = {
  ok: boolean;
  selectedPath?: string;
  backupRootDir?: string;
  namespaceDir?: string;
  tenantCount?: number;
  tenants?: Array<{
    tenantId?: string;
    tenantBackupDir?: string;
    latestBackup?: BackupItem;
    backups?: BackupItem[];
  }>;
  error?: string;
};

type CommandResult = {
  ok: boolean;
  error?: string;
};

type LocalDataSection = {
  key: string;
  label: string;
  count?: number;
  updatedAtMs?: number;
  route?: string;
};

type LocalOverview = {
  ok: boolean;
  tenantId?: string;
  stats?: Record<string, unknown>;
  sections?: LocalDataSection[];
  recentImportRuns?: unknown[];
  error?: string;
};

type BadgeTone = "ok" | "warning" | "error" | "neutral";
type ActionName = "open-settings" | "refresh-status" | "run-sync" | "run-backup" | "choose-backup-folder" | "restore-backup" | "open-data-overview";

let serviceSnapshot: ServiceStatus | null = null;
let serviceLoadError = "";
let cloudSyncSnapshot: CloudSyncStatus | null = null;
let cloudSyncLoadError = "";
let backupSnapshot: BackupStatus | null = null;
let backupLoadError = "";
let backupList: BackupItem[] = [];
let selectedBackupManifestPath = "";
let backupPreview: BackupPreview | null = null;
let backupRestoreMessage = "";
let backupRestoreTone: BadgeTone = "neutral";
let localOverview: LocalOverview | null = null;
let selectedDataSectionKey = "";
let localDataRecords: unknown[] = [];
let selectedDataRecordIndex = -1;
const busyActions = new Set<ActionName>();

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

function numberText(value?: number) {
  return String(Number(value || 0) || 0);
}

function numeric(value?: number) {
  return Number(value || 0) || 0;
}

function actionButtons(action: ActionName) {
  return Array.from(document.querySelectorAll<HTMLButtonElement>(`button[data-action="${action}"]`));
}

function setActionLabel(action: ActionName, label: string) {
  actionButtons(action).forEach((button) => {
    button.textContent = label;
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
  (["open-settings", "refresh-status", "run-sync", "run-backup", "choose-backup-folder", "restore-backup", "open-data-overview"] as ActionName[]).forEach((action) => {
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

function currentBackupTenantId() {
  return byId<HTMLInputElement>("backupTenantInput").value.trim();
}

function tenantLabel(status?: CloudSyncStatus | null) {
  return status?.tenantName || status?.tenantId || "연결된 학급 없음";
}

function accountLabel(status?: CloudSyncStatus | null) {
  return status?.accountEmail || status?.accountDisplayName || status?.uid || "-";
}

function buildTeacherSettingsUrl(tenantId = "") {
  const url = new URL(TEACHER_SETTINGS_URL);
  const safeTenantId = tenantId.trim();
  if (safeTenantId) url.searchParams.set("tenantId", safeTenantId);
  url.searchParams.set("tab", "sensitive");
  url.searchParams.set("connectLocal", "1");
  url.searchParams.set("source", "local-sensitive-store");
  return url.toString();
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

function backupMathDailyCount(counts?: Record<string, number>) {
  return [
    "math_daily_attempts",
    "math_daily_student_profiles",
    "math_daily_review_sessions",
    "math_daily_assignments",
    "math_daily_assignment_results",
    "math_daily_cache_runs",
    "mathDailyAttemptCount",
    "mathDailyProfileCount",
    "mathDailyReviewSessionCount",
    "mathDailyAssignmentResultCount",
  ].reduce((sum, key) => sum + (Number(counts?.[key] || 0) || 0), 0);
}

function backupBoardMediaCount(counts?: Record<string, number>, media?: BackupItem["media"] | BackupPreview["media"]) {
  const count = countFrom(counts, ["board_media_files", "boardMediaCount"]);
  if (count) return count;
  const records = Array.isArray(media?.records) ? media.records.length : 0;
  return records;
}

function backupShortId(backup: BackupItem) {
  return String(backup.backupId || "").slice(-8) || "-";
}

function backupRowSummary(backup: BackupItem) {
  const counts = backup.counts || {};
  const media = backup.media || {};
  return `관찰 ${numberText(backupObservationCount(counts))}건 · 학생 비공개 ${numberText(backupPrivateDetailCount(counts))}건 · 보드 미디어 ${numberText(backupBoardMediaCount(counts, media))}개`;
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

function sectionByKey(key: string) {
  return (localOverview?.sections || []).find((section) => section.key === key) || null;
}

function recordDisplayTitle(record: unknown, index: number) {
  const row = (record && typeof record === "object" ? record : {}) as Record<string, unknown>;
  return String(
    row.title
      || row.planName
      || row.studentName
      || row.name
      || row.dateKey
      || row.id
      || row.docId
      || row.recordId
      || row.assignmentId
      || row.resultId
      || `record-${index + 1}`,
  );
}

function recordDisplayMeta(record: unknown) {
  const row = (record && typeof record === "object" ? record : {}) as Record<string, unknown>;
  const parts = [
    row.studentCode || row.studentId,
    row.dateKey || row.scheduledDate || row.linkedDateKey,
    row.status || row.kind || row.resultMode,
    row.updatedAtMs ? formatDateTime(Number(row.updatedAtMs)) : "",
  ].map((item) => String(item || "").trim()).filter(Boolean);
  return parts.join(" · ") || "-";
}

function renderDataSections() {
  const listEl = byId<HTMLElement>("localDataSectionList");
  const sections = localOverview?.sections || [];
  if (!sections.length) {
    listEl.innerHTML = `<p class="data-empty">저장 내용을 아직 불러오지 않았습니다.</p>`;
    return;
  }
  listEl.innerHTML = sections.map((section) => {
    const selected = section.key === selectedDataSectionKey;
    return `
      <button class="data-section-row${selected ? " is-selected" : ""}" type="button" data-data-section="${escapeHtml(section.key)}">
        <span>${escapeHtml(section.label || section.key)}</span>
        <strong>${numberText(section.count)}건</strong>
        <small>${escapeHtml(formatDateTime(section.updatedAtMs))}</small>
      </button>
    `;
  }).join("");
}

function renderDataRecords() {
  const listEl = byId<HTMLElement>("localDataRecordList");
  const detailEl = byId<HTMLElement>("localDataDetail");
  if (!selectedDataSectionKey) {
    listEl.innerHTML = `<p class="data-empty">왼쪽에서 데이터 종류를 선택하세요.</p>`;
    detailEl.textContent = "섹션을 선택하면 상세 payload가 표시됩니다.";
    return;
  }
  if (!localDataRecords.length) {
    listEl.innerHTML = `<p class="data-empty">선택한 섹션에 저장된 기록이 없습니다.</p>`;
    detailEl.textContent = "저장된 payload가 없습니다.";
    return;
  }
  listEl.innerHTML = localDataRecords.map((record, index) => `
    <button class="data-record-row${index === selectedDataRecordIndex ? " is-selected" : ""}" type="button" data-data-record-index="${index}">
      <strong>${escapeHtml(recordDisplayTitle(record, index))}</strong>
      <span>${escapeHtml(recordDisplayMeta(record))}</span>
    </button>
  `).join("");
  const selected = localDataRecords[selectedDataRecordIndex >= 0 ? selectedDataRecordIndex : 0];
  selectedDataRecordIndex = Math.max(0, selectedDataRecordIndex);
  detailEl.textContent = JSON.stringify(selected, null, 2);
}

function renderDataOverview() {
  renderDataSections();
  renderDataRecords();
  refreshActionStates();
}

async function loadDataSection(sectionKey: string) {
  const tenantId = currentBackupTenantId();
  const section = sectionByKey(sectionKey);
  if (!tenantId || !section?.route) return;
  selectedDataSectionKey = sectionKey;
  selectedDataRecordIndex = -1;
  renderDataOverview();
  const payload = await invoke<{ ok?: boolean; records?: unknown[] }>("list_local_data_section", {
    tenantId,
    route: section.route,
    limit: 10000,
  });
  if (payload?.ok === false) throw new Error(String((payload as { error?: string }).error || "local_data_section_failed"));
  localDataRecords = Array.isArray(payload.records) ? payload.records : [];
  selectedDataRecordIndex = localDataRecords.length ? 0 : -1;
  renderDataOverview();
}

async function loadDataOverview() {
  const tenantId = currentBackupTenantId();
  if (!tenantId) {
    localOverview = null;
    selectedDataSectionKey = "";
    localDataRecords = [];
    selectedDataRecordIndex = -1;
    setText("dataOverviewStatus", "학급 ID를 입력하거나 다시 연결하기로 학급을 연결해 주세요.");
    renderDataOverview();
    return;
  }
  setActionBusy("open-data-overview", true);
  setText("dataOverviewStatus", "로컬 DB 저장 내용을 불러오는 중입니다.");
  try {
    localOverview = await invoke<LocalOverview>("get_local_overview", { tenantId });
    if (localOverview?.ok === false) throw new Error(String(localOverview.error || "local_overview_failed"));
    const firstWithRecords = (localOverview.sections || []).find((section) => numeric(section.count) > 0)
      || (localOverview.sections || [])[0]
      || null;
    selectedDataSectionKey = selectedDataSectionKey || firstWithRecords?.key || "";
    setText("dataOverviewStatus", `${tenantId} 로컬 DB 저장 내용을 확인했습니다.`);
    renderDataOverview();
    if (selectedDataSectionKey) {
      await loadDataSection(selectedDataSectionKey);
    }
  } catch (error) {
    setText("dataOverviewStatus", `저장 내용 조회 실패: ${String((error as Error)?.message || error)}`);
    localDataRecords = [];
    selectedDataRecordIndex = -1;
    renderDataOverview();
  } finally {
    setActionBusy("open-data-overview", false);
  }
}

function setBackupRestoreMessage(message: string, tone: BadgeTone = "neutral") {
  backupRestoreMessage = message;
  backupRestoreTone = tone;
}

function renderSummary() {
  const summaryCard = byId<HTMLElement>("summaryCard");
  const pending = numeric(cloudSyncSnapshot?.lastPending);
  const failed = numeric(cloudSyncSnapshot?.lastFailed) + numeric(cloudSyncSnapshot?.lastConflicts);
  const backupHasError = backupSnapshot?.ok === false || Boolean(backupLoadError);
  const backupConfigured = backupSnapshot?.configured === true;

  let tone: "is-ok" | "is-warning" | "is-error" | "is-checking" = "is-checking";
  let title = "상태 확인 중";
  let description = "로컬 저장소, 자동 수거, 백업 상태를 확인하고 있습니다.";

  if (serviceLoadError || serviceSnapshot?.ok === false) {
    tone = "is-error";
    title = "로컬 앱 상태를 확인해야 합니다.";
    description = "PC 설치본 DBHelper가 정상 실행 중인지 확인한 뒤 상태 확인을 눌러 주세요.";
  } else if (!cloudSyncSnapshot && !cloudSyncLoadError) {
    tone = "is-checking";
  } else if (!cloudSyncSnapshot?.connected || isCredentialMissing(cloudSyncSnapshot)) {
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
    title = "정상 작동 중입니다.";
    description = "이 PC에서 민감기록을 저장하고, 임시 기록을 자동 수거하며, 백업도 준비되어 있습니다.";
  }

  summaryCard.className = `summary-card ${tone}`;
  setText("summaryTitle", title);
  setText("summaryDescription", description);
  setText("summaryTenantText", tenantLabel(cloudSyncSnapshot));
  setText("summarySyncText", formatDateTime(latestSyncTime(cloudSyncSnapshot)));
  setText("summaryBackupText", formatDateTime(latestBackupTime(backupSnapshot)));
  setText("summaryPendingText", `${numberText(pending)}건`);
}

async function loadStatus() {
  serviceLoadError = "";
  const status = await invoke<ServiceStatus>("get_service_status");
  serviceSnapshot = status;
  const statusDot = byId<HTMLSpanElement>("statusDot");
  const keyInput = byId<HTMLInputElement>("pairingKeyInput");

  statusDot.classList.toggle("is-ok", status.ok);
  statusDot.classList.toggle("is-error", !status.ok);
  setText("statusText", status.ok ? "실행 중" : `시작 실패: ${status.error || "unknown"}`);
  setText("endpointText", status.endpoint);
  setText("dbPathText", status.dbPath);
  setText("dataDirText", status.dataDir);
  setText("serviceVersionText", status.version);
  setText("servicePortText", status.port ? String(status.port) : "-");
  keyInput.value = status.pairingKey || "";
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
    setBadge("connectionBadge", "연결 전", "warning");
    setText("connectionTitle", "교사 설정 연결이 필요합니다.");
    setText("connectionMetaText", "교사 설정 화면에서 이 PC 자동 연결을 실행하면 수거와 백업 학급 정보가 연결됩니다.");
    setText("connectionModeText", "자동 연결 대기");
    setText("connectionAccountText", "-");
    setText("connectionCheckText", "-");
    return;
  }

  const tenant = tenantLabel(status);
  if (isCredentialMissing(status)) {
    setBadge("connectionBadge", "재연결 필요", "warning");
    setText("connectionTitle", `${tenant} 연결을 다시 해야 합니다.`);
    setText("connectionMetaText", reconnectMessage(status));
    setText("connectionModeText", "자동 연결 만료");
    setText("connectionAccountText", accountLabel(status));
    setText("connectionCheckText", formatDateTime(latestSyncTime(status)));
    return;
  }

  setBadge("connectionBadge", "정상", "ok");
  setText("connectionTitle", "정상 작동 중입니다.");
  setText("connectionMetaText", "이 PC에서 민감기록을 저장하고, 임시 기록을 자동 수거합니다.");
  setText("connectionModeText", status.credentialStorage === "windows_dpapi_file" ? "자동 연결(암호화 보관)" : "자동 연결");
  setText("connectionAccountText", accountLabel(status));
  setText("connectionCheckText", formatDateTime(latestSyncTime(status)));
}

async function openTeacherSettings() {
  const tenantId = currentBackupTenantId();
  setActionBusy("open-settings", true);
  try {
    const result = await invoke<CommandResult>("open_teacher_settings_url", {
      url: buildTeacherSettingsUrl(tenantId),
    });
    if (!result?.ok) {
      setText("connectionMetaText", `브라우저 열기 실패: ${result?.error || "open_failed"}`);
    }
  } catch (error) {
    setText("connectionMetaText", `브라우저 열기 실패: ${String((error as Error)?.message || error)}`);
  } finally {
    setActionBusy("open-settings", false);
  }
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
    setBadge("syncBadge", "재연결 필요", "warning");
    setText("cloudSyncText", `${tenantLabel(status)} 자동 수거가 멈춰 있습니다.`);
    setText("syncStatus", reconnectMessage(status));
  } else if (hasFailure) {
    setBadge("syncBadge", "확인 필요", "error");
    setText("cloudSyncText", "마지막 수거에서 확인이 필요한 항목이 있습니다.");
    setText(
      "syncStatus",
      status.lastError
        ? `문제: ${status.lastError}`
        : `실패 ${failed}건 · 충돌 ${conflicts}건을 확인하세요.`,
    );
  } else {
    setBadge("syncBadge", "정상", "ok");
    setText("cloudSyncText", imported || serverProcessed || pending ? "마지막 수거 결과를 확인했습니다." : "현재 가져올 임시 기록이 없습니다.");
    setText("syncStatus", `${modeLabel} 방식으로 처리합니다.`);
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
    setBadge("syncBadge", "오류", "error");
    setText("syncStatus", `문제: 임시 기록 수거 실패. 원인: ${String((error as Error)?.message || error)}. 해결: 다시 연결하기 후 재시도하세요.`);
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
    setText("backupFolderText", "-");
    setText("backupLatestText", "-");
    setText("backupNextText", "-");
    setText("backupMediaText", "-");
    setText(
      "backupStatus",
      `문제: 설정된 백업 폴더에 접근할 수 없습니다. 원인: ${status?.error || "unknown"}. 해결: OneDrive 로그인 상태와 폴더 위치를 확인한 뒤 백업 폴더를 다시 선택하세요.`,
    );
    setActionLabel("choose-backup-folder", "백업 폴더 다시 선택");
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
  const folder = status.tenantBackupDir || status.backupRootDir || "-";

  setText("backupFolderText", folder);
  setText("backupLatestText", formatDateTime(latestBackupTime(status)));
  setText("backupNextText", status.configured ? formatDateTime(status.nextRunAtMs) : "-");
  setText("backupMediaText", `복사 ${numberText(copied)}개 · 유지 ${numberText(skipped)}개${missing ? ` · 누락 ${numberText(missing)}개` : ""}${failed ? ` · 실패 ${numberText(failed)}개` : ""}`);

  if (!status.configured) {
    setBadge("backupBadge", "설정 필요", "warning");
    setText("backupStatus", "백업 폴더를 선택하면 하루 1회 자동 백업됩니다.");
    setActionLabel("choose-backup-folder", "백업 폴더 선택");
  } else if (failed > 0) {
    setBadge("backupBadge", "확인 필요", "error");
    setText("backupStatus", "첨부파일 일부를 백업하지 못했습니다. 백업 폴더 접근 권한과 남은 용량을 확인하세요.");
    setActionLabel("choose-backup-folder", "백업 폴더 변경");
  } else {
    setBadge("backupBadge", "설정됨", "ok");
    setText("backupStatus", "DB 백업과 보드 첨부파일 폴더 미러링이 자동으로 실행됩니다.");
    setActionLabel("choose-backup-folder", "백업 폴더 변경");
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
          ? "미리보기를 확인한 뒤 선택 백업 복원을 실행할 수 있습니다."
          : "복원할 백업을 선택하면 미리보기를 불러옵니다."),
    );
    listEl.innerHTML = backupList.map((backup) => {
      const isSelected = backup.manifestPath === selectedBackupManifestPath;
      const failed = numeric(backup.media?.failed);
      return `
        <button class="backup-list-row${isSelected ? " is-selected" : ""}" type="button" data-backup-manifest="${escapeHtml(backup.manifestPath)}">
          <span class="backup-list-row__radio" aria-hidden="true"></span>
          <span>
            <strong>${escapeHtml(formatDateTime(backup.createdAtMs))}</strong>
            <span>${escapeHtml(backupRowSummary(backup))}</span>
          </span>
          <span>${failed ? "첨부 확인" : `#${escapeHtml(backupShortId(backup))}`}</span>
        </button>
      `;
    }).join("");
  }

  const counts = backupPreview?.counts || selected?.counts || {};
  const media = backupPreview?.media || selected?.media || {};
  setText("backupPreviewObservations", backupList.length ? numberText(backupObservationCount(counts)) : "-");
  setText("backupPreviewPrivateDetails", backupList.length ? numberText(backupPrivateDetailCount(counts)) : "-");
  setText("backupPreviewMathDaily", backupList.length ? numberText(backupMathDailyCount(counts)) : "-");
  setText("backupPreviewBoardMedia", backupList.length ? numberText(backupBoardMediaCount(counts, media)) : "-");
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
    setBackupRestoreMessage(`백업 폴더에서 ${tenants.length}개 학급을 찾았습니다. ${detected.tenantId} 백업을 선택했습니다.`, "warning");
  } else if (detected?.tenantId) {
    setBackupRestoreMessage(`${detected.tenantId} 백업 폴더를 찾았습니다.`, "ok");
  } else {
    setBackupRestoreMessage("백업 root 폴더를 설정했습니다. 새 백업을 만들면 이곳에 표시됩니다.", "neutral");
  }
  return rootDir;
}

async function chooseBackupFolder() {
  setActionBusy("choose-backup-folder", true);
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "OnlineClass 로컬 백업을 저장할 클라우드 동기화 폴더 선택",
    });
    if (!selected || Array.isArray(selected)) return;
    const discovery = await invoke<BackupDiscovery>("discover_backup_tenants", { folderPath: selected });
    const folderPath = applyBackupDiscovery(discovery || { ok: false }, selected);
    const tenantId = currentBackupTenantId();
    if (!tenantId) {
      setText("backupStatus", "학급 ID를 찾지 못했습니다. 학급 ID를 입력한 뒤 백업 폴더를 다시 선택해 주세요.");
      renderBackupRestorePanel();
      return;
    }
    const status = await invoke<BackupStatus>("set_backup_folder", { tenantId, folderPath });
    await loadBackupList(tenantId).catch(() => undefined);
    renderBackupStatus(status);
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
  const ok = window.confirm(`${formatDateTime(selected?.createdAtMs)} 백업을 이 PC의 로컬 DB에 병합 복원합니다. 현재 PC의 더 최신 기록은 유지됩니다. 계속할까요?`);
  if (!ok) return;
  setActionBusy("restore-backup", true);
  setBackupRestoreMessage("선택한 백업을 로컬 DB에 병합 복원하는 중입니다.", "neutral");
  renderBackupRestorePanel();
  try {
    const result = await invoke<{ ok?: boolean; imported?: number; mediaRestored?: number; mediaMissing?: number; error?: string }>("restore_local_backup", {
      tenantId,
      manifestPath,
    });
    if (!result?.ok) {
      setBackupRestoreMessage(`복원 실패: ${result?.error || "backup_restore_failed"}`, "error");
    } else {
      setBackupRestoreMessage(`복원 완료: DB 반영 ${numberText(result.imported)}건, 첨부 복원 ${numberText(result.mediaRestored)}개${numeric(result.mediaMissing) ? `, 누락 ${numberText(result.mediaMissing)}개` : ""}.`, "ok");
      await loadBackupStatus();
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
    await loadBackupStatus().catch(renderBackupLoadError);
  } finally {
    setActionBusy("refresh-status", false);
  }
}

function bindUi() {
  const keyInput = byId<HTMLInputElement>("pairingKeyInput");
  byId<HTMLButtonElement>("toggleKeyBtn").addEventListener("click", () => {
    const visible = keyInput.type === "text";
    keyInput.type = visible ? "password" : "text";
    byId<HTMLButtonElement>("toggleKeyBtn").textContent = visible ? "보기" : "숨기기";
  });

  document.querySelectorAll<HTMLButtonElement>("button[data-copy-target]").forEach((button) => {
    button.addEventListener("click", async () => {
      const targetId = button.dataset.copyTarget || "";
      const copied = await copyText(copyTargetValue(targetId));
      setText("copyStatus", copied ? "복사했습니다." : "클립보드 복사에 실패했습니다.");
    });
  });

  actionButtons("open-settings").forEach((button) => button.addEventListener("click", openTeacherSettings));
  actionButtons("refresh-status").forEach((button) => button.addEventListener("click", refreshAll));
  actionButtons("run-sync").forEach((button) => button.addEventListener("click", runCloudSyncNow));
  actionButtons("run-backup").forEach((button) => button.addEventListener("click", runBackupNow));
  actionButtons("restore-backup").forEach((button) => button.addEventListener("click", restoreSelectedBackup));
  actionButtons("open-data-overview").forEach((button) => {
    button.addEventListener("click", () => {
      loadDataOverview().catch((error) => {
        setText("dataOverviewStatus", `저장 내용 조회 실패: ${String((error as Error)?.message || error)}`);
      });
    });
  });
  actionButtons("choose-backup-folder").forEach((button) => {
    button.addEventListener("click", () => {
      chooseBackupFolder().catch((error) => {
        renderBackupStatus({ ok: false, configured: false, error: String((error as Error)?.message || error) });
      });
    });
  });

  byId<HTMLElement>("backupList").addEventListener("click", (event) => {
    const target = event.target as HTMLElement | null;
    const row = target?.closest<HTMLButtonElement>("[data-backup-manifest]");
    if (!row) return;
    selectBackupManifest(row.dataset.backupManifest || "");
  });

  byId<HTMLElement>("localDataSectionList").addEventListener("click", (event) => {
    const target = event.target as HTMLElement | null;
    const row = target?.closest<HTMLButtonElement>("[data-data-section]");
    if (!row) return;
    loadDataSection(row.dataset.dataSection || "").catch((error) => {
      setText("dataOverviewStatus", `섹션 조회 실패: ${String((error as Error)?.message || error)}`);
    });
  });

  byId<HTMLElement>("localDataRecordList").addEventListener("click", (event) => {
    const target = event.target as HTMLElement | null;
    const row = target?.closest<HTMLButtonElement>("[data-data-record-index]");
    if (!row) return;
    selectedDataRecordIndex = Number(row.dataset.dataRecordIndex || 0) || 0;
    renderDataOverview();
  });

  byId<HTMLInputElement>("backupTenantInput").addEventListener("change", () => {
    backupList = [];
    selectedBackupManifestPath = "";
    backupPreview = null;
    localOverview = null;
    selectedDataSectionKey = "";
    localDataRecords = [];
    selectedDataRecordIndex = -1;
    setBackupRestoreMessage("", "neutral");
    renderDataOverview();
    loadBackupStatus().catch(renderBackupLoadError);
  });
}

bindUi();
renderAppVersion();
renderDataOverview();
refreshAll().catch((error) => {
  renderServiceLoadError(error);
});
