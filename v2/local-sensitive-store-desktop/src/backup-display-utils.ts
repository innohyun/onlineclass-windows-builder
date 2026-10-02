import type { BackupItem, BackupPreview, BackupSource, BackupStatus } from "./backup-types";
const numeric = (value?: number) => Number(value || 0) || 0;
const numberText = (value?: number) => String(numeric(value));

export function formatBackupDateTime(ms?: number) {
  const value = Number(ms || 0) || 0;
  if (!value) return "-";
  const date = new Date(value);
  const hour = date.getHours();
  const minute = String(date.getMinutes()).padStart(2, "0");
  return `${date.getFullYear()}년 ${date.getMonth() + 1}월 ${date.getDate()}일 ${hour < 12 ? "오전" : "오후"} ${hour % 12 || 12}:${minute}`;
}

export function countFrom(counts: Record<string, number> | undefined, keys: string[]) {
  if (!counts) return 0;
  for (const key of keys) {
    const value = Number(counts[key] || 0) || 0;
    if (value) return value;
  }
  return 0;
}

export function backupObservationCount(counts?: Record<string, number>) {
  return countFrom(counts, ["lesson_observations", "observationCount"]);
}

export function backupPrivateDetailCount(counts?: Record<string, number>) {
  return countFrom(counts, ["student_private_details", "studentPrivateDetailCount"]);
}

export function backupCounselingCount(counts?: Record<string, number>) {
  return countFrom(counts, ["teacher_counseling_sessions", "teacherCounselingSessionCount"]);
}

export function backupCareCount(counts?: Record<string, number>) {
  return backupObservationCount(counts) + backupCounselingCount(counts) + backupPrivateDetailCount(counts);
}

export function backupMathDailyCount(counts?: Record<string, number>) {
  return [
    ["math_daily_attempts", "mathDailyAttemptCount"],
    ["math_daily_student_profiles", "mathDailyProfileCount"],
    ["math_daily_review_sessions", "mathDailyReviewSessionCount"],
    ["math_daily_assignments", "mathDailyAssignmentCount"],
    ["math_daily_assignment_results", "mathDailyAssignmentResultCount"],
    ["math_daily_cache_runs", "mathDailyCacheRunCount"],
  ].reduce((sum, keys) => sum + countFrom(counts, keys), 0);
}

export function backupBoardSnapshotCount(counts?: Record<string, number>) {
  return countFrom(counts, ["board_post_snapshots", "boardSnapshotCount"]);
}

export function backupBoardMediaCount(counts?: Record<string, number>, media?: BackupItem["media"] | BackupPreview["media"]) {
  const count = countFrom(counts, ["board_media_files", "boardMediaCount"]);
  if (count) return count;
  const records = Array.isArray(media?.records) ? media.records.length : 0;
  return records;
}

export function backupArchiveCount(counts?: Record<string, number>) {
  return countFrom(counts, ["sharedArchiveCount"]);
}

export function backupAttendanceCount(counts?: Record<string, number>) {
  return [
    ["attendance_records", "attendanceRecordCount"],
    ["attendance_nais_checks", "attendanceNaisCheckCount"],
    ["attendance_document_requests", "attendanceDocumentRequestCount"],
  ].reduce((sum, keys) => sum + countFrom(counts, keys), 0);
}

export function backupEvalCount(counts?: Record<string, number>) {
  return [
    ["eval_assignments", "evalAssignmentCount"],
    ["eval_results", "evalResultCount"],
  ].reduce((sum, keys) => sum + countFrom(counts, keys), 0);
}

export function backupStudentRecordCount(counts?: Record<string, number>) {
  return [
    ["student_record_draft_sets", "studentRecordDraftSetCount"],
    ["student_record_drafts", "studentRecordDraftCount"],
  ].reduce((sum, keys) => sum + countFrom(counts, keys), 0);
}

export function backupLearningCount(counts?: Record<string, number>) {
  return backupMathDailyCount(counts) + backupEvalCount(counts);
}

export function backupSourcePcName(source?: BackupSource) {
  return String(source?.pcName || "").trim() || "PC 정보 없음";
}

export function backupSourceRelation(source?: BackupSource, currentPcName = "") {
  const sourcePc = String(source?.pcName || "").trim().toLowerCase();
  const currentPc = String(currentPcName || "").trim().toLowerCase();
  if (!sourcePc) return "PC 정보 없음";
  if (!currentPc) return "현재 PC와 비교 전";
  if (sourcePc === currentPc) return "이 PC";
  return "다른 PC";
}

export function backupEnvironmentText(source?: BackupSource) {
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

export function backupSourceSummary(source?: BackupSource, currentPcName = "") {
  return `${backupSourceRelation(source, currentPcName)} · ${backupSourcePcName(source)} · ${backupEnvironmentText(source)}`;
}

export function backupSourceListText(source?: BackupSource, currentPcName = "") {
  const os = String(source?.os || "").trim() || "운영체제 정보 없음";
  return `${backupSourceRelation(source, currentPcName)} · ${backupSourcePcName(source)} · ${os}`;
}

export function backupFolderLabel(status: BackupStatus) {
  if (!status.configured) return "-";
  const folder = String(status.tenantBackupDir || status.backupRootDir || "").toLowerCase();
  if (folder.includes("onedrive")) return "학교 OneDrive · OnlineClassLocalBackups";
  if (folder.includes("google drive")) return "Google Drive · OnlineClassLocalBackups";
  if (folder.includes("dropbox")) return "Dropbox · OnlineClassLocalBackups";
  if (folder.includes("icloud")) return "iCloud Drive · OnlineClassLocalBackups";
  return "선택한 백업 폴더 · OnlineClassLocalBackups";
}

export function backupRowSummary(backup: BackupItem) {
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

export function normalizeBackupList(items: unknown): BackupItem[] {
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

