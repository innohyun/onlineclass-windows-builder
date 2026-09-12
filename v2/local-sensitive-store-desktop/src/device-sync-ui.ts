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
  syncPhase?: string;
  recoveryRequired?: boolean;
  oneDriveEvidence?: {
    state: "unknown" | "pending" | "in_sync" | "error" | "unsupported";
    checkedAtMs: number;
    requiredFileCount: number;
    inSyncFileCount?: number;
    pendingFileCount?: number;
    missingFileCount?: number;
    unknownFileCount?: number;
    errorFileCount?: number;
  };
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

function oneDriveDownloadFailureMessage(value: string) {
  const legacy380 = value.startsWith("backup_hash_read_failed:") && value.includes("(os error 380)");
  if (!legacy380 && !/^onedrive_download_failed(?::|$)/u.test(value)) return null;
  const parsed = value.match(/^onedrive_download_failed(?::([a-z_]+))?(?::(\d{1,10}|0x[\da-f]{8}))?$/iu);
  const phase = legacy380 ? "io" : parsed?.[1] || "";
  const rawCode = legacy380 ? 380 : parsed?.[2] ? Number(parsed[2]) : undefined;
  const code = rawCode === undefined || rawCode > 0xffffffff ? undefined
    : rawCode >>> 16 === 0x8007 ? rawCode & 0xffff : rawCode;
  const phaseLabels: Record<string, string> = {
    open: "다운로드 파일 열기", io: "다운로드 요청 결과", event: "다운로드 준비",
    hydrate_open: "다운로드 파일 열기", hydrate_event: "다운로드 준비",
    hydrate_request: "다운로드 요청", hydrate_wait: "다운로드 응답 대기", hydrate_io: "다운로드 요청 결과",
    read_open: "파일 읽기 재요청 준비", read_event: "파일 읽기 재요청 준비",
    read_io: "파일 읽기 재요청", read_wait: "파일 읽기 응답 대기",
    hydrate_timeout: "다운로드 시간 제한", read_timeout: "파일 읽기 시간 제한",
    deadline_thread: "다운로드 시간 제한 준비", deadline_worker: "다운로드 시간 제한 준비",
    provider_timeout: "다운로드 시간 제한", provider_wait: "다운로드 응답 대기",
  };
  let detail = "OneDrive에서 필요한 파일을 내려받지 못했습니다. OneDrive 앱의 동기화 상태를 확인한 뒤";
  if (code === 380) detail = "OneDrive가 파일 다운로드 요청을 거절했습니다. OneDrive 동기화 상태를 확인한 뒤";
  else if (code === 386) detail = "OneDrive 인증을 확인하지 못했습니다. OneDrive 계정 로그인 상태를 확인한 뒤";
  else if (code === 388) detail = "OneDrive에 연결할 수 없습니다. 인터넷 연결과 OneDrive 동기화 상태를 확인한 뒤";
  else if (code === 5 || code === 395) detail = "OneDrive 파일에 접근할 권한이 없습니다. 해당 파일의 접근 권한을 확인한 뒤";
  else if (code === 362) detail = "OneDrive 파일 공급자가 응답할 수 없습니다. OneDrive 앱이 실행 중인지 확인한 뒤";
  else if (code === 1460 || code === 426 || ["provider_timeout", "hydrate_timeout", "read_timeout"].includes(phase)) {
    detail = "OneDrive 파일 다운로드가 제한 시간 안에 끝나지 않았습니다. OneDrive 동기화 상태를 확인한 뒤";
  }
  const fallback = !legacy380 && phase.startsWith("read_") && phaseLabels[phase]
    ? "일반 파일 읽기를 통한 자동 재요청도 완료하지 못했습니다. " : "";
  const diagnostic = [phaseLabels[phase], code === undefined ? undefined : `Windows ${code}`].filter(Boolean).join(" · ");
  return `${fallback}${detail} 지금 동기화를 다시 눌러 주세요. 현재 로컬 자료는 유지됩니다.${diagnostic ? ` (${diagnostic})` : ""}`;
}

export function deviceSyncErrorMessage(error?: string, recoveryRequired?: boolean) {
  const value = String(error || "");
  if (value.includes("restore_recovery_required") && recoveryRequired === false) return "이전 동기화 시도에서 복원 파일을 확인하지 못했습니다. 현재 미완료 복구 작업은 없습니다. 지금 동기화로 다시 확인할 수 있습니다.";
  if (value.includes("restore_recovery_required")) return "중단된 복원을 안전하게 마치지 못했습니다. 원본과 복구 파일을 보존하고 이 학급의 변경·동기화를 중단했습니다. 파일을 삭제하거나 복원을 반복하지 말고 복구 상태를 확인해 주세요.";
  if (value === "lesson_plan_binding_revision_conflict") return "같은 수업 연결 버전의 값이 달라 충돌 자료를 보관하고 세대 적용·기기 확인을 중단했습니다. 웹에서 최신 수업 연결을 확인해 주세요.";
  if (isOneDriveDownloadPending(value)) return ONE_DRIVE_DOWNLOAD_PENDING_MESSAGE;
  const downloadFailure = oneDriveDownloadFailureMessage(value);
  if (downloadFailure) return downloadFailure;
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
  const unavailable = !snapshot?.connected || !snapshot.credentialAvailable || !snapshot.oneDriveConfigured || Boolean(snapshot.backupError) || snapshot.recoveryRequired === true;
  document.querySelectorAll<HTMLButtonElement>('button[data-action="run-device-sync"]').forEach((button) => {
    button.disabled = busy || unavailable;
  });
  document.querySelectorAll<HTMLButtonElement>('button[data-action="repair-device-sync"]').forEach((button) => {
    button.hidden = snapshot?.connected === true && snapshot.credentialAvailable === true;
    button.disabled = busy;
  });
}

export function renderDeviceSyncStatus(status: DeviceSyncStatus | null) {
  const failure = status?.error || status?.lastError || "";
  if (status && failure.startsWith("restore_recovery_required") && status.recoveryRequired !== false) status = { ...status, recoveryRequired: true };
  snapshot = status;
  setText("deviceSyncLatestText", status?.connected ? `${Number(status.latestGeneration || 0)}세대` : "-");
  setText("deviceSyncAppliedText", status?.connected ? `${Number(status.appliedGeneration || 0)}세대` : "-");
  setText("deviceSyncVerifiedText", status?.connected ? verificationLabel(status) : "-");
  setText("deviceSyncConflictText", status?.connected
    ? `미검토 ${Number(status.conflictUnreviewedCount || 0)} · 보관 ${Number(status.conflictRetainedCount ?? status.conflictCount ?? 0)} · 누적 ${Number(status.conflictLifetimeCount ?? status.conflictCount ?? 0)}`
    : "-");
  if (status?.recoveryRequired) {
    setBadge("복구 확인 필요", "error");
    setText("deviceSyncStatus", deviceSyncErrorMessage("restore_recovery_required"));
  } else if (status?.syncPhase === "ack_pending" || failure.startsWith("device_sync_ack_pending:")) {
    setBadge("기기 확인 전송 대기", "warning");
    setText("deviceSyncStatus", "이 PC의 자료 적용은 끝났지만 서버에 기기 확인을 전달하지 못했습니다. 확인 전송을 재시도하며, 아직 검증 완료로 표시하지 않습니다.");
  } else if (status?.syncPhase === "snapshot_missing" || failure.startsWith("onedrive_snapshot_pending")) {
    setBadge("OneDrive 보관본 도착 대기", "warning");
    setText("deviceSyncStatus", "서버에는 최신 세대가 게시되었지만 이 PC의 폴더에는 해당 보관본이 아직 보이지 않습니다. 업로드·수신 지연 또는 계정·폴더 차이를 확인해야 하며 현재 자료는 유지됩니다.");
  } else if (status?.ok === false && status.error) {
    const pending = isOneDriveDownloadPending(status.error);
    setBadge(pending ? "OneDrive 다운로드 대기" : "확인 필요", pending ? "warning" : "error");
    setText("deviceSyncStatus", pending
      ? ONE_DRIVE_DOWNLOAD_PENDING_MESSAGE
      : `기기 동기화 상태를 확인하지 못했습니다: ${deviceSyncErrorMessage(status.error, status.recoveryRequired)}`);
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
      : `마지막 동기화 문제: ${deviceSyncErrorMessage(status.lastError, status.recoveryRequired)}`);
  } else if (status.waitingForOneDrive) {
    setBadge("OneDrive 다운로드 대기", "warning");
    setText("deviceSyncStatus", ONE_DRIVE_DOWNLOAD_PENDING_MESSAGE);
  } else if (status.hasUnsyncedChanges) {
    setBadge("변경 내용 대기", "warning");
    setText("deviceSyncStatus", "이 PC의 최근 변경 내용을 잠시 모은 뒤 자동으로 새 세대에 반영합니다.");
  } else if (status.latestStatus === "announced") {
    setBadge("다른 기기 확인 대기", "warning");
    const evidence = status.oneDriveEvidence;
    const detail = evidence?.state === "in_sync"
      ? "OneDrive 공급자는 필수 파일을 동기화 상태로 표시하지만, 다른 PC의 수신·적용 완료를 뜻하지는 않습니다."
      : evidence?.state === "pending" ? "OneDrive 필수 파일의 전달 대기 또는 미도착이 관찰되었습니다."
      : evidence?.state === "error" ? "OneDrive 파일 상태를 확인하지 못했습니다."
      : "OneDrive 업로드 완료 여부는 아직 확인되지 않았습니다.";
    setText("deviceSyncStatus", `로컬 보관본 생성과 서버 세대 게시가 완료되었습니다. ${detail} 다른 기기가 검증·적용 후 확인하면 검증 완료로 바뀝니다.`);
  } else {
    setBadge("최신 상태", "ok");
    setText("deviceSyncStatus", status.lastSuccessAtMs
      ? `${formatDateTime(status.lastSuccessAtMs)}에 자료와 보관본의 최신 상태를 확인했습니다. 충돌 건수는 누적 보관 기록입니다.`
      : "이 PC는 확인된 최신 세대까지 반영했습니다. 서버의 다른 기기 확인과 OneDrive 파일 전달 상태는 별도로 판단합니다. 충돌 건수는 누적 보관 기록입니다.");
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
  setBadge("확인·검증 중", "warning");
  setText("deviceSyncStatus", "필요한 OneDrive 파일을 요청하고 최신 세대를 확인하고 있습니다. 다운로드 완료만으로 반영하지 않고 파일 검증을 마친 뒤 반영합니다.");
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
      if (message.startsWith("device_sync_ack_pending:") || message.startsWith("restore_recovery_required")) {
        try { snapshot = await invoke<DeviceSyncStatus>("get_device_sync_status"); }
        catch {
          // A previous successful status cannot establish current recovery safety.
          if (message.startsWith("restore_recovery_required") && snapshot) snapshot = { ...snapshot, recoveryRequired: undefined };
        }
      }
      renderDeviceSyncStatus({ ...(snapshot || { connected: false }), ok: false, error: message, lastError: message });
    } finally {
      statusRevision += 1;
      syncRun = null;
      updateActionState();
    }
  })();
  return syncRun;
}
