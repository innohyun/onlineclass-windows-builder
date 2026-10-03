import { renderDeskSummary, formatDateTime, tenantLabel, accountLabel, normalizeStorageMode, cloudSyncModeLabel, isCredentialMissing, reconnectMessage, credentialStorageLabel, latestSyncTime, latestBackupTime, type ServiceStatus, type CloudSyncStatus, type DeviceConnectionStatus } from "./desk-status-renderer";
import { formatBackupDateTime, backupSourceSummary, backupFolderLabel, normalizeBackupList } from "./backup-display-utils";
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
import "./local-workspaces.css";
import "./quick-observation.css";
import "./desk-shell.css";
import "./teacher-desk-ux.css";
import "./device-sync-ui.css";
import "./backup-storage.css";
import { initDeskStudentPanel } from "./desk-student-panel";
import { beginDeskRestore, isDeskRestoreBlocked } from "./desk-restore-lock";
import { mountDeskViews } from "./desk-navigation";
import { initDeskPageGuide } from "./desk-page-guide";
import { bindTeacherDeskLifecycle, initTeacherDeskDocuments } from "./desk-workspace-lifecycle";
import { initDeskRecordEditor } from "./desk-record-editor";
import { initSharedArchive } from "./shared-archive";
import { initHomeDashboard, loadHomeOverview } from "./home-dashboard";
import { createDeviceAuthorizationController, type DeviceAuthorizationResult } from "./device-authorization";
import { initRecordBrowsers } from "./record-browsers";
import { createBackupRestoreProgress } from "./backup-restore-progress";
import { renderBackupRestorePanel as renderRestorePanel, renderRestoreProgress } from "./backup-restore-renderer";
import { confirmBackupRestore } from "./backup-restore-confirmation";
import { initBackupRestorePreview } from "./backup-restore-preview";
import { initSharedArchivePreview } from "./shared-archive-preview";
import { initArchiveBoardExplorer } from "./archive-board-explorer";
import { initWorkNoteReader } from "./work-note-reader";
import { initDeviceSyncConflicts } from "./device-sync-conflicts";
import { initHealthDashboardPreview } from "./health-dashboard-preview";
import { initSettingsDashboard } from "./settings-dashboard";
import { initSettingsDashboardPreview } from "./settings-dashboard-preview";
import { loadDeviceSyncStatus, renderDeviceSyncStatus, runDeviceSyncNow, isDeviceSyncRunning } from "./device-sync-ui";
import { initDesktopShell } from "./desktop-shell";
import { backupKindLabel, initBackupStorage } from "./backup-storage";
import { initQuickObservation } from "./quick-observation";
import type { BackupDiscovery, BackupItem, BackupPreview, BackupSource, BackupStatus, CommandResult } from "./backup-types";
import { initLocalClassSelector, type LocalClassStorage } from "./local-class-selector";
import { captureLocalClassRequest, currentLocalClass, isCurrentLocalClassRequest, setCurrentLocalClass } from "./local-class-context";

declare const __APP_VERSION__: string;

const APP_VERSION = String(__APP_VERSION__ || "").trim() || "0.0.0";
const designPreview = new URLSearchParams(window.location.search).get("designPreview");

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
let backupPreviewGeneration = 0;
let backupRestoreMessage = "";
let backupRestoreTone: BadgeTone = "neutral";
const busyActions = new Set<ActionName>();
let sharedArchive = { refresh: async () => undefined as void, canLeave: () => true };

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
  if (id === "backupBadge") { const icon=document.querySelector<HTMLElement>(".backup-health-icon i"); if(icon)icon.className=`fa-solid ${tone === "ok" ? "fa-check" : tone === "error" || tone === "warning" ? "fa-triangle-exclamation" : "fa-arrows-rotate"}`; }
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

function validBackupPreview() {
  return backupPreview?.ok === true && backupPreview.manifestPath === selectedBackupManifestPath && backupPreview.tenantId === currentBackupTenantId() && backupList.some(row => row.manifestPath === selectedBackupManifestPath && (!row.tenantId || row.tenantId === currentBackupTenantId()));
}

function refreshActionStates() {
  const syncUnavailable = !cloudSyncSnapshot?.connected || isCredentialMissing(cloudSyncSnapshot);
  const backupUnavailable = !backupSnapshot?.configured || !currentBackupTenantId();
  const restoreUnavailable = !validBackupPreview();
  const disabledByAction: Partial<Record<ActionName, boolean>> = {
    "run-sync": syncUnavailable,
    "run-backup": backupUnavailable,
    "restore-backup": restoreUnavailable,
  };
  (["open-settings", "open-data-directory", "refresh-status", "run-sync", "run-backup", "choose-backup-folder", "restore-backup"] as ActionName[]).forEach((action) => {
    actionButtons(action).forEach((button) => {
      button.disabled = isDeskRestoreBlocked() || busyActions.has(action) || Boolean(disabledByAction[action]);
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

function currentBackupTenantId() {
  return currentLocalClass() || (designPreview ? byId<HTMLInputElement>("backupTenantInput").value.trim() : "");
}

function setBackupRestoreMessage(message: string, tone: BadgeTone = "neutral") {
  backupRestoreMessage = message;
  backupRestoreTone = tone;
}

function renderSummary() {
  renderDeskSummary({ appVersion: APP_VERSION, serviceSnapshot, serviceLoadError, cloudSyncSnapshot, deviceConnectionSnapshot, cloudSyncLoadError, backupSnapshot, backupLoadError });
}

async function loadStatus() {
  const request = captureLocalClassRequest();
  serviceLoadError = "";
  const status = await invoke<ServiceStatus>("get_service_status", { tenantId: request.tenantId || null });
  if (!isCurrentLocalClassRequest(request)) return;
  serviceSnapshot = status;
  classSelector.renderStorage(status.storage);
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
  const request = captureLocalClassRequest();
  const result = await invoke<DeviceConnectionStatus>("get_device_connection_status", { tenantId: request.tenantId || null });
  if (!isCurrentLocalClassRequest(request) || (request.tenantId && result.tenantId && result.tenantId !== request.tenantId)) return;
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
  if (status?.disabledReason === "legacy_cloud_sync_disabled") {
    setBadge("syncBadge", "사용 안함", "neutral");
    setText("cloudSyncText", "Mac에서는 이전 Firebase 자동 수거를 사용하지 않습니다.");
    setText("cloudSyncMetaText", "-");
    setText("syncModeText", "V3 로컬 저장소");
    for (const id of ["syncImportedCount", "syncPendingCount", "syncFailedCount", "syncServerCount"]) setText(id, "0");
    setText("syncStatus", "PC 연결과 OneDrive 기기 동기화 상태는 별도로 확인합니다.");
    setHealthPanelState("syncCard", "ok");
    setHidden("healthSyncSettingsAction", true);
    renderSummary();
    refreshActionStates();
    return;
  }
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
  const request = captureLocalClassRequest();
  cloudSyncLoadError = "";
  const status = await invoke<CloudSyncStatus>("get_cloud_sync_status");
  if (!isCurrentLocalClassRequest(request) || (request.tenantId && status.tenantId && status.tenantId !== request.tenantId)) return;
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
  if (isDeskRestoreBlocked()) return;
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
  } else if (!latestBackupTime(status)) {
    setHealthPanelState("healthBackupCard", "warning"); setBadge("backupBadge", "첫 백업 필요", "warning"); setBadge("healthBackupBadge", "백업 확인 전", "warning");
    setText("backupStatus", "폴더 설정은 완료되었습니다. 첫 백업을 만든 뒤 결과를 확인해 주세요."); setText("healthBackupText", "완료된 백업이 아직 없습니다.");
    setBackupFolderActionLabels("백업 폴더 변경"); setHidden("healthBackupRunAction", false); setHidden("healthBackupFolderAction", true);
  } else {
    setHealthPanelState("healthBackupCard", "ok");
    setBadge("backupBadge", "자동 백업 정상", "ok");
    setBadge("healthBackupBadge", "정상", "ok");
    setText("backupStatus", "확인한 백업과 첨부파일 복사 결과가 정상입니다.");
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
  renderRestorePanel({ tenantId: currentBackupTenantId(), configured: backupSnapshot?.configured === true, backups: backupList, selectedManifestPath: selectedBackupManifestPath, preview: backupPreview, message: backupRestoreMessage, tone: backupRestoreTone, busy: isDeskRestoreBlocked(), currentPcName: serviceSnapshot?.pcName });
  refreshActionStates();
}

async function loadBackupList(tenantId: string) {
  const request = captureLocalClassRequest();
  const payload = await invoke<{ ok?: boolean; backups?: BackupItem[]; error?: string }>("list_local_backups", {
    tenantId,
    limit: 10,
  });
  if (!isCurrentLocalClassRequest(request) || tenantId !== currentBackupTenantId()) return;
  backupList = normalizeBackupList(payload?.backups || []);
  if (!backupList.some((backup) => backup.manifestPath === selectedBackupManifestPath)) {
    selectedBackupManifestPath = backupList[0]?.manifestPath || "";
    backupPreview = null;
  }
}

async function loadBackupStatus() {
  const request = captureLocalClassRequest();
  backupLoadError = "";
  const tenantId = currentBackupTenantId();
  if (!tenantId) {
    backupList = [];
    selectedBackupManifestPath = "";
    backupPreview = null;
    renderBackupStatus({ ok: true, configured: false });
    backupStorage.clear();
    return;
  }
  const status = await invoke<BackupStatus>("get_backup_status", { tenantId });
  if (!isCurrentLocalClassRequest(request)) return;
  await loadBackupList(tenantId).catch(() => {
    if (isCurrentLocalClassRequest(request)) backupList = normalizeBackupList(status.backups || (status.latestBackup ? [status.latestBackup] : []));
  });
  if (!isCurrentLocalClassRequest(request)) return;
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
  backupStorage.error(backupLoadError);
}

function applyBackupDiscovery(discovery: BackupDiscovery, selectedFolder: string) {
  const tenants = Array.isArray(discovery.tenants) ? discovery.tenants : [];
  const currentTenant = currentBackupTenantId();
  const detected = tenants.find((tenant) => tenant.tenantId === currentTenant)
    || null;
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
  if (isDeskRestoreBlocked()) return;
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
      setText("backupStatus", "위의 현재 학급에서 승인된 학급을 선택한 뒤 백업 폴더를 다시 선택하세요.");
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
      backupStorage.invalidate();
      void backupStorage.refresh(true);
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
  const generation = ++backupPreviewGeneration;
  if (!tenantId || !manifestPath) return;
  try {
    const preview = await invoke<BackupPreview>("preview_local_backup_restore", { tenantId, manifestPath });
    if (generation !== backupPreviewGeneration || tenantId !== currentBackupTenantId() || manifestPath !== selectedBackupManifestPath) return;
    if (!preview?.ok || preview.tenantId !== tenantId || preview.manifestPath !== manifestPath) {
      backupPreview = null;
      setBackupRestoreMessage(`미리보기 실패: ${preview?.error || "backup_restore_preview_failed"}`, "error");
    } else if (selectedBackupManifestPath === manifestPath) {
      backupPreview = preview;
      if (backupRestoreTone !== "ok") setBackupRestoreMessage("", "neutral");
    }
  } catch (error) {
    if (generation !== backupPreviewGeneration || tenantId !== currentBackupTenantId() || manifestPath !== selectedBackupManifestPath) return;
    backupPreview = null;
    setBackupRestoreMessage(`미리보기 실패: ${String((error as Error)?.message || error)}`, "error");
  }
  renderBackupRestorePanel();
}

function selectBackupManifest(manifestPath: string) {
  if (isDeskRestoreBlocked()) return;
  const safePath = String(manifestPath || "").trim();
  if (!safePath || safePath === selectedBackupManifestPath) return;
  selectedBackupManifestPath = safePath;
  backupPreview = null;
  setBackupRestoreMessage("", "neutral");
  renderBackupRestorePanel();
  void loadBackupPreview(safePath);
}

async function runBackupNow() {
  if (isDeskRestoreBlocked()) return;
  if (busyActions.has("run-backup")) return;
  const tenantId = currentBackupTenantId();
  if (!tenantId) {
    setText("backupStatus", "위의 현재 학급에서 사용할 학급을 선택하세요. 연결된 학급이 없으면 교사 홈에서 이 PC 연결을 승인해 주세요.");
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
    backupStorage.invalidate();
    void backupStorage.refresh(true);
  } catch (error) {
    renderBackupStatus({ ok: false, configured: false, error: String((error as Error)?.message || error) });
  } finally {
    setActionBusy("run-backup", false);
  }
}

async function restoreSelectedBackup() {
  if (isDeskRestoreBlocked() || busyActions.has("restore-backup") || !await canLeaveWorkspace()) return;
  const tenantId = currentBackupTenantId();
  const manifestPath = selectedBackupManifestPath;
  if (!tenantId || !manifestPath || !validBackupPreview()) {
    setBackupRestoreMessage("복원할 백업의 미리보기 검증을 먼저 완료해 주세요.", "warning");
    renderBackupRestorePanel();
    return;
  }
  const selected = backupList.find((backup) => backup.manifestPath === manifestPath);
  const sourceText = backupSourceSummary(backupPreview?.source || selected?.source, serviceSnapshot?.pcName);
  const ok = await confirmBackupRestore({
    kind: backupKindLabel(selected?.kind),
    date: formatBackupDateTime(backupPreview?.createdAtMs || selected?.createdAtMs),
    source: sourceText,
    summary: "선택한 백업의 자료와 첨부파일을 현재 PC에 병합합니다.",
  });
  if (!ok || tenantId !== currentBackupTenantId() || manifestPath !== selectedBackupManifestPath || !validBackupPreview() || !await canLeaveWorkspace()) return;
  let releaseRestore: (() => void) | undefined;
  let commandSucceeded = false;
  const requestId = crypto.randomUUID();
  setActionBusy("restore-backup", true);
  setBackupRestoreMessage("현재 상태 보호 백업을 만든 뒤 선택한 백업을 병합하는 중입니다.", "neutral");
  renderBackupRestorePanel();
  try {
    await restoreProgress.prepare();
    if (tenantId !== currentBackupTenantId() || manifestPath !== selectedBackupManifestPath || !validBackupPreview() || isDeskRestoreBlocked()) throw new Error("backup_restore_selection_changed");
    releaseRestore = beginDeskRestore();
    restoreProgress.begin(requestId); renderBackupRestorePanel();
    const result = await invoke<{ ok?: boolean; imported?: number; mediaRestored?: number; mediaMissing?: number; safetyBackup?: object; error?: string }>("restore_local_backup", {
      tenantId,
      manifestPath, requestId,
    });
    restoreProgress.finish(result); commandSucceeded = result?.ok === true;
    if (!result?.ok) {
      const error = String(result?.error || "backup_restore_failed");
      setBackupRestoreMessage(
        error.startsWith("pre_restore_backup_failed:")
          ? "현재 상태 보호 백업을 만들지 못해 복원을 시작하지 않았습니다. OneDrive 연결과 남은 용량을 확인한 뒤 다시 시도하세요."
          : `복원 실패: ${error}`,
        "error",
      );
    } else {
      setBackupRestoreMessage(`보호 백업 후 이 PC 복원 완료: DB 반영 ${numberText(result.imported)}건, 첨부 복원 ${numberText(result.mediaRestored)}개${numeric(result.mediaMissing) ? `, 누락 ${numberText(result.mediaMissing)}개` : ""}. 기기 동기화 완료와는 별개입니다. 서버 최신 세대 반영과 이 PC 변경 게시 상태를 확인해 주세요.`, "ok");
      await loadBackupStatus();
      await loadDeviceSyncStatus().catch(() => renderDeviceSyncStatus(null));
      renderSummary();
      await Promise.all([localWorkspaces.refresh(), dataExplorer.refresh(), studentTimeline.refresh(), sharedArchive.refresh(), loadHomeOverview(tenantId)]);
      backupStorage.invalidate();
      void backupStorage.refresh(true);
    }
  } catch (error) {
    if (!commandSucceeded) restoreProgress.finish(null, error);
    setBackupRestoreMessage(`${commandSucceeded ? "이 PC 복원은 완료했지만 후속 상태 조회를 완료하지 못했습니다. 상태를 새로 확인하세요. " : "복원 실패: "} ${String((error as Error)?.message || error)}`, "error");
  } finally {
    restoreProgress.release(); releaseRestore?.();
    setActionBusy("restore-backup", false);
    renderBackupRestorePanel();
  }
}

async function refreshAll() {
  await classSelector.refresh();
  await refreshSelectedClass();
}

async function refreshSelectedClass() {
  const request = captureLocalClassRequest();
  setActionBusy("refresh-status", true);
  try {
    await loadStatus().catch(error => { if (isCurrentLocalClassRequest(request)) renderServiceLoadError(error); });
    if (!isCurrentLocalClassRequest(request)) return;
    await loadCloudSyncStatus().catch(error => { if (isCurrentLocalClassRequest(request)) renderCloudSyncLoadError(error); });
    if (!isCurrentLocalClassRequest(request)) return;
    await loadDeviceConnectionStatus().catch(() => undefined);
    await desktopShell.refreshConnection().catch(() => undefined);
    await loadBackupStatus().catch(error => { if (isCurrentLocalClassRequest(request)) renderBackupLoadError(error); });
    await loadDeviceSyncStatus().catch(() => renderDeviceSyncStatus(null));
    renderSummary();
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
      loadBackupStatus().then(() => {
        backupStorage.invalidate();
        void backupStorage.refresh(true);
      }),
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
    if (isDeskRestoreBlocked()) return;
    ++backupPreviewGeneration;
    backupStorage.clear();
    backupList = [];
    selectedBackupManifestPath = "";
    backupPreview = null;
    setBackupRestoreMessage("", "neutral");
    loadBackupStatus().then(() => backupStorage.refresh()).catch(renderBackupLoadError);
  });
}

const restoreProgress = createBackupRestoreProgress({ onChange: renderRestoreProgress });
mountDeskViews();
initDeskPageGuide();
window.addEventListener("desk:restore-lock-changed", refreshActionStates);
const canLeaveWorkspace = async () => !isDeskRestoreBlocked() && sharedArchive.canLeave() && await quickObservation.canLeave() && await documentWorkspace.canLeave() && await deskRecordEditor.canLeave();
// A restored teacher tab must wait until the document controllers below finish mounting.
const desktopShell = initDesktopShell({ beforeLeave: () => Promise.resolve().then(canLeaveWorkspace) });
const quickObservation = initQuickObservation({
  getTenantId: currentBackupTenantId,
  onConnect: () => document.getElementById("desktopTeacherHome")?.click(),
});
initArchiveBoardExplorer();
initWorkNoteReader();
const { dataExplorer, studentTimeline } = initRecordBrowsers(currentBackupTenantId, () => quickObservation.canLeave());
const deskRecordEditor = initDeskRecordEditor({ getTenantId: currentBackupTenantId, onChanged: () => { void dataExplorer.refresh(); void studentTimeline.refresh(); void loadHomeOverview(currentBackupTenantId()); } });
initDeviceSyncConflicts({ getTenantId: currentBackupTenantId });
const { localWorkspaces, documentWorkspace } = initTeacherDeskDocuments({ getTenantId: currentBackupTenantId, openTeacherHome: desktopShell.openTeacherHome, navigate: (view, context) => homeDashboard.navigate(view, context) });
const backupStorage = initBackupStorage({
  getTenantId: currentBackupTenantId,
  isConfigured: () => designPreview === "backup" || backupSnapshot?.configured === true, onBackupsChanged: () => loadBackupStatus().catch(renderBackupLoadError),
});
const homeDashboard = initHomeDashboard({
  beforeViewChange: canLeaveWorkspace,
  async onViewChange(view, context) {
    studentPanel.onViewChange(view);
    if (view === "quick-observation") await quickObservation.open({ focus: true });
    if (view === "lesson-materials" || view === "work-materials" || view === "student-learning-materials") await localWorkspaces.open(view);
    if (view === "data") await dataExplorer.open({ group: context.group, sectionKey: context.sectionKey, hasAttachment: context.attachment, resetFilters:context.resetFilters });
    if (view === "students") await studentTimeline.open();
    if (view === "backup") await backupStorage.refresh();
  },
  onSearch(query) {
    return dataExplorer.open({ query, resetFilters:true });
  },
});
const studentPanel = initDeskStudentPanel({ quickObservation, studentTimeline, navigate: view => homeDashboard.navigate(view) });
const classSelector = initLocalClassSelector({
  canLeave: async () => !busyActions.size && !isDeviceSyncRunning() && await canLeaveWorkspace() && documentWorkspace.prepareClassChange(),
  openTeacherHome: desktopShell.openTeacherHome,
  async onSelected(tenantId) {
    if (tenantId === currentLocalClass()) return;
    await documentWorkspace.resetForClass();
    if (!setCurrentLocalClass(tenantId)) return;
    byId<HTMLInputElement>("backupTenantInput").value = tenantId;
    ++backupPreviewGeneration;
    backupStorage.clear();
    backupList = []; selectedBackupManifestPath = ""; backupPreview = null;
    backupSnapshot = null; cloudSyncSnapshot = null; deviceConnectionSnapshot = null;
    setBackupRestoreMessage("", "neutral");
    renderDeviceSyncStatus(null);
    window.dispatchEvent(new CustomEvent("desk:class-changed", { detail: { tenantId } }));
    await desktopShell.resetTeacherHome();
    await Promise.allSettled([refreshSelectedClass(), localWorkspaces.refresh(), dataExplorer.refresh(), studentTimeline.resetForClass(), sharedArchive.refresh(), loadHomeOverview(tenantId)]);
    await homeDashboard.navigate("home");
  },
});
bindTeacherDeskLifecycle({ getTenantId: currentBackupTenantId, desktopShell, homeDashboard, canLeave: canLeaveWorkspace, waitForPaint });
bindUi();
if (designPreview !== "settings") {
  initSettingsDashboard({ onDisconnected: refreshAll, onAuthorizeBrowser: () => deviceAuthorization.start(), beforeDisconnect: () => classSelector.canChange() });
}
renderAppVersion();
if (designPreview === "archive") initSharedArchivePreview();
else sharedArchive = initSharedArchive({ getTenantId: currentBackupTenantId });
if (designPreview === "auth") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="settings"]')?.click();
  deviceAuthorization.render({ ok: true, status: "pending", expiresAtMs: Date.now() + 10 * 60 * 1000 });
} else if (designPreview === "data") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="data"]')?.click();
} else if (designPreview === "quick-observation") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="quick-observation"]')?.click();
} else if (designPreview === "lesson-materials" || designPreview === "work-materials" || designPreview === "student-learning-materials") {
  document.querySelector<HTMLButtonElement>(`.sidebar-link[data-app-view-target="${designPreview}"]`)?.click();
} else if (designPreview === "students") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="students"]')?.click();
} else if (designPreview === "backup") {
  document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="backup"]')?.click();
  initBackupRestorePreview();
  void backupStorage.refresh();
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
