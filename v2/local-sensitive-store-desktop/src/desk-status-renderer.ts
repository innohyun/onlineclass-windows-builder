import type { DeviceAuthorizationResult } from "./device-authorization";
import type { BackupStatus } from "./backup-types";
import { backupFolderLabel } from "./backup-display-utils";
import { getDeviceSyncPresentation } from "./device-sync-ui";
import { renderHomeStatus } from "./home-dashboard";
import { renderSettingsDashboard } from "./settings-dashboard";

export type ServiceStatus = {
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

export type CloudSyncStatus = {
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
  disabledReason?: string;
};

export type DeviceConnectionStatus = DeviceAuthorizationResult & {
  connected?: boolean;
  uid?: string;
  connectedAtMs?: number;
};

function byId(id:string):HTMLElement { const element = document.getElementById(id); if (!element) throw new Error(`missing element: ${id}`); return element; }
function setText(id:string,value:string) { byId(id).textContent = value || "-"; }
function numeric(value?:number) { return Number(value || 0) || 0; }
function numberText(value?:number) { return String(numeric(value)); }
function setBadge(id:string,label:string,tone:string) { const el=byId(id); el.textContent=label; el.className=`status-badge badge-${tone}`; }

export function formatDateTime(ms?: number) {
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

export function tenantLabel(status?: Pick<CloudSyncStatus, "tenantName" | "tenantId"> | null) {
  return status?.tenantName || status?.tenantId || "연결된 학급 없음";
}

export function accountLabel(status?: Pick<CloudSyncStatus, "accountEmail" | "accountDisplayName" | "uid"> | null) {
  return status?.accountEmail || status?.accountDisplayName || status?.uid || "-";
}

export function normalizeStorageMode(value?: string) {
  const mode = String(value || "").trim();
  if (mode === "hybrid_firestore_local_keep_remote") return mode;
  if (mode === "hybrid_firestore_local") return mode;
  if (mode === "local_sqlite") return mode;
  if (mode === "firestore") return mode;
  return "";
}

export function cloudSyncModeLabel(value?: string) {
  const mode = normalizeStorageMode(value);
  if (mode === "hybrid_firestore_local_keep_remote") return "PC로 옮기고 서버에는 처리완료 표시";
  if (mode === "hybrid_firestore_local") return "PC로 옮긴 뒤 서버 임시본 삭제";
  if (mode === "local_sqlite") return "이 PC에 직접 저장";
  if (mode === "firestore") return "서버에만 저장";
  return "저장 방식 확인 중";
}

export function isCredentialMissing(status?: CloudSyncStatus | null) {
  return status?.credentialMissing === true
    || status?.needsReconnect === true
    || status?.lastErrorCode === "credential_missing"
    || String(status?.lastError || "").startsWith("keyring_get_failed:");
}

export function reconnectMessage(status?: CloudSyncStatus | null) {
  return status?.reconnectMessage
    || "브라우저 로그인 정보가 만료되어 자동 수거가 멈춰 있습니다. 아래 다시 연결하기를 누르면 교사 설정 화면으로 이동합니다.";
}

export function credentialStorageLabel(value?: string) {
  const storage = String(value || "");
  if (storage.includes("windows_dpapi_file")) return "자동 연결(암호화 보관)";
  if (storage.includes("macos_file")) return "자동 연결(로컬 보조 보관)";
  return "자동 연결";
}

export function latestSyncTime(status?: CloudSyncStatus | null) {
  return status?.lastSyncAtMs || status?.lastRunAtMs || 0;
}

export function latestBackupTime(status?: BackupStatus | null) {
  return status?.latestBackup?.createdAtMs || 0;
}

export function renderDeskSummary(input: {
  appVersion: string; serviceSnapshot: ServiceStatus | null; serviceLoadError: string;
  cloudSyncSnapshot: CloudSyncStatus | null; deviceConnectionSnapshot: DeviceConnectionStatus | null;
  cloudSyncLoadError: string; backupSnapshot: BackupStatus | null; backupLoadError: string;
}) {
  const { appVersion, serviceSnapshot, serviceLoadError, cloudSyncSnapshot, deviceConnectionSnapshot, cloudSyncLoadError, backupSnapshot, backupLoadError } = input;
  const device = getDeviceSyncPresentation();
  const serviceFailed = Boolean(serviceLoadError) || serviceSnapshot?.ok === false;
  const summaryCard = byId("summaryCard");
  const pendingKnown = typeof cloudSyncSnapshot?.lastPending === "number" && Number.isFinite(cloudSyncSnapshot.lastPending);
  const pending = pendingKnown ? numeric(cloudSyncSnapshot!.lastPending) : undefined;
  const failed = numeric(cloudSyncSnapshot?.lastFailed) + numeric(cloudSyncSnapshot?.lastConflicts);
  const backupMedia = backupSnapshot?.latestBackup?.media || backupSnapshot?.lastResult?.media;
  const backupHasError = backupSnapshot?.ok === false
    || backupSnapshot?.lastResult?.ok === false
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
  } else if (!serviceSnapshot || (!cloudSyncSnapshot && !deviceConnectionSnapshot && !cloudSyncLoadError) || (!backupSnapshot && !backupLoadError)) {
    tone = "is-checking";
  } else if (!deviceConnectionSnapshot?.connected && (!cloudSyncSnapshot?.connected || isCredentialMissing(cloudSyncSnapshot))) {
    tone = "is-warning";
    title = "재연결이 필요합니다.";
    description = "브라우저 로그인 정보가 만료되어 자동 수거가 멈춰 있습니다. 다시 연결하면 수거가 재개됩니다.";
  } else if (cloudSyncLoadError || failed || backupHasError) {
    tone = "is-error";
    title = "확인이 필요한 문제가 있습니다.";
    description = "아래 카드의 실패 항목과 해결 안내를 확인하세요.";
  } else if (!backupConfigured || !latestBackupTime(backupSnapshot)) {
    tone = "is-warning";
    title = backupConfigured ? "첫 백업을 확인해 주세요." : "백업 폴더 설정이 필요합니다.";
    description = backupConfigured ? "백업 폴더는 설정되었지만 완료된 백업은 아직 확인되지 않았습니다." : "로컬 저장과 별도로 백업 폴더를 설정해 주세요.";
  } else if (device.tone !== "ok") {
    tone = device.tone === "error" ? "is-error" : "is-warning";
    title = device.label;
    description = device.detail;
  } else {
    tone = "is-ok";
    title = "확인한 저장·백업 상태가 정상입니다";
    description = "민감기록 저장, 임시 기록 수거, 백업이 안전하게 작동하고 있습니다.";
  }

  summaryCard.className = `health-summary ${tone}`;
  setText("summaryTitle", title);
  setText("summaryDescription", description);
  setText("summaryTenantText", tenantLabel(deviceConnectionSnapshot?.connected ? deviceConnectionSnapshot : cloudSyncSnapshot));
  setText("summarySyncText", formatDateTime(latestSyncTime(cloudSyncSnapshot)));
  setText("summaryBackupText", formatDateTime(latestBackupTime(backupSnapshot)));
  setText("summaryPendingText", pendingKnown && !serviceFailed ? `${numberText(pending)}건` : "—");
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
    healthy: serviceSnapshot?.ok === true && !serviceFailed,
    storeReady: serviceSnapshot?.ok === true && !serviceFailed,
    tenantLabel: tenantLabel(deviceConnectionSnapshot?.connected ? deviceConnectionSnapshot : cloudSyncSnapshot),
    syncAtMs: latestSyncTime(cloudSyncSnapshot),
    backupAtMs: serviceFailed || backupLoadError || !backupSnapshot ? undefined : latestBackupTime(backupSnapshot),
    pending: serviceFailed || !cloudSyncSnapshot ? undefined : pending,
    deviceName: serviceSnapshot?.pcName,
    appVersion, os: serviceFailed ? undefined : serviceSnapshot?.os, arch: serviceFailed ? undefined : serviceSnapshot?.arch,
    backupConfigured: serviceFailed ? undefined : backupSnapshot?.configured, backupOk: serviceFailed || backupLoadError || backupHasError ? false : backupSnapshot?.ok,
    deviceSync: serviceFailed ? undefined : {label:device.label, detail:device.detail, tone:device.tone},
  });
  const storeTone = serviceFailed ? "error" : serviceSnapshot?.ok ? "ok" : "neutral";
  setBadge("healthStoreBadge", serviceFailed ? "확인 실패" : serviceSnapshot?.ok ? "사용 가능" : "확인 중", storeTone);
  setText("healthStoreText", serviceFailed ? "서비스 응답을 확인하지 못했습니다. 다른 저장·백업 상태도 새로 확인해 주세요." : serviceSnapshot?.ok ? "이 PC의 로컬 서비스 응답을 확인했습니다." : "서비스 응답을 확인하고 있습니다.");
  setText("healthStoreLocationText", serviceFailed ? "위치 확인 전" : serviceSnapshot?.dataDir || "위치 확인 전");
  byId("healthStoreCard").className = `health-panel is-${serviceFailed ? "error" : serviceSnapshot?.ok ? "ok" : "checking"}`;
  setText("healthDeviceSyncBadge", serviceFailed ? "기기 동기화 확인 전" : device.label);
  setText("healthDeviceSyncText", serviceFailed ? "로컬 서비스 연결을 먼저 확인하세요." : device.detail);
  if (serviceFailed) {
    for (const [panel, badge] of [["connectionCard", "connectionBadge"], ["syncCard", "syncBadge"], ["healthBackupCard", "healthBackupBadge"]]) {
      byId(panel).classList.remove("is-ok", "is-warning", "is-error"); byId(panel).classList.add("is-checking"); setBadge(badge, "확인 전", "neutral");
    }
    setText("connectionTitle", "연결 상태 확인 전"); setText("connectionMetaText", "로컬 서비스 응답을 다시 확인하세요.");
    setText("cloudSyncText", "수거 결과 확인 전"); setText("healthBackupText", "백업 상태 확인 전");
    for (const id of ["syncImportedCount", "syncPendingCount", "syncFailedCount", "syncServerCount"]) setText(id, "—");
    for (const id of ["summaryPendingText", "summarySyncText", "summaryBackupText", "healthBackupLatestText", "healthBackupNextText", "healthBackupMediaText", "cloudSyncMetaText"]) setText(id, "—");
  }
  if (!serviceFailed) {
    for (const [id, value] of [["syncImportedCount",cloudSyncSnapshot?.lastImported], ["syncPendingCount",cloudSyncSnapshot?.lastPending], ["syncFailedCount",cloudSyncSnapshot?.lastFailed], ["syncServerCount",normalizeStorageMode(cloudSyncSnapshot?.observationStorageMode)==="hybrid_firestore_local_keep_remote" ? cloudSyncSnapshot?.lastMarked : cloudSyncSnapshot?.lastDeleted]] as const) {
      if (typeof value !== "number" || !Number.isFinite(value)) setText(id,"—");
    }
  }
  const connection = deviceConnectionSnapshot?.connected ? deviceConnectionSnapshot : cloudSyncSnapshot;
  const connected = deviceConnectionSnapshot?.connected === true || cloudSyncSnapshot?.connected === true;
  renderSettingsDashboard({
    connected,
    needsReconnect: isCredentialMissing(cloudSyncSnapshot),
    tenantLabel: tenantLabel(connection),
    accountLabel: accountLabel(connection),
    backupConfigured: backupSnapshot?.configured === true,
    backupOk: !serviceFailed && !backupLoadError && !backupHasError && backupSnapshot?.ok === true,
    backupVerified: Boolean(latestBackupTime(backupSnapshot)),
    backupLocation: backupSnapshot ? backupFolderLabel(backupSnapshot) : "확인 중",
    backupLatest: backupSnapshot ? formatDateTime(latestBackupTime(backupSnapshot)) : "확인 중",
    appVersion,
  });
}

