import { updateTeacherRecordAction } from "./desk-record-editor";
import { invoke } from "@tauri-apps/api/core";
import { openArchiveBoardViewer, searchArchiveBoards, type ArchiveBoardSummary } from "./archive-board-explorer";
import { openWorkspaceWorkNoteReader } from "./work-note-reader";
import { isStudentPrivacyEnabled } from "./desk-privacy";
import { isDeskRestoreBlocked } from "./desk-restore-lock";

type LocalDataSection = {
  key: string;
  count?: number;
};

type LocalOverview = {
  ok?: boolean;
  sections?: LocalDataSection[];
  error?: string;
};

export type LocalDataRecord = {
  sectionKey: string;
  sectionLabel: string;
  groupKey: string;
  payload: Record<string, unknown>;
  updatedAtMs: number;
  dateKey?: string;
  hasAttachment?: boolean;
};

export type SearchResult = {
  ok?: boolean;
  total?: number;
  offset?: number;
  limit?: number;
  hasMore?: boolean;
  records?: LocalDataRecord[];
  error?: string;
};

export type Attachment = {
  mediaId: string;
  attachmentKind: string;
  fileName: string;
  contentType: string;
  size: number;
};

type ExplorerOptions = {
  getTenantId: () => string;
};

export type ExplorerOpenOptions = {
  resetFilters?: boolean;
  query?: string;
  group?: string;
  sectionKey?: string;
  hasAttachment?: boolean;
};

const PAGE_SIZE = 40;
const DESIGN_PREVIEW = new URLSearchParams(window.location.search).get("designPreview") === "data";
const GROUP_KEYS = ["care", "attendance", "learning", "student-record", "work-notes"] as const;
const GROUP_SECTIONS = {
  care: ["observations", "teacher-counseling-sessions", "student-private-details"],
  attendance: ["attendance-records", "attendance-nais-checks", "attendance-document-requests"],
  learning: ["eval-assignments", "eval-results", "math-daily-attempts", "board-post-snapshots", "board-media"],
  "student-record": ["student-record-drafts", "student-record-draft-sets"],
  "work-notes": ["work-notes"],
} as const;
const SECTION_LABELS: Record<string, string> = {
  "archive-board": "보관 보드",
  observations: "관찰 기록",
  "teacher-counseling-sessions": "상담 기록",
  "student-private-details": "학생 민감정보",
  "attendance-records": "출결 기록",
  "attendance-nais-checks": "출결 NEIS 확인",
  "attendance-document-requests": "출결 증빙",
  "math-daily-attempts": "매일수학 시도",
  "eval-assignments": "평가 운영",
  "eval-results": "평가 결과",
  "board-post-snapshots": "게시판 자료",
  "board-media": "게시판 첨부파일",
  "student-record-draft-sets": "학생부 초안 세트",
  "student-record-drafts": "학생부 초안",
  "work-notes": "수업자료·업무 노트",
};

const PREVIEW_RECORDS: LocalDataRecord[] = [
  {
    sectionKey: "observations", sectionLabel: "수업 관찰", groupKey: "care", updatedAtMs: Date.parse("2026-08-04T11:24:00+09:00"), dateKey: "2026-08-04",
    payload: { studentName: "학생01", studentCode: "1", observation: "수업 참여 태도와 또래 관계 행동을 관찰했습니다.", memo: "모둠 활동에서 친구의 의견을 경청하고 자신의 생각을 차분히 설명했습니다." },
  },
  {
    sectionKey: "teacher-counseling-sessions", sectionLabel: "교사 상담기록", groupKey: "care", updatedAtMs: Date.parse("2026-08-03T16:12:00+09:00"), dateKey: "2026-08-03",
    payload: { studentName: "학생02", studentCode: "2", summary: "진로 고민 상담 및 학습 계획을 함께 정리했습니다.", topics: ["진로", "학습 계획"] },
  },
  {
    sectionKey: "attendance-document-requests", sectionLabel: "출결 증빙 요청", groupKey: "attendance", updatedAtMs: Date.parse("2026-08-04T09:36:00+09:00"), dateKey: "2026-08-04", hasAttachment: true,
    payload: { studentName: "학생03", studentCode: "3", status: "병결", reason: "8월 4일 병결로 처리했습니다. 보호자가 진료확인서를 제출했습니다.", attachments: [{ mediaId: "preview-proof", fileName: "진료확인서_학생03.pdf", contentType: "application/pdf", size: 253952 }] },
  },
  {
    sectionKey: "eval-results", sectionLabel: "평가 기록", groupKey: "learning", updatedAtMs: Date.parse("2026-08-02T15:05:00+09:00"), dateKey: "2026-08-02",
    payload: { studentName: "학생04", studentId: "4", title: "수학 단원평가", summary: "분수의 덧셈 단원평가 결과 및 피드백", result: "개념 이해가 안정적이며 풀이 과정을 정확히 설명했습니다." },
  },
  {
    sectionKey: "student-record-drafts", sectionLabel: "학생부 초안", groupKey: "student-record", updatedAtMs: Date.parse("2026-08-01T14:20:00+09:00"), dateKey: "2026-08-01",
    payload: { studentName: "학생05", studentCode: "5", title: "행동특성 및 종합의견 초안", content: "책임감 있게 학급 활동에 참여하고 친구를 배려하는 태도가 돋보입니다." },
  },
];

export function element<T extends HTMLElement>(id: string) {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element: ${id}`);
  return found as T;
}

export function escapeHtml(value: unknown) {
  return String(value ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function numeric(value: unknown) {
  return Math.max(0, Number(value || 0) || 0);
}

function objectValue(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

export function firstText(row: Record<string, unknown>, keys: string[]) {
  for (const key of keys) {
    const value = row[key];
    if (typeof value === "string" && value.trim()) return value.trim();
    if (typeof value === "number" && Number.isFinite(value)) return String(value);
  }
  return "";
}

export function studentName(record: LocalDataRecord) {
  if (record.sectionKey === "archive-board") return "공유 보관본";
  const name = firstText(record.payload, ["studentName", "displayName", "name"]);
  if (name) return name;
  return firstText(record.payload, ["studentCode", "studentId"]) ? "이름 미확인" : "학급 자료";
}

export function sectionLabel(record: LocalDataRecord) {
  return SECTION_LABELS[record.sectionKey] || record.sectionLabel || record.sectionKey;
}

export function formatDate(ms: number, dateKey = "") {
  const date = ms ? new Date(ms) : dateKey ? new Date(`${dateKey}T00:00:00`) : null;
  if (!date || Number.isNaN(date.getTime())) return dateKey || "-";
  return date.toLocaleString("ko-KR", { year: "numeric", month: "long", day: "numeric", hour: ms ? "numeric" : undefined, minute: ms ? "2-digit" : undefined });
}

export function shortDate(record: LocalDataRecord) {
  const date = record.dateKey ? new Date(`${record.dateKey}T00:00:00`) : new Date(record.updatedAtMs);
  if (Number.isNaN(date.getTime())) return "-";
  return date.toLocaleDateString("ko-KR", { month: "numeric", day: "numeric" });
}

function valueAtPath(payload: Record<string, unknown>, path: string) {
  return path.split(".").reduce<unknown>((value, key) => objectValue(value)[key], payload);
}

function readableValues(value: unknown) {
  if (typeof value === "string" && value.trim()) return [value.trim()];
  if (!Array.isArray(value)) return [];
  return value.flatMap((item) => {
    if (typeof item === "string" && item.trim()) return [item.trim()];
    const row = objectValue(item);
    return ["text", "content", "comment", "caption", "summary", "note"]
      .map((key) => row[key])
      .filter((text): text is string => typeof text === "string" && Boolean(text.trim()))
      .map((text) => text.trim());
  });
}

const CONTENT_PATHS: Record<string, string[]> = {
  observations: ["observation", "memo", "note", "content"],
  "teacher-counseling-sessions": ["summary", "followUpNote", "note", "sourceTranscript.text", "content"],
  "student-private-details": ["siblingsNote", "specialNote", "health.emergencyNote"],
  "attendance-records": ["reason", "note", "memo", "description"],
  "attendance-nais-checks": ["reason", "note", "memo", "description"],
  "attendance-document-requests": ["reason", "note", "memo", "description"],
  "eval-assignments": ["description", "subject", "achievement", "coreStandard", "achievementStandard", "levelTexts", "note"],
  "eval-results": ["customResultText", "customText", "levelLabel", "note", "feedback", "summary", "text", "result"],
  "math-daily-attempts": ["feedback", "summary", "answer", "note"],
  "board-post-snapshots": ["content", "body", "summary", "description"],
  "student-record-draft-sets": ["summary", "description", "note"],
  "student-record-drafts": ["behaviorComment", "subjectComments", "creativeComments", "content", "draftText", "text", "summary", "note"],
  "work-notes": ["markdown", "properties.summary", "properties.description", "blocks"],
};

export function contentParts(payload: Record<string, unknown>, sectionKey = "") {
  const preferred = CONTENT_PATHS[sectionKey] || ["summary", "content", "observation", "memo", "note", "reason", "description", "detail", "comment", "feedback"];
  const seen = new Set<string>();
  const parts: string[] = [];
  const add = (text: string) => {
    const normalized = text.trim();
    if (!normalized || seen.has(normalized)) return;
    seen.add(normalized);
    parts.push(normalized);
  };
  if (sectionKey === "student-private-details") {
    const guardianText = (key: "guardian1" | "guardian2", label: string) => {
      const guardian = objectValue(payload[key]);
      const value = [firstText(guardian, ["name"]), firstText(guardian, ["phone"])].filter(Boolean).join(" · ");
      if (value) add(`${label}: ${value}`);
    };
    guardianText("guardian1", "보호자 1");
    guardianText("guardian2", "보호자 2");
    const health = objectValue(payload.health);
    for (const [key, label] of [["conditions", "건강 유의사항"], ["allergies", "알레르기"], ["cautionFoods", "주의 음식"]] as const) {
      const values = readableValues(health[key]);
      if (values.length) add(`${label}: ${values.join(", ")}`);
    }
    const emergency = firstText(health, ["emergencyNote"]);
    if (emergency) add(`응급 참고: ${emergency}`);
  }
  for (const path of preferred) {
    for (const text of readableValues(valueAtPath(payload, path))) {
      add(text);
      if (parts.length >= 4) return parts;
    }
  }
  return parts.slice(0, 4);
}

export function recordSummary(record: LocalDataRecord) {
  const parts = contentParts(record.payload, record.sectionKey);
  const value = parts[0] || recordStatusLabel(record) || "저장된 원문을 확인하세요.";
  return value.length > 62 ? `${value.slice(0, 62)}…` : value;
}

function recordTitle(record: LocalDataRecord) {
  if (record.sectionKey === "archive-board") return firstText(record.payload, ["title"]) || "제목 없는 보관 보드";
  if (record.sectionKey === "work-notes") {
    return `${firstText(record.payload, ["emoji"])} ${firstText(record.payload, ["title"]) || "제목 없음"}`.trim();
  }
  return `${studentName(record)} · ${sectionLabel(record)}`;
}

export function recordStatusLabel(record: LocalDataRecord) {
  if (record.sectionKey === "archive-board") return "읽기 전용";
  const mode = firstText(record.payload, ["resultMode"]);
  if (mode === "custom_text") return "서술형 평가";
  if (mode === "level") return "단계형 평가";
  const status = firstText(record.payload, ["status", "kind"]);
  const labels: Record<string, string> = {
    draft: "초안",
    recorded: "기록 완료",
    completed: "완료",
    reviewed: "검토 완료",
    unread: "미확인",
    read: "확인",
    closed: "종료",
    in_progress: "진행 중",
    pending: "대기",
    approved: "승인",
    rejected: "반려",
    absent: "결석",
    late: "지각",
  };
  return labels[status] || (record.sectionKey === "work-notes" ? "이 PC 업무 노트" : status);
}

function attachmentFromValue(value: unknown, defaultKind: string): Attachment | null {
  const row = objectValue(value);
  const mediaId = firstText(row, ["mediaId", "id"]);
  const fileName = firstText(row, ["fileName", "originalName", "name"]);
  if (!mediaId && !fileName) return null;
  return {
    mediaId,
    attachmentKind: firstText(row, ["attachmentKind", "kind"]) || defaultKind,
    fileName: fileName || "첨부파일",
    contentType: firstText(row, ["contentType", "mimeType", "type"]) || "application/octet-stream",
    size: numeric(row.size || row.bytes),
  };
}

export function recordAttachments(record: LocalDataRecord) {
  const values: unknown[] = [];
  for (const key of ["attachments", "files", "media", "documents"]) {
    const value = record.payload[key];
    if (Array.isArray(value)) values.push(...value);
    else if (value && typeof value === "object") values.push(value);
  }
  if (record.sectionKey === "board-media") values.unshift(record.payload);
  const seen = new Set<string>();
  const defaultKind = record.sectionKey === "work-notes" ? "work-note" : "board-media";
  return values.map((value) => attachmentFromValue(value, defaultKind)).filter((item): item is Attachment => {
    if (!item) return false;
    const key = item.mediaId || item.fileName;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

export function byteText(bytes: number) {
  if (!bytes) return "크기 정보 없음";
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
  return `${Math.round(bytes / 1024)}KB`;
}

export function fileTypeLabel(contentType: string, fileName: string) {
  const extension = fileName.split(".").pop()?.toUpperCase();
  if (extension && extension.length <= 5) return extension;
  if (contentType === "application/pdf") return "PDF";
  return "파일";
}

export function dateRange(period: string) {
  if (period === "all") return { dateFrom: "", dateTo: "" };
  const days = Math.max(1, Number(period || 30) || 30);
  const end = new Date();
  const start = new Date(end.getFullYear(), end.getMonth(), end.getDate() - days + 1);
  const localIso = (date: Date) => `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
  return { dateFrom: localIso(start), dateTo: localIso(end) };
}

function groupForSection(sectionKey: string) {
  if (sectionKey === "archive-board") return "archive-boards";
  return GROUP_KEYS.find((group) => GROUP_SECTIONS[group].includes(sectionKey as never)) || "";
}

function archiveBoardRecord(board: ArchiveBoardSummary): LocalDataRecord {
  return {
    sectionKey: "archive-board",
    sectionLabel: "보관 보드",
    groupKey: "archive-boards",
    updatedAtMs: board.importedAt,
    hasAttachment: board.fileCount > 0,
    payload: {
      archiveId: board.archiveId,
      title: board.title,
      postCount: board.postCount,
      fileCount: board.fileCount,
      totalFileBytes: board.totalFileBytes,
      studentViewMode: board.studentViewMode,
      summary: `게시글 ${board.postCount}개 · 첨부파일 ${board.fileCount}개 · ${board.studentViewMode === "shelf" ? "선반" : board.studentViewMode === "detail" ? "상세 목록" : "갤러리"} 보기`,
    },
  };
}

export function recordCanonicalKey(record: LocalDataRecord | undefined): string {
  if (!record)
    return "";
  const keys: Record<string, string> = { observations: 'docId', 'teacher-counseling-sessions': 'sessionId', 'student-private-details': 'studentCode', 'attendance-records': 'recordId', 'attendance-nais-checks': 'checkId', 'attendance-document-requests': 'requestId', 'eval-assignments': 'assignmentId', 'eval-results': 'resultId', 'math-daily-attempts': 'attemptId', 'board-post-snapshots': 'postId', 'board-media': 'mediaId', 'student-record-draft-sets': 'draftSetId', 'student-record-drafts': 'draftId', 'work-notes': 'pageId', 'archive-board': 'archiveId' };
  if (record.sectionKey === 'board-post-snapshots') {
    const boardId = firstText(record.payload, ['boardId']);
    const postId = firstText(record.payload, ['postId']);
    return boardId && postId ? `${record.sectionKey}:${JSON.stringify([boardId, postId])}` : '';
  }
  const id = keys[record.sectionKey] && firstText(record.payload, [keys[record.sectionKey]]);
  return id ? `${record.sectionKey}:${id}` : '';
}
export function isSensitiveStudentRecord(record: LocalDataRecord): boolean {
  return !['work-notes', 'archive-board'].includes(record.sectionKey) && (record.sectionKey !== 'eval-assignments' || Boolean(firstText(record.payload, ['studentName', 'studentCode', 'studentId'])));
}
export function attachmentFailureKind(error: unknown): 'missing' | 'open-failed' {
  const code = String((error as Error)?.message || error);
  return ['media_file_missing', 'media_not_found', 'work_note_attachment_file_missing', 'work_note_attachment_missing', 'work_note_attachment_not_found'].includes(code) ? 'missing' : 'open-failed';
}
export type ExplorerFilters = {
  query: string;
  group: string;
  sectionKey: string;
  student: string;
  period: string;
  dateFrom: string;
  dateTo: string;
  hasAttachment: boolean;
};
export function applyExplorerOpenOptions(filters: ExplorerFilters, input: ExplorerOpenOptions): ExplorerFilters {
  const next = input.resetFilters ? {query:'', group:'', sectionKey:'', student:'', period:'', dateFrom:'', dateTo:'', hasAttachment:false} : { ...filters };
  if (input.query !== undefined) {
    next.query = input.query;
    next.group = '';
    next.sectionKey = '';
  }
  if (input.sectionKey !== undefined) {
    next.sectionKey = input.sectionKey;
    if (input.group === undefined)
      next.group = groupForSection(input.sectionKey);
  }
  if (input.group !== undefined) {
    next.group = input.group;
    if (input.sectionKey === undefined)
      next.sectionKey = '';
  }
  if (input.hasAttachment !== undefined)
    next.hasAttachment = input.hasAttachment;
  return next;
}
export function initDataExplorer(options: ExplorerOptions) {
  let records: LocalDataRecord[] = [];
  let selectedIndex = -1;
  let selectedKey = '';
  let selectedMedia = '';
  let page = 0;
  let total = 0;
  let sectionKey = '';
  let loading = false;
  let cancelled = false;
  let failure = '';
  let tenant = '';
  let generation = 0;
  let countGeneration = 0;
  let actionGeneration = 0;
  let listScroll = 0;
  let detailScroll = 0;
  let detailClosed = false;
  let archiveVisibleTotal = 0;
  const attachmentStates = new Map<string, {
    kind: 'missing' | 'open-failed';
    message: string;
  }>();
  let studentFilter = '';
  const queryInput = element<HTMLInputElement>('dataExplorerQuery');
  const groupSelect = element<HTMLSelectElement>('dataExplorerGroup');
  const studentInput = element<HTMLInputElement>('dataExplorerStudent');
  const periodSelect = element<HTMLSelectElement>('dataExplorerPeriod');
  const attachmentInput = element<HTMLInputElement>('dataExplorerHasAttachment');
  const view = element('dataOverviewSection').closest<HTMLElement>('.data-explorer-view')!;
  const list = element('localDataRecordList');
  const detailColumn = element('localDataDetail');
  const form = element<HTMLFormElement>('dataExplorerSearchForm');
  const submit = form.querySelector<HTMLButtonElement>('button[type=submit]')!;
  submit.classList.remove('sr-only');
  submit.classList.add('data-search-submit');
  submit.textContent = '검색';
  periodSelect.add(new Option('기간 직접 선택', 'custom'));
  periodSelect.closest('label')!.insertAdjacentHTML('beforeend', '<span class="data-custom-period" id="dataExplorerCustomPeriod" hidden><input type="date" id="dataExplorerDateFrom" aria-label="검색 시작일"><span>–</span><input type="date" id="dataExplorerDateTo" aria-label="검색 종료일"></span>');
  const dateFrom = element<HTMLInputElement>('dataExplorerDateFrom');
  const dateTo = element<HTMLInputElement>('dataExplorerDateTo');
  detailColumn.insertAdjacentHTML('afterbegin', '<button type="button" id="dataExplorerDetailToggle" class="data-detail-toggle" aria-controls="dataExplorerDetail" aria-expanded="true">패널 접기</button>');
  element('dataExplorerGroups').insertAdjacentHTML('beforebegin', '<button type="button" id="dataExplorerGroupDisclosure" class="data-group-disclosure" aria-expanded="false" aria-controls="dataExplorerGroups">이 PC의 자료 종류별 전체 건수</button>');
  element('dataExplorerPagination').insertAdjacentHTML('afterbegin', '<span id="dataExplorerRange"></span>');
  element('dataExplorerSearchForm').insertAdjacentHTML('afterend', '<p class="data-filter-context" id="dataExplorerFilterContext" role="status"></p>');
  const initialFilters = (): ExplorerFilters => ({ query: '', group: '', sectionKey: '', student: '', period: '30', dateFrom: '', dateTo: '', hasAttachment: false });
  type Memory = {
    filters: ExplorerFilters;
    page: number;
    selectedKey: string;
    selectedMedia: string;
    listScroll: number;
    detailScroll: number;
    detailClosed: boolean;
    records: LocalDataRecord[];
    total: number;
  };
  const memories = new Map<string, Memory>();
  const filters = (): ExplorerFilters => ({ query: queryInput.value, group: groupSelect.value, sectionKey, student: isStudentPrivacyEnabled() ? studentFilter : studentInput.value, period: periodSelect.value, dateFrom: dateFrom.value, dateTo: dateTo.value, hasAttachment: attachmentInput.checked });
  const setFilters = (value: ExplorerFilters) => { queryInput.value = value.query; groupSelect.value = value.group; sectionKey = value.sectionKey; studentFilter = value.student; studentInput.value = isStudentPrivacyEnabled() ? '' : value.student; periodSelect.value = value.period; dateFrom.value = value.dateFrom; dateTo.value = value.dateTo; attachmentInput.checked = value.hasAttachment; };
  const remember = (captureScroll = true) => { if (captureScroll && !loading && !failure && !cancelled) {
    listScroll = list.scrollTop;
    detailScroll = detailColumn.scrollTop;
  } if (tenant)
    memories.set(tenant, { filters: filters(), page, selectedKey, selectedMedia, listScroll, detailScroll, detailClosed, records: [...records], total }); };
  const selected = () => records[selectedIndex];
  const privateRecord = (record?: LocalDataRecord) => Boolean(record && isStudentPrivacyEnabled() && isSensitiveStudentRecord(record));
  const setStatus = (message: string) => { element('dataOverviewStatus').textContent = message; };
  function ensureTenant() {
    const next = options.getTenantId().trim();
    if (next === tenant)
      return;
    remember();
    tenant = next;
    ++generation;
    ++countGeneration;
    ++actionGeneration;
    attachmentStates.clear();
    failure = '';
    cancelled = false;
    loading = false;
    const memory = memories.get(tenant);
    setFilters(memory?.filters || initialFilters());
    page = memory?.page || 0;
    selectedKey = memory?.selectedKey || '';
    selectedMedia = memory?.selectedMedia || '';
    listScroll = memory?.listScroll || 0;
    detailScroll = memory?.detailScroll || 0;
    detailClosed = memory?.detailClosed || false;
    records = memory?.records || [];
    total = memory?.total || 0;
    selectedIndex = records.findIndex(item => recordCanonicalKey(item) === selectedKey);
    renderRecords();
    renderDetail();
  }
  function syncFilters() {
    const archive = groupSelect.value === 'archive-boards';
    studentInput.disabled = archive || isStudentPrivacyEnabled();
    studentInput.placeholder = isStudentPrivacyEnabled() ? '학생 조건 가림' : '전체 학생';
    periodSelect.disabled = archive;
    attachmentInput.disabled = archive;
    dateFrom.disabled = archive;
    dateTo.disabled = archive;
    element('dataExplorerCustomPeriod').hidden = periodSelect.value !== 'custom' || archive;
    element('dataExplorerFilterContext').textContent = archive ? '보관 보드는 검색어만 적용합니다. 전체 건수는 보관 DB 기준이며 최근 100건까지 확인할 수 있습니다.' : sectionKey ? `현재 범위: ${SECTION_LABELS[sectionKey] || sectionKey}` : '';
    view.classList.toggle('is-attachment-mode', attachmentInput.checked && !archive);
    element('dataExplorerTitle').textContent = attachmentInput.checked && !archive ? '첨부파일' : '전체 검색';
    const heading = view.querySelector<HTMLElement>('.data-explorer-heading p');
    if (heading)
      heading.textContent = attachmentInput.checked && !archive ? '첨부가 있는 자료와 연결 문서를 함께 찾습니다.' : '이 PC에 저장된 자료를 찾습니다.';
  }
  function renderGroups(counts?: Record<string, number>) {
    if (counts) {
      for (const [key, id] of [['care', 'Care'], ['attendance', 'Attendance'], ['learning', 'Learning'], ['student-record', 'StudentRecord'], ['work-notes', 'WorkNotes'], ['archive-boards', 'ArchiveBoards']])
        element(`dataGroup${id}Count`).textContent = String(numeric(counts[key]));
    }
    document.querySelectorAll<HTMLButtonElement>('[data-data-group]').forEach(button => { const active = Boolean(groupSelect.value) && button.dataset.dataGroup === groupSelect.value; button.classList.toggle('is-selected', active); button.setAttribute('aria-pressed', String(active)); });
    syncFilters();
  }
  function renderRecords() {
    element('dataExplorerTotal').textContent = loading ? '불러오는 중' : failure || cancelled ? '확인 필요' : `${total.toLocaleString('ko-KR')}건`;
    const pageCount = Math.max(1, Math.ceil((groupSelect.value === 'archive-boards' ? Math.min(total, 100) : total) / PAGE_SIZE));
    element('dataExplorerPage').textContent = `${Math.min(page + 1, pageCount)} / ${pageCount}`;
    element('dataExplorerRange').textContent = loading || failure || cancelled ? '—' : records.length ? `${page * PAGE_SIZE + 1}–${page * PAGE_SIZE + records.length} / ${total}개` : '0개 표시';
    element<HTMLButtonElement>('dataExplorerPrevious').disabled = loading || Boolean(failure) || cancelled || page <= 0;
    element<HTMLButtonElement>('dataExplorerNext').disabled = loading || Boolean(failure) || cancelled || (page + 1) * PAGE_SIZE >= total || (groupSelect.value === 'archive-boards' && (page + 1) * PAGE_SIZE >= archiveVisibleTotal);
    list.setAttribute('aria-busy', String(loading));
    if (loading) {
      list.innerHTML = '<div class="data-loading"><p role="status">검색 결과를 불러오는 중입니다.</p>' + Array.from({ length: 5 }, () => '<div class="data-skeleton" aria-hidden="true"><span></span><span></span></div>').join('') + '<button type="button" data-search-cancel>취소</button></div>';
      return;
    }
    if (cancelled) {
      list.innerHTML = '<div class="data-list-message"><strong>검색을 취소했습니다.</strong><p>검색어와 필터는 유지되어 있습니다.</p><button type="button" data-search-retry>다시 검색</button></div>';
      return;
    }
    if (failure) {
      list.innerHTML = '<div class="data-list-message is-error"><strong>검색 결과를 불러오지 못했습니다.</strong><p>검색어와 필터는 유지되어 있습니다. 다시 시도해 주세요.</p><div><button type="button" data-search-retry>다시 시도</button><button type="button" data-search-reset>필터 초기화</button></div><small>이 PC에 저장된 자료는 변경되지 않았습니다.</small></div>';
      return;
    }
    if (!records.length) {
      list.innerHTML = '<div class="data-list-message"><i class="fa-solid fa-magnifying-glass" aria-hidden="true"></i><strong>조건에 맞는 자료가 없습니다.</strong><p>검색어를 수정하거나 필터를 초기화해 주세요.</p><button type="button" data-search-reset>필터 초기화</button></div>';
      return;
    }
    list.innerHTML = records.map((record, index) => `<button class="data-record-row${index === selectedIndex ? ' is-selected' : ''}" type="button" role="option" aria-selected="${index === selectedIndex}" data-data-record-index="${index}"><span class="data-record-title"><strong>${escapeHtml(privateRecord(record) ? `${sectionLabel(record)} · 학생 정보 가림` : recordTitle(record))}</strong>${record.hasAttachment ? '<i class="fa-solid fa-paperclip" aria-label="첨부파일 있음"></i>' : ''}</span><span class="data-record-date">${escapeHtml(shortDate(record))} · ${escapeHtml(privateRecord(record) ? sectionLabel(record) : recordStatusLabel(record) || sectionLabel(record))}</span><span class="data-record-summary">${escapeHtml(privateRecord(record) ? '학생 이름과 내용이 가려져 있습니다.' : recordSummary(record))}</span><i class="fa-solid fa-chevron-right data-record-arrow" aria-hidden="true"></i></button>`).join('');
    list.scrollTop = listScroll;
  }
  function renderAttachments(record: LocalDataRecord) {
    const masked = privateRecord(record);
    const files = masked ? [] : recordAttachments(record);
    element('dataAttachmentCount').textContent = `${files.length}개`;
    element('dataAttachments').hidden = !files.length;
    element('dataAttachmentList').innerHTML = files.map(file => { const state = attachmentStates.get(`${tenant}:${file.attachmentKind}:${file.mediaId}`); return `<div class="data-attachment-row${selectedMedia === file.mediaId ? ' is-selected' : ''}"><span class="data-file-icon" aria-hidden="true">${escapeHtml(fileTypeLabel(file.contentType, file.fileName))}</span><span><strong>${escapeHtml(file.fileName)}</strong><small>${escapeHtml(byteText(file.size))} · ${state?.kind === 'missing' ? '파일 없음' : '첨부 등록됨'}</small></span><button type="button" data-open-media="${escapeHtml(file.mediaId)}" data-attachment-kind="${escapeHtml(file.attachmentKind)}"${file.mediaId ? '' : ' disabled'}>${file.mediaId ? '파일 열기' : '파일 정보만 있음'}</button>${state ? `<div class="data-attachment-error" role="alert"><strong>${state.kind === 'missing' ? '첨부파일을 찾을 수 없습니다.' : '파일을 열지 못했습니다.'}</strong><p>${state.kind === 'missing' ? `${state.message.includes('file_missing') ? '첨부 정보는 남아 있지만 이 PC의 파일이 없습니다.' : '등록된 첨부 정보로 파일을 찾을 수 없습니다.'} 백업·동기화 상태를 확인해 보세요.` : '첨부 등록 정보와 연결 문서는 그대로 유지됩니다. 연결 프로그램을 확인한 뒤 다시 열어 보세요.'}</p><button type="button" data-open-media="${escapeHtml(file.mediaId)}" data-attachment-kind="${escapeHtml(file.attachmentKind)}">${state.kind === 'missing' ? '파일 다시 확인' : '다시 열기'}</button>${record.sectionKey === 'work-notes' ? '<button type="button" data-source-document>연결 문서 열기 ›</button>' : ''}<button type="button" data-app-view-target="backup">백업 상태 보기 ›</button></div>` : ''}</div>`; }).join('');
  }
  function renderDetail() {
    const record = !loading && !failure && !cancelled ? selected() : undefined;
    const masked = privateRecord(record);
    element('dataExplorerEmpty').hidden = Boolean(record);
    element('dataExplorerDetail').hidden = !record;
    detailColumn.classList.toggle('is-collapsed', detailClosed);
    element<HTMLButtonElement>('dataExplorerDetailToggle').textContent = detailClosed ? '패널 펼치기' : '패널 접기';
    element('dataExplorerDetailToggle').setAttribute('aria-expanded', String(!detailClosed));
    updateTeacherRecordAction(element('dataExplorerDetail'), masked ? undefined : record, tenant);
    element('dataOpenArchiveBoard').hidden = record?.sectionKey !== 'archive-board';
    element('dataOpenWorkNote').hidden = record?.sectionKey !== 'work-notes';
    element('dataOpenWorkNote').textContent = '연결 문서 열기 ›';
    element<HTMLButtonElement>('dataOpenWorkNote').disabled = false;
    element('dataDetailTitle').textContent = record ? (masked ? '학생 정보 가림' : recordTitle(record)) : '';
    element('dataDetailStudent').textContent = record ? (masked ? '가려짐' : record.sectionKey === 'work-notes' ? '학급 자료' : studentName(record)) : '';
    element('dataDetailKind').textContent = record ? sectionLabel(record) : '';
    element('dataDetailSavedAt').textContent = record ? formatDate(record.updatedAtMs, record.dateKey) : '';
    element('dataDetailBody').innerHTML = record ? (masked ? '<p>학생 이름과 내용을 가렸습니다. 학생 기록 가림을 해제하면 확인할 수 있습니다.</p>' : contentParts(record.payload, record.sectionKey).map(part => `<p>${escapeHtml(part)}</p>`).join('') || '<p>저장된 원문 필드가 없습니다.</p>') : '';
    element('dataJsonDetail').textContent = record && !masked ? JSON.stringify(record.payload, null, 2) : '';
    element('dataJsonDetail').closest('details')!.hidden = masked || !record;
    element('dataAttachmentList').replaceChildren();
    element('dataAttachments').hidden = true;
    element('dataAttachmentCount').textContent = '0개';
    if (record)
      renderAttachments(record);
    detailColumn.scrollTop = detailScroll;
  }
  async function loadCounts() {
    const current = tenant;
    const requestGeneration = ++countGeneration;
    if (DESIGN_PREVIEW) {
      renderGroups({ care: 327, attendance: 93, learning: 874, 'student-record': 174, 'work-notes': 12, 'archive-boards': 4 });
      return;
    }
    if (!current) {
      renderGroups({});
      return;
    }
    const [overview, archiveBoards] = await Promise.all([invoke<LocalOverview>('get_local_overview', { tenantId: current }), searchArchiveBoards(current, '', 1)]);
    if (requestGeneration !== countGeneration || current !== tenant || current !== options.getTenantId().trim())
      return;
    if (overview?.ok === false)
      throw new Error(overview.error || 'local_overview_failed');
    const sections = overview.sections || [];
    const countFor = (group: typeof GROUP_KEYS[number]) => sections.reduce((sum, section) => sum + (GROUP_SECTIONS[group].includes(section.key as never) ? numeric(section.count) : 0), 0);
    renderGroups({ care: countFor('care'), attendance: countFor('attendance'), learning: countFor('learning'), 'student-record': countFor('student-record'), 'work-notes': countFor('work-notes'), 'archive-boards': archiveBoards.total });
  }
  function previewSearch(value: ExplorerFilters): SearchResult {
    const range = value.period === 'custom' ? { dateFrom: value.dateFrom, dateTo: value.dateTo } : dateRange(value.period);
    const filtered = PREVIEW_RECORDS.filter(record => !value.group || record.groupKey === value.group).filter(record => !value.sectionKey || record.sectionKey === value.sectionKey).filter(record => !value.query || JSON.stringify(record.payload).toLocaleLowerCase('ko-KR').includes(value.query.toLocaleLowerCase('ko-KR'))).filter(record => !value.student || studentName(record).includes(value.student)).filter(record => !range.dateFrom || (record.dateKey || '') >= range.dateFrom).filter(record => !range.dateTo || (record.dateKey || '') <= range.dateTo).filter(record => !value.hasAttachment || record.hasAttachment);
    return { ok: true, total: filtered.length, records: filtered.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE) };
  }
  async function search() {
    ensureTenant();
    remember(false);
    const tenantId = tenant;
    ++actionGeneration;
    const requestGeneration = ++generation;
    const value = filters();
    const requestedPage = page;
    failure = '';
    cancelled = false;
    loading = true;
    renderGroups();
    setStatus('검색 결과를 불러오는 중입니다.');
    renderRecords();
    renderDetail();
    if (!tenantId && !DESIGN_PREVIEW) {
      loading = false;
      records = [];
      total = 0;
      selectedIndex = -1;
      setStatus('학급을 먼저 연결해 주세요.');
      renderRecords();
      renderDetail();
      return;
    }
    const range = value.period === 'custom' ? { dateFrom: value.dateFrom, dateTo: value.dateTo } : dateRange(value.period);
    try {
      if (value.group !== 'archive-boards' && range.dateFrom && range.dateTo && range.dateFrom > range.dateTo)
        throw new Error('시작일이 종료일보다 늦습니다. 기간을 확인해 주세요.');
      let result: SearchResult;
      let returnedArchiveCount = 0;
      if (!DESIGN_PREVIEW && value.group === 'archive-boards') {
        const archiveResult = await searchArchiveBoards(tenantId, value.query.trim(), 100);
        returnedArchiveCount = archiveResult.boards.length;
        result = { ok: true, total: archiveResult.total, records: archiveResult.boards.slice(requestedPage * PAGE_SIZE, (requestedPage + 1) * PAGE_SIZE).map(archiveBoardRecord) };
      }
      else
        result = DESIGN_PREVIEW ? previewSearch(value) : await invoke<SearchResult>('search_local_data', { input: { tenantId, group: value.group, sectionKey: value.sectionKey, studentQuery: value.student.trim(), textQuery: value.query.trim(), dateFrom: range.dateFrom, dateTo: range.dateTo, hasAttachment: value.hasAttachment, offset: requestedPage * PAGE_SIZE, limit: PAGE_SIZE } });
      if (requestGeneration !== generation || tenantId !== tenant || tenantId !== options.getTenantId().trim())
        return;
      if (result?.ok === false)
        throw new Error(result.error || 'local_data_search_failed');
      if (value.group === 'archive-boards')
        archiveVisibleTotal = returnedArchiveCount;
      records = result.records || [];
      total = numeric(result.total);
      selectedIndex = selectedKey ? records.findIndex(record => recordCanonicalKey(record) === selectedKey) : -1;
      if (selectedIndex < 0)
        selectedIndex = records.length ? 0 : -1;
      selectedKey = selectedIndex >= 0 ? recordCanonicalKey(records[selectedIndex]) : '';
      setStatus(total ? `이 PC에서 ${total.toLocaleString('ko-KR')}건을 찾았습니다.` : '조건에 맞는 자료가 없습니다.');
    }
    catch (reason) {
      if (requestGeneration !== generation || tenantId !== tenant || tenantId !== options.getTenantId().trim())
        return;
      failure = String((reason as Error).message || reason);
      setStatus(/시작일/.test(failure) ? failure : '검색 결과를 불러오지 못했습니다. 검색어와 필터는 유지됩니다.');
    }
    finally {
      if (requestGeneration === generation && tenantId === tenant && tenantId === options.getTenantId().trim()) {
        loading = false;
        renderRecords();
        renderDetail();
        remember();
      }
    }
  }
  async function refresh() { ensureTenant(); const current = tenant; const requests = [loadCounts(), search()]; const searchRequest = generation; const countRequest = countGeneration; const results = await Promise.allSettled(requests); if (current !== tenant || current !== options.getTenantId().trim() || searchRequest !== generation || countRequest !== countGeneration)
    return; element('dataExplorerLocalStatus').textContent = results[0].status === 'rejected' ? '자료 종류별 전체 건수를 확인하지 못했습니다. 검색 결과는 별도로 확인하세요.' : failure ? '검색 결과를 확인하지 못했습니다. 다시 시도해 주세요.' : '이 PC의 자료를 조회했습니다. 파일 존재 여부는 파일 열기로 확인합니다.'; }
  function changedFilters() { page = 0; selectedKey = ''; selectedIndex = -1; selectedMedia = ''; listScroll = 0; detailScroll = 0; ++actionGeneration; void search(); }
  function reset() { setFilters(initialFilters()); changedFilters(); }
  async function openSourceDocument() {
    const record = selected();
    if (record?.sectionKey !== 'work-notes' || privateRecord(record))
      return;
    const pageId = firstText(record.payload, ['pageId']);
    if (!pageId)
      return;
    if (isDeskRestoreBlocked()) {
      element('dataExplorerLocalStatus').textContent = '자료함 복원 상태를 확인하는 동안 편집을 시작할 수 없습니다.';
      return;
    }
    const currentTenant = tenant;
    const key = recordCanonicalKey(record);
    const requestGeneration = ++actionGeneration;
    element<HTMLButtonElement>('dataOpenWorkNote').disabled = true;
    try {
      for (const workspace of ['work_materials', 'lesson_materials', 'student_learning_materials'] as const) {
        const result = await invoke<{
          ok?: boolean;
          tenantId?: string;
          workspace?: string;
          page?: {
            pageId: string;
          };
          error?: string;
        }>('get_local_workspace_page', { tenantId: currentTenant, workspace, pageId });
        if (requestGeneration !== actionGeneration || currentTenant !== tenant || currentTenant !== options.getTenantId().trim() || key !== recordCanonicalKey(selected()) || isDeskRestoreBlocked())
          return;
        if (result.ok === false && result.error === 'local_workspace_page_not_found')
          continue;
        if (result.ok !== true || result.page?.pageId !== pageId || result.tenantId !== currentTenant || result.workspace !== workspace)
          throw new Error('연결 문서의 저장 위치와 식별자를 확인하지 못했습니다.');
        remember();
        if (workspace === 'student_learning_materials')
          await openWorkspaceWorkNoteReader(currentTenant, workspace, pageId);
        else
          document.dispatchEvent(new CustomEvent('desk:open-document', { detail: { tenantId: currentTenant, pageId, workspace: workspace === 'lesson_materials' ? 'lessonMaterials' : 'workNotes' } }));
        return;
      }
      throw new Error('연결 문서를 찾을 수 없습니다. 현재 자료함과 휴지통을 확인해 주세요.');
    }
    catch {
      if (requestGeneration === actionGeneration && currentTenant === tenant && currentTenant === options.getTenantId().trim())
        element('dataExplorerLocalStatus').textContent = '연결 문서를 열지 못했습니다. 현재 자료함과 휴지통을 확인하고 다시 시도해 주세요.';
    }
    finally {
      if (requestGeneration === actionGeneration)
        element<HTMLButtonElement>('dataOpenWorkNote').disabled = false;
    }
  }
  form.addEventListener('submit', event => { event.preventDefault(); changedFilters(); });
  groupSelect.addEventListener('change', () => { sectionKey = ''; changedFilters(); });
  studentInput.addEventListener('change', () => { studentFilter = studentInput.value; changedFilters(); });
  for (const input of [periodSelect, attachmentInput, dateFrom, dateTo])
    input.addEventListener('change', changedFilters);
  element('dataExplorerGroups').addEventListener('click', event => { const button = (event.target as Element).closest<HTMLElement>('[data-data-group]'); if (!button)
    return; groupSelect.value = button.dataset.dataGroup || ''; sectionKey = ''; changedFilters(); });
  list.addEventListener('click', event => { const button = (event.target as Element).closest<HTMLElement>('button'); if (!button)
    return; if (button.hasAttribute('data-search-retry')) {
    void search();
    return;
  } if (button.hasAttribute('data-search-reset')) {
    reset();
    return;
  } if (button.hasAttribute('data-search-cancel')) {
    ++generation;
    loading = false;
    cancelled = true;
    setStatus('검색을 취소했습니다.');
    renderRecords();
    renderDetail();
    return;
  } if (button.dataset.dataRecordIndex !== undefined) {
    listScroll = list.scrollTop;
    detailScroll = 0;
    selectedIndex = Number(button.dataset.dataRecordIndex);
    selectedKey = recordCanonicalKey(selected());
    selectedMedia = '';
    ++actionGeneration;
    renderRecords();
    renderDetail();
    remember();
  } });
  element('dataAttachmentList').addEventListener('click', async (event) => {
    const button = (event.target as Element).closest<HTMLButtonElement>('button');
    if (button?.hasAttribute('data-source-document')) {
      void openSourceDocument();
      return;
    }
    if (!button?.dataset.openMedia || privateRecord(selected()))
      return;
    const mediaId = button.dataset.openMedia;
    const kind = button.dataset.attachmentKind || '';
    const currentTenant = tenant;
    const key = selectedKey;
    const requestGeneration = ++actionGeneration;
    selectedMedia = mediaId;
    button.disabled = true;
    try {
      if (!DESIGN_PREVIEW) {
        const result = await invoke<{
          ok?: boolean;
          error?: string;
        }>('open_local_data_attachment', { tenantId: currentTenant, mediaId, attachmentKind: kind });
        if (result.ok === false)
          throw new Error(result.error || 'media_open_failed');
      }
      if (requestGeneration !== actionGeneration || currentTenant !== tenant || currentTenant !== options.getTenantId().trim() || key !== selectedKey || privateRecord(selected()))
        return;
      attachmentStates.delete(`${tenant}:${kind}:${mediaId}`);
      element('dataExplorerLocalStatus').textContent = '이 PC의 기본 프로그램에 파일 열기를 요청했습니다.';
      renderDetail();
    }
    catch (reason) {
      if (requestGeneration !== actionGeneration || currentTenant !== tenant || currentTenant !== options.getTenantId().trim() || key !== selectedKey || privateRecord(selected()))
        return;
      attachmentStates.set(`${tenant}:${kind}:${mediaId}`, { kind: attachmentFailureKind(reason), message: String((reason as Error).message || reason) });
      renderDetail();
    }
    finally {
      if (requestGeneration === actionGeneration)
        button.disabled = false;
    }
  });
  element('dataOpenArchiveBoard').addEventListener('click', () => { const archiveId = firstText(selected()?.payload || {}, ['archiveId']); if (!archiveId)
    return; const currentTenant = tenant; remember(); void openArchiveBoardViewer(currentTenant, archiveId).catch(() => { if (currentTenant === tenant)
    element('dataExplorerLocalStatus').textContent = '보관 보드를 열지 못했습니다. 다시 시도해 주세요.'; }); });
  element('dataOpenWorkNote').addEventListener('click', () => void openSourceDocument());
  element('dataExplorerReset').addEventListener('click', reset);
  element('dataExplorerRefresh').addEventListener('click', () => void refresh());
  element('dataExplorerPrevious').addEventListener('click', () => { if (!loading && page > 0) {
    page--;
    listScroll = 0;
    void search();
  } });
  element('dataExplorerNext').addEventListener('click', () => { if (!loading && (page + 1) * PAGE_SIZE < total) {
    page++;
    listScroll = 0;
    void search();
  } });
  element('dataExplorerGroupDisclosure').addEventListener('click', () => { const button = element('dataExplorerGroupDisclosure'); const open = element('dataOverviewSection').classList.toggle('groups-open'); button.setAttribute('aria-expanded', String(open)); });
  element('dataExplorerDetailToggle').addEventListener('click', () => { detailClosed = !detailClosed; renderDetail(); remember(); });
  list.addEventListener('scroll', () => { if (!loading && !failure && !cancelled)
    listScroll = list.scrollTop; });
  detailColumn.addEventListener('scroll', () => { if (!loading && !failure && !cancelled)
    detailScroll = detailColumn.scrollTop; });
  window.addEventListener('desk:student-privacy-changed', () => { ++actionGeneration; if (isStudentPrivacyEnabled()) {
    studentFilter = studentInput.value || studentFilter;
    studentInput.value = '';
  }
  else
    studentInput.value = studentFilter; syncFilters(); renderRecords(); renderDetail(); });
  return { async open(input: ExplorerOpenOptions = {}) { ensureTenant(); if (!loading && !failure && !cancelled)
      remember(); const previous = filters(); const next = applyExplorerOpenOptions(previous, input); setFilters(next); if (JSON.stringify(previous) !== JSON.stringify(next)) {
      page = 0;
      selectedKey = '';
      selectedIndex = -1;
      listScroll = 0;
      detailScroll = 0;
    } await refresh(); }, refresh };
}
