export type DuplicateRecord = { docId: string; revisionId: string; savedAtMs: number; body: string; referenced: boolean };
export type DuplicateGroup = {
  groupId: string; snapshotHash: string; sectionKey: string; studentId: string; studentName: string;
  date: string; body: string; matchReason: string; keeperId: string; records: DuplicateRecord[];
  archiveIds: string[]; canApply: boolean; blockedReasons: string[];
};
export type DuplicateScan = { ok: boolean; tenantId: string; scannedCount: number; groups: DuplicateGroup[]; maxArchiveCount: number };
export type CleanupEntry = {
  cleanupId: string; createdAtMs: number; archivedCount: number; canUndo: boolean;
  state: "archived" | "restored" | "changed";
  records: { docId: string; studentName: string; date: string; body: string }[]; blockedReason: string;
};
export type CleanupRequest = { tenantId: string; cleanupId: string; groups: { groupId: string; snapshotHash: string }[] };

// A review authorizes exactly the displayed native snapshot, never inferred record IDs.
export function cleanupSelection(scan: DuplicateScan | null, selected: Set<string>) {
  const groups = (scan?.groups || []).filter(group => selected.has(group.groupId) && group.canApply && group.archiveIds.length > 0);
  const count = groups.reduce((sum, group) => sum + group.archiveIds.length, 0);
  return { groups, count, valid: count > 0 && count <= (scan?.maxArchiveCount || 0) };
}

export function cleanupReadbackMatches(entry: CleanupEntry | undefined, expectedIds: string[], state: "archived" | "restored") {
  if (!entry || entry.state !== state || entry.archivedCount !== expectedIds.length) return false;
  const actual = new Set(entry.records.map(record => record.docId));
  return actual.size === expectedIds.length && expectedIds.every(id => actual.has(id));
}

export function duplicateErrorMessage(value: unknown) {
  const code = value instanceof Error ? value.message : String(value || "");
  if (/stale|conflict|changed|revision|snapshot/i.test(code)) return "자료가 검토 이후 바뀌었습니다. 다시 검색한 뒤 정리 대상을 확인해 주세요.";
  if (/limit|too_many/i.test(code)) return "한 번에 정리할 수 있는 건수를 넘었습니다. 일부 묶음만 선택해 주세요.";
  if (/reference|referenced/i.test(code)) return "학생기록의 근거로 사용 중인 자료입니다. 최신 중복 검색 결과를 확인해 주세요.";
  if (/readback/i.test(code)) return "정리 결과의 재조회 확인이 끝나지 않았습니다. 정리 내역을 다시 확인해 주세요.";
  if (/authority|integrity|content_changed/i.test(code)) return "기록 원문과 증빙 이력을 확인하지 못해 정리를 중단했습니다. 로컬 자료 상태를 확인해 주세요.";
  if (/restore_recovery_required/i.test(code)) return "이 학급의 복원 복구가 필요해 정리를 중단했습니다. 백업·복원 상태를 먼저 확인해 주세요.";
  if (/tenant|unauthorized|connected/i.test(code)) return "연결된 학급을 확인한 뒤 다시 열어 주세요.";
  return "로컬 자료를 확인하지 못했습니다. 잠시 후 다시 시도해 주세요.";
}
