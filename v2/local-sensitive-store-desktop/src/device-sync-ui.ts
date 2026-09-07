import { invoke } from "@tauri-apps/api/core";

export type DeviceSyncStatus = {
  ok: boolean;
  connected: boolean;
  tenantId?: string;
  credentialAvailable?: boolean;
  oneDriveConfigured?: boolean;
  backupError?: string | null;
  appliedGeneration?: number;
  publishedGeneration?: number;
  latestGeneration?: number;
  latestStatus?: string;
  hasUnsyncedChanges?: boolean;
  lastSuccessAtMs?: number;
  lastError?: string;
  conflictCount?: number;
  conflictRetainedCount?: number;
  conflictUnreviewedCount?: number;
  conflictLifetimeCount?: number;
  waitingForOneDrive?: boolean;
  error?: string;
};

let snapshot: DeviceSyncStatus | null = null;
let syncRun: Promise<void> | null = null;
let statusRevision = 0;
const ONE_DRIVE_DOWNLOAD_PENDING_MESSAGE = "최신 세대에 필요한 OneDrive 파일만 다운로드 요청하고 있습니다. 준비되면 무결성을 검증한 뒤 자동으로 반영합니다. 기다리는 동안 현재 로컬 자료는 유지됩니다.";

function isOneDriveDownloadPending(error?: string) {
  return /^(onedrive_download_pending|onedrive_snapshot_pending)(?::|$)/u.test(String(error || ""));
}

function element(id: string) {
  const target = document.getElementById(id);
  if (!target) throw new Error(`missing element: ${id}`);
  return target;
}

function setText(id: string, value: string) {
  element(id).textContent = value || "-";
}

function setBadge(label: string, tone: "ok" | "warning" | "error") {
  const badge = element("deviceSyncBadge");
  badge.textContent = label;
  badge.className = `status-badge badge-${tone}`;
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

function verificationLabel(status: DeviceSyncStatus) {
  if (!Number(status.latestGeneration || 0)) return "초기 상태";
  if (status.latestStatus === "verified") return "다른 기기 확인됨";
  if (status.latestStatus === "announced") return "검증 대기";
  return status.latestStatus || "확인 중";
}

export function deviceSyncErrorMessage(error?: string) {
  const value = String(error || "");
  if (isOneDriveDownloadPending(value)) return ONE_DRIVE_DOWNLOAD_PENDING_MESSAGE;
  if (/^onedrive_download_failed(?::|$)/u.test(value)
      || (value.startsWith("backup_hash_read_failed:") && value.includes("(os error 380)"))) {
    return "OneDrive에서 필요한 파일을 내려받지 못했습니다. OneDrive 연결·로그인 상태를 확인한 뒤 지금 동기화를 다시 눌러 주세요. 현재 로컬 자료는 유지됩니다.";
  }
  if (value.startsWith("restore_sync_merge_failed:work_note_attachments:")) {
    return "업무노트와 첨부파일의 연결 순서를 확인하지 못했습니다. 최신 앱에서 다시 동기화해 주세요. 복원 전 보호 백업과 현재 자료는 유지됩니다.";
  }
  if (value.startsWith("restore_sync_merge_failed:counseling_teacher_notes:")) {
    return "상담 기록과 교사 메모의 연결 순서를 확인하지 못했습니다. 최신 앱에서 다시 동기화해 주세요. 복원 전 보호 백업과 현재 자료는 유지됩니다.";
  }
  if (value.startsWith("archive_sync_") || value.startsWith("backup_apply_index_")) {
    return "OneDrive의 보관본 무결성을 확인하지 못해 적용과 기기 확인을 중단했습니다. 현재 자료는 유지됩니다. OneDrive 동기화가 끝난 뒤 다시 시도해 주세요.";
  }
  return value;
}

function updateActionState(busy = false) {
  busy = busy || syncRun !== null;
  const unavailable = !snapshot?.connected || !snapshot.credentialAvailable || !snapshot.oneDriveConfigured || Boolean(snapshot.backupError);
  document.querySelectorAll<HTMLButtonElement>('button[data-action="run-device-sync"]').forEach((button) => {
    button.disabled = busy || unavailable;
  });
  document.querySelectorAll<HTMLButtonElement>('button[data-action="repair-device-sync"]').forEach((button) => {
    button.hidden = snapshot?.connected === true && snapshot.credentialAvailable === true;
    button.disabled = busy;
  });
}

export function renderDeviceSyncStatus(status: DeviceSyncStatus | null) {
  snapshot = status;
  setText("deviceSyncLatestText", status?.connected ? `${Number(status.latestGeneration || 0)}세대` : "-");
  setText("deviceSyncAppliedText", status?.connected ? `${Number(status.appliedGeneration || 0)}세대` : "-");
  setText("deviceSyncVerifiedText", status?.connected ? verificationLabel(status) : "-");
  setText("deviceSyncConflictText", status?.connected
    ? `미검토 ${Number(status.conflictUnreviewedCount || 0)} · 보관 ${Number(status.conflictRetainedCount ?? status.conflictCount ?? 0)} · 누적 ${Number(status.conflictLifetimeCount ?? status.conflictCount ?? 0)}`
    : "-");
  if (status?.ok === false && status.error) {
    const pending = isOneDriveDownloadPending(status.error);
    setBadge(pending ? "OneDrive 다운로드 대기" : "확인 필요", pending ? "warning" : "error");
    setText("deviceSyncStatus", pending
      ? ONE_DRIVE_DOWNLOAD_PENDING_MESSAGE
      : `기기 동기화 상태를 확인하지 못했습니다: ${deviceSyncErrorMessage(status.error)}`);
  } else if (!status?.connected) {
    setBadge("PC 연결 필요", "warning");
    setText("deviceSyncStatus", "교사 설정에서 이 PC를 연결하면 OneDrive 최신 내용을 자동으로 맞춥니다.");
  } else if (!status.credentialAvailable) {
    setBadge("재연결 필요", "warning");
    setText("deviceSyncStatus", "기기 동기화 자격 증명을 확인할 수 없습니다. 교사 설정에서 PC를 다시 연결해 주세요.");
  } else if (status.backupError) {
    setBadge("백업 폴더 확인 필요", "warning");
    setText("deviceSyncStatus", "PC 연결은 완료되었습니다. 백업 폴더에 접근할 수 없어 기기 간 동기화만 보류합니다. 로컬 자료는 계속 사용할 수 있으며, 백업·복원에서 폴더 연결과 접근 권한을 확인해 주세요.");
  } else if (!status.oneDriveConfigured) {
    setBadge("OneDrive 설정 필요", "warning");
    setText("deviceSyncStatus", "학교 OneDrive 안의 백업 폴더를 선택하면 자동 동기화를 시작합니다.");
  } else if (status.lastError) {
    const pending = isOneDriveDownloadPending(status.lastError);
    setBadge(pending ? "OneDrive 다운로드 대기" : "확인 필요", pending ? "warning" : "error");
    setText("deviceSyncStatus", pending
      ? ONE_DRIVE_DOWNLOAD_PENDING_MESSAGE
      : `마지막 동기화 문제: ${deviceSyncErrorMessage(status.lastError)}`);
  } else if (status.waitingForOneDrive) {
    setBadge("OneDrive 다운로드 대기", "warning");
    setText("deviceSyncStatus", ONE_DRIVE_DOWNLOAD_PENDING_MESSAGE);
  } else if (status.hasUnsyncedChanges) {
    setBadge("변경 내용 대기", "warning");
    setText("deviceSyncStatus", "이 PC의 최근 변경 내용을 잠시 모은 뒤 자동으로 새 세대에 반영합니다.");
  } else if (status.latestStatus === "announced") {
    setBadge("다른 기기 확인 대기", "warning");
    setText("deviceSyncStatus", "최신 내용과 보관본은 OneDrive에 저장되었습니다. 다른 기기가 확인하면 검증 완료로 바뀝니다. 충돌 건수는 동기화를 막지 않는 누적 보관 기록입니다.");
  } else {
    setBadge("최신 상태", "ok");
    setText("deviceSyncStatus", status.lastSuccessAtMs
      ? `${formatDateTime(status.lastSuccessAtMs)}에 자료와 보관본의 최신 상태를 확인했습니다. 충돌 건수는 누적 보관 기록입니다.`
      : "현재 PC와 OneDrive의 자료·보관본이 일치합니다. 충돌 건수는 누적 보관 기록입니다.");
  }
  updateActionState();
}

export async function loadDeviceSyncStatus() {
  const requestRevision = ++statusRevision;
  try {
    const status = await invoke<DeviceSyncStatus>("get_device_sync_status");
    if (!syncRun && requestRevision === statusRevision) renderDeviceSyncStatus(status);
  } catch (error) {
    if (!syncRun && requestRevision === statusRevision) throw error;
  }
}

export async function runDeviceSyncNow(afterRun: () => Promise<unknown>) {
  if (syncRun) return syncRun;
  statusRevision += 1;
  updateActionState(true);
  setText("deviceSyncStatus", "필요한 OneDrive 파일을 요청하고 최신 세대를 확인하고 있습니다. 다운로드가 준비되면 검증한 뒤 반영합니다.");
  syncRun = (async () => {
    try {
      const status = await invoke<DeviceSyncStatus>("run_device_sync_now");
      if (!status?.ok) throw new Error(status?.error || "device_sync_failed");
      renderDeviceSyncStatus(status);
      try { await afterRun(); }
      catch {
        if (status.lastError || status.waitingForOneDrive) renderDeviceSyncStatus(status);
        else setText("deviceSyncStatus", "일부 화면을 새로 읽지 못했습니다. 상태 확인을 눌러 다시 확인해 주세요.");
      }
    } catch (error) {
      const message = String((error as Error)?.message || error || "device_sync_failed");
      renderDeviceSyncStatus({ ...(snapshot || { connected: false }), ok: false, error: message, lastError: message });
    } finally {
      statusRevision += 1;
      syncRun = null;
      updateActionState();
    }
  })();
  return syncRun;
}
