import type { DeviceSyncStatus } from "./device-sync-ui";
import type { DeviceAuthorizationResult } from "./device-authorization";

export type SyncTone = "ok" | "warning" | "error" | "neutral";
export type SyncStage = { id: "Posted" | "Provider" | "Applied" | "Ack"; label: string; status: string; detail: string; tone: SyncTone };
export type DeviceSyncPresentation = { phase: string; tone: SyncTone; label: string; detail: string; canRun: boolean; stages: SyncStage[] };

function generation(value?: number) {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 ? value : null;
}

export function deriveDeviceSyncPresentation(status: DeviceSyncStatus | null): DeviceSyncPresentation {
  const latest = generation(status?.latestGeneration);
  const applied = generation(status?.appliedGeneration);
  const published = generation(status?.publishedGeneration);
  const failure = status?.error || status?.lastError || "";
  const recovery = status?.recoveryRequired === true || (failure.startsWith("restore_recovery_required") && status?.recoveryRequired !== false);
  const ackPending = status?.syncPhase === "ack_pending" || failure.startsWith("device_sync_ack_pending:");
  const connected = status?.connected === true;
  const ready = connected && status?.credentialAvailable === true && status?.oneDriveConfigured === true && !status.backupError && !recovery;
  const reached = latest !== null && applied !== null && applied >= latest;
  const verified = status?.latestStatus === "verified" && latest !== null;
  const evidence = status?.oneDriveEvidence;
  const stages: SyncStage[] = [
    { id: "Posted", label: "백업 정보 게시", status: published ? "게시 확인" : "확인 전", detail: published ? `이 PC 게시 세대 ${published}` : "게시된 세대를 확인하지 못했습니다.", tone: published ? "ok" : "neutral" },
    { id: "Provider", label: "OneDrive 전달 상태", status: evidence?.state === "in_sync" ? "파일 상태 확인" : evidence?.state === "error" ? "확인 필요" : evidence?.state === "pending" ? "전달 대기" : "확인 전", detail: evidence?.state === "in_sync" ? "공급자 표시 기준 · 다른 PC 반영 완료와 별도" : "필수 파일의 공급자 상태를 확인합니다.", tone: evidence?.state === "in_sync" ? "ok" : evidence?.state === "error" ? "error" : evidence?.state === "pending" ? "warning" : "neutral" },
    { id: "Applied", label: "이 PC 검증·반영", status: reached ? "반영 확인" : latest ? "반영 대기" : "확인 전", detail: applied ? `이 PC 반영 ${applied}세대${latest ? ` · 서버 ${latest}세대` : ""}` : "확인된 반영 세대가 없습니다.", tone: reached ? "ok" : latest ? "warning" : "neutral" },
    { id: "Ack", label: "기기 확인 전송·다른 PC 확인", status: ackPending ? "확인 전송 대기" : verified ? "다른 PC 확인됨" : latest ? "다른 PC 확인 대기" : "확인 전", detail: ackPending ? "이 PC 반영과 서버 확인 전송은 별도입니다." : verified ? `${latest}세대 전달·적용 확인 · 현재 본문 일치와 별도` : "게시 완료만으로 다른 PC 반영을 확정하지 않습니다.", tone: ackPending || (latest && !verified) ? "warning" : verified ? "ok" : "neutral" },
  ];
  let phase = "checking"; let tone: SyncTone = "neutral"; let label = "확인 전"; let detail = "기기 동기화 상태를 확인하고 있습니다.";
  if (recovery) { phase = "recovery"; tone = "error"; label = "복구 확인 필요"; detail = "복구 상태를 확인할 때까지 변경과 동기화를 중단합니다."; }
  else if (!status || (status.ok === false && !connected)) { phase = "unavailable"; tone = "warning"; label = "확인 불가"; detail = "기기 동기화 상태를 확인하지 못했습니다."; }
  else if (!connected) { phase = "disconnected"; tone = "warning"; label = "PC 연결 필요"; detail = "교사 로그인으로 이 PC를 연결하세요."; }
  else if (!status.credentialAvailable) { phase = "credential"; tone = "warning"; label = "재연결 필요"; detail = "기기 연결 정보를 확인해야 합니다."; }
  else if (status.backupError) { phase = "folder"; tone = "warning"; label = "백업 폴더 확인 필요"; detail = "백업 폴더에 접근할 수 없어 백업과 기기 간 동기화를 보류합니다. 로컬 자료는 계속 사용할 수 있습니다. 폴더 연결과 접근 권한을 확인해 주세요."; }
  else if (!status.oneDriveConfigured) { phase = "folder"; tone = "warning"; label = "백업 폴더 설정 필요"; detail = "기기 간 전달에 사용할 백업 폴더를 선택하세요."; }
  else if (ackPending) { phase = "ack_pending"; tone = "warning"; label = "기기 확인 전송 대기"; detail = reached ? "이 PC 반영은 끝났지만 서버에 기기 확인 전송이 남아 있습니다." : "서버에 기기 확인 전송이 남아 있습니다. 이 PC 반영 세대도 다시 확인해야 합니다."; }
  else if (status.artifactIssue?.kind === "integrity" || status.artifactIssue?.kind === "unavailable") { phase = "artifact_error"; tone = "error"; label = "파일 확인 필요"; detail = "필수 파일 검증을 완료하지 못해 반영과 게시를 보류합니다."; }
  else if (failure && !/^(onedrive_download_pending|onedrive_snapshot_pending)(?::|$)/u.test(failure)) { phase = "error"; tone = "error"; label = "동기화 확인 필요"; detail = "필수 자료 검증 또는 전달을 완료하지 못했습니다. 현재 자료와 기존 백업을 유지하며 진단을 확인하세요."; }
  else if (status.waitingForOneDrive || (latest !== null && applied !== null && latest > applied) || status.artifactIssue?.kind === "missing" || /^(onedrive_download_pending|onedrive_snapshot_pending)/u.test(failure)) { phase = "download_pending"; tone = "warning"; label = "필수 파일 도착 대기"; detail = "파일 도착과 검증을 기다립니다. 현재 로컬 자료는 유지됩니다."; }
  else if (status.ok === false || failure) { phase = "error"; tone = "error"; label = "동기화 확인 필요"; detail = "동기화 결과를 확인하고 다시 시도하세요."; }
  else if (status.hasUnsyncedChanges || (status.pendingLocalChangeCount ?? 0) > 0) { phase = "publish_pending"; tone = "warning"; label = "변경 게시 대기"; detail = "이 PC 변경 내용의 게시가 남아 있습니다."; }
  else if (latest && !verified) { phase = "remote_pending"; tone = "warning"; label = "다른 PC 확인 대기"; detail = "게시·파일 전달과 다른 PC 검증·반영 확인을 구분합니다."; }
  else if (status.observationEvidence?.state !== "clear" || status.observationEvidence.conflictedRecordCount !== 0) { phase = "observation_review"; tone = "warning"; label = "관찰 이력 확인 필요"; detail = "기기 전달 확인과 관찰 내용의 일치는 별도입니다."; }
  else if (reached) { phase = "applied"; tone = "ok"; label = "이 PC 반영 확인"; detail = `${applied}세대 반영이 확인되었습니다. 다른 PC의 현재 내용 일치와는 별도입니다.`; }
  else { phase = "initial"; label = "동기화 확인 전"; detail = "확인된 반영 세대가 없습니다."; }
  if (!ready || (status?.ok === false && !ackPending) || recovery) {
    for (const stage of stages) { stage.status = "확인 전"; stage.tone = "neutral"; }
    if (recovery) { stages[2].status = "복구 확인 필요"; stages[2].tone = "error"; }
    else if (status?.backupError) { stages[1].status = "폴더 확인 필요"; stages[1].tone = "warning"; }
  } else if (status?.artifactIssue || phase === "download_pending") {
    stages[1].status = status.artifactIssue?.kind === "integrity" ? "파일 검증 필요" : "필수 파일 대기";
    stages[1].tone = phase === "artifact_error" ? "error" : "warning";
    const count = status.artifactIssue?.missingFileCount;
    if (typeof count === "number" && Number.isSafeInteger(count) && count > 0) stages[1].detail = `필수 파일 ${count}개 미도착 · 이 PC 변경 게시도 대기 중`;
    if (!reached) { stages[2].status = "반영 보류"; stages[2].tone = "warning"; }
  }
  return { phase, tone, label, detail, canRun: ready, stages };
}

export function authorizationStepStates(result: DeviceAuthorizationResult) {
  const approved = result.status === "approved" || result.status === "connected" || result.status === "device_sync_failed";
  return [approved ? "complete" : "neutral", approved ? "complete" : result.status === "pending" ? "current" : "neutral", result.status === "connected" ? "complete" : result.status === "device_sync_failed" ? "error" : result.status === "approved" ? "current" : "neutral", "optional"] as const;
}
