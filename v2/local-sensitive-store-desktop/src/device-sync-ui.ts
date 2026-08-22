import { invoke } from "@tauri-apps/api/core";

export type DeviceSyncStatus = {
  ok: boolean;
  connected: boolean;
  tenantId?: string;
  credentialAvailable?: boolean;
  oneDriveConfigured?: boolean;
  appliedGeneration?: number;
  publishedGeneration?: number;
  latestGeneration?: number;
  latestStatus?: string;
  hasUnsyncedChanges?: boolean;
  lastSuccessAtMs?: number;
  lastError?: string;
  conflictCount?: number;
  waitingForOneDrive?: boolean;
  error?: string;
};

let snapshot: DeviceSyncStatus | null = null;

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

function updateActionState(busy = false) {
  const unavailable = !snapshot?.connected || !snapshot.credentialAvailable || !snapshot.oneDriveConfigured;
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
  setText("deviceSyncConflictText", status?.connected ? `${Number(status.conflictCount || 0)}건` : "-");
  if (status?.ok === false && status.error) {
    setBadge("확인 필요", "error");
    setText("deviceSyncStatus", `기기 동기화 상태를 확인하지 못했습니다: ${status.error}`);
  } else if (!status?.connected) {
    setBadge("PC 연결 필요", "warning");
    setText("deviceSyncStatus", "교사 설정에서 이 PC를 연결하면 OneDrive 최신 내용을 자동으로 맞춥니다.");
  } else if (!status.credentialAvailable) {
    setBadge("재연결 필요", "warning");
    setText("deviceSyncStatus", "기기 동기화 자격 증명을 확인할 수 없습니다. 교사 설정에서 PC를 다시 연결해 주세요.");
  } else if (!status.oneDriveConfigured) {
    setBadge("OneDrive 설정 필요", "warning");
    setText("deviceSyncStatus", "학교 OneDrive 안의 백업 폴더를 선택하면 자동 동기화를 시작합니다.");
  } else if (status.lastError) {
    setBadge("확인 필요", "error");
    setText("deviceSyncStatus", `마지막 동기화 문제: ${status.lastError}`);
  } else if (status.waitingForOneDrive) {
    setBadge("파일 도착 대기", "warning");
    setText("deviceSyncStatus", "서버의 최신 세대를 확인했습니다. OneDrive 파일이 이 PC에 도착하면 자동으로 반영합니다.");
  } else if (status.hasUnsyncedChanges) {
    setBadge("변경 내용 대기", "warning");
    setText("deviceSyncStatus", "이 PC의 최근 변경 내용을 잠시 모은 뒤 자동으로 새 세대에 반영합니다.");
  } else if (status.latestStatus === "announced") {
    setBadge("다른 기기 확인 대기", "warning");
    setText("deviceSyncStatus", "최신 내용은 OneDrive에 저장되었습니다. 다른 기기가 확인하면 검증 완료로 바뀝니다.");
  } else {
    setBadge("최신 상태", "ok");
    setText("deviceSyncStatus", status.lastSuccessAtMs
      ? `${formatDateTime(status.lastSuccessAtMs)}에 최신 상태를 확인했습니다.`
      : "현재 PC와 OneDrive의 최신 내용이 일치합니다.");
  }
  updateActionState();
}

export async function loadDeviceSyncStatus() {
  renderDeviceSyncStatus(await invoke<DeviceSyncStatus>("get_device_sync_status"));
}

export async function runDeviceSyncNow(afterRun: () => Promise<unknown>) {
  updateActionState(true);
  setText("deviceSyncStatus", "OneDrive 최신 파일과 서버 세대를 대조하고 있습니다.");
  try {
    const status = await invoke<DeviceSyncStatus>("run_device_sync_now");
    if (!status?.ok) throw new Error(status?.error || "device_sync_failed");
    renderDeviceSyncStatus(status);
    await afterRun();
  } catch (error) {
    const message = String((error as Error)?.message || error || "device_sync_failed");
    renderDeviceSyncStatus({ ...(snapshot || { connected: false }), ok: false, lastError: message });
  } finally {
    updateActionState();
  }
}
