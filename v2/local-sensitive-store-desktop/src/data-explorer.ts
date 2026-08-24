import { invoke } from "@tauri-apps/api/core";
import { openArchiveBoardViewer, searchArchiveBoards, type ArchiveBoardSummary } from "./archive-board-explorer";

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
  "work-notes": "업무 노트",
};

const PREVIEW_RECORDS: LocalDataRecord[] = [
  {
    sectionKey: "observations", sectionLabel: "수업 관찰", groupKey: "care", updatedAtMs: Date.parse("2026-08-04T11:24:00+09:00"), dateKey: "2026-08-04",
    payload: { studentName: "김하늘", studentCode: "1", observation: "수업 참여 태도와 또래 관계 행동을 관찰했습니다.", memo: "모둠 활동에서 친구의 의견을 경청하고 자신의 생각을 차분히 설명했습니다." },
  },
  {
    sectionKey: "teacher-counseling-sessions", sectionLabel: "교사 상담기록", groupKey: "care", updatedAtMs: Date.parse("2026-08-03T16:12:00+09:00"), dateKey: "2026-08-03",
    payload: { studentName: "박도윤", studentCode: "2", summary: "진로 고민 상담 및 학습 계획을 함께 정리했습니다.", topics: ["진로", "학습 계획"] },
  },
  {
    sectionKey: "attendance-document-requests", sectionLabel: "출결 증빙 요청", groupKey: "attendance", updatedAtMs: Date.parse("2026-08-04T09:36:00+09:00"), dateKey: "2026-08-04", hasAttachment: true,
    payload: { studentName: "이서윤", studentCode: "3", status: "병결", reason: "8월 4일 병결로 처리했습니다. 보호자가 진료확인서를 제출했습니다.", attachments: [{ mediaId: "preview-proof", fileName: "진료확인서_이서윤.pdf", contentType: "application/pdf", size: 253952 }] },
  },
  {
    sectionKey: "eval-results", sectionLabel: "평가 기록", groupKey: "learning", updatedAtMs: Date.parse("2026-08-02T15:05:00+09:00"), dateKey: "2026-08-02",
    payload: { studentName: "정민준", studentId: "4", title: "수학 단원평가", summary: "분수의 덧셈 단원평가 결과 및 피드백", result: "개념 이해가 안정적이며 풀이 과정을 정확히 설명했습니다." },
  },
  {
    sectionKey: "student-record-drafts", sectionLabel: "학생부 초안", groupKey: "student-record", updatedAtMs: Date.parse("2026-08-01T14:20:00+09:00"), dateKey: "2026-08-01",
    payload: { studentName: "최하은", studentCode: "5", title: "행동특성 및 종합의견 초안", content: "책임감 있게 학급 활동에 참여하고 친구를 배려하는 태도가 돋보입니다." },
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

export function initDataExplorer(options: ExplorerOptions) {
  let records: LocalDataRecord[] = [];
  let selectedIndex = -1;
  let page = 0;
  let total = 0;
  let sectionKey = "";
  let loading = false;

  const queryInput = element<HTMLInputElement>("dataExplorerQuery");
  const groupSelect = element<HTMLSelectElement>("dataExplorerGroup");
  const studentInput = element<HTMLInputElement>("dataExplorerStudent");
  const periodSelect = element<HTMLSelectElement>("dataExplorerPeriod");
  const attachmentInput = element<HTMLInputElement>("dataExplorerHasAttachment");

  function setStatus(message: string) {
    element("dataOverviewStatus").textContent = message;
  }

  function renderGroups(counts: Record<string, number>) {
    element("dataGroupCareCount").textContent = String(numeric(counts.care));
    element("dataGroupAttendanceCount").textContent = String(numeric(counts.attendance));
    element("dataGroupLearningCount").textContent = String(numeric(counts.learning));
    element("dataGroupStudentRecordCount").textContent = String(numeric(counts["student-record"]));
    element("dataGroupWorkNotesCount").textContent = String(numeric(counts["work-notes"]));
    element("dataGroupArchiveBoardsCount").textContent = String(numeric(counts["archive-boards"]));
    document.querySelectorAll<HTMLButtonElement>("[data-data-group]").forEach((button) => {
      const selected = Boolean(groupSelect.value) && button.dataset.dataGroup === groupSelect.value;
      button.classList.toggle("is-selected", selected);
      button.setAttribute("aria-pressed", String(selected));
    });
  }

  function renderRecords() {
    const list = element("localDataRecordList");
    element("dataExplorerTotal").textContent = `${total.toLocaleString("ko-KR")}건`;
    const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));
    element("dataExplorerPage").textContent = `${Math.min(page + 1, pageCount)} / ${pageCount}`;
    element<HTMLButtonElement>("dataExplorerPrevious").disabled = loading || page <= 0;
    element<HTMLButtonElement>("dataExplorerNext").disabled = loading || (page + 1) * PAGE_SIZE >= total;
    if (loading) {
      list.innerHTML = `<div class="data-list-message"><i class="fa-solid fa-spinner fa-spin" aria-hidden="true"></i><span>로컬 DB를 검색하고 있습니다.</span></div>`;
      return;
    }
    if (!records.length) {
      list.innerHTML = `<div class="data-list-message"><i class="fa-solid fa-magnifying-glass" aria-hidden="true"></i><span>조건에 맞는 저장 자료가 없습니다.</span></div>`;
      selectedIndex = -1;
      renderDetail();
      return;
    }
    list.innerHTML = records.map((record, index) => `
      <button class="data-record-row${index === selectedIndex ? " is-selected" : ""}" type="button" role="option" aria-selected="${index === selectedIndex}" data-data-record-index="${index}">
        <span class="data-record-title"><strong>${escapeHtml(recordTitle(record))}</strong>${record.hasAttachment ? '<i class="fa-solid fa-paperclip" aria-label="첨부파일 있음"></i>' : ""}</span>
        <span class="data-record-date">${escapeHtml(shortDate(record))} · ${escapeHtml(recordStatusLabel(record) || sectionLabel(record))}</span>
        <span class="data-record-summary">${escapeHtml(recordSummary(record))}</span>
        <i class="fa-solid fa-chevron-right data-record-arrow" aria-hidden="true"></i>
      </button>
    `).join("");
  }

  function renderAttachments(record: LocalDataRecord) {
    const attachments = recordAttachments(record);
    element("dataAttachmentCount").textContent = `${attachments.length}개`;
    const section = element<HTMLElement>("dataAttachments");
    const list = element("dataAttachmentList");
    section.hidden = !attachments.length;
    list.innerHTML = attachments.map((attachment) => `
      <div class="data-attachment-row">
        <span class="data-file-icon"><i class="fa-solid fa-file-pdf" aria-hidden="true"></i></span>
        <span><strong>${escapeHtml(attachment.fileName)}</strong><small>${escapeHtml(fileTypeLabel(attachment.contentType, attachment.fileName))} · ${escapeHtml(byteText(attachment.size))}</small></span>
        <button type="button" data-open-media="${escapeHtml(attachment.mediaId)}" data-attachment-kind="${escapeHtml(attachment.attachmentKind)}"${attachment.mediaId ? "" : " disabled"}>${attachment.mediaId ? "열기" : "파일 정보만 있음"}</button>
      </div>
    `).join("");
  }

  function renderDetail() {
    const empty = element<HTMLElement>("dataExplorerEmpty");
    const detail = element<HTMLElement>("dataExplorerDetail");
    const record = records[selectedIndex];
    empty.hidden = Boolean(record);
    detail.hidden = !record;
    const openBoardButton = element<HTMLButtonElement>("dataOpenArchiveBoard");
    openBoardButton.hidden = record?.sectionKey !== "archive-board";
    if (!record) return;
    element("dataDetailTitle").textContent = recordTitle(record);
    element("dataDetailStudent").textContent = record.sectionKey === "work-notes" ? "학급 업무" : studentName(record);
    element("dataDetailKind").textContent = sectionLabel(record);
    element("dataDetailSavedAt").textContent = formatDate(record.updatedAtMs, record.dateKey);
    const parts = contentParts(record.payload, record.sectionKey);
    element("dataDetailBody").innerHTML = (parts.length ? parts : ["저장된 원문 필드가 없습니다. 원본 JSON에서 전체 내용을 확인할 수 있습니다."])
      .map((part) => `<p>${escapeHtml(part)}</p>`).join("");
    renderAttachments(record);
    element("dataJsonDetail").textContent = JSON.stringify(record.payload, null, 2);
  }

  async function loadCounts() {
    if (DESIGN_PREVIEW) {
      renderGroups({ care: 327, attendance: 93, learning: 874, "student-record": 174, "work-notes": 12, "archive-boards": 4 });
      return;
    }
    const tenantId = options.getTenantId().trim();
    if (!tenantId) {
      renderGroups({});
      return;
    }
    const [overview, archiveBoards] = await Promise.all([
      invoke<LocalOverview>("get_local_overview", { tenantId }),
      searchArchiveBoards(tenantId, "", 1),
    ]);
    if (overview?.ok === false) throw new Error(overview.error || "local_overview_failed");
    const sections = Array.isArray(overview.sections) ? overview.sections : [];
    const countFor = (group: typeof GROUP_KEYS[number]) => sections.reduce((sum, section) => sum + (GROUP_SECTIONS[group].includes(section.key as never) ? numeric(section.count) : 0), 0);
    renderGroups({ care: countFor("care"), attendance: countFor("attendance"), learning: countFor("learning"), "student-record": countFor("student-record"), "work-notes": countFor("work-notes"), "archive-boards": archiveBoards.total });
  }

  function previewSearch(): SearchResult {
    const query = queryInput.value.trim().toLocaleLowerCase("ko-KR");
    const student = studentInput.value.trim().toLocaleLowerCase("ko-KR");
    const range = dateRange(periodSelect.value);
    const filtered = PREVIEW_RECORDS.filter((record) => !groupSelect.value || record.groupKey === groupSelect.value)
      .filter((record) => !sectionKey || record.sectionKey === sectionKey)
      .filter((record) => !query || JSON.stringify(record.payload).toLocaleLowerCase("ko-KR").includes(query))
      .filter((record) => !student || studentName(record).toLocaleLowerCase("ko-KR").includes(student))
      .filter((record) => !range.dateFrom || String(record.dateKey || "") >= range.dateFrom)
      .filter((record) => !range.dateTo || String(record.dateKey || "") <= range.dateTo)
      .filter((record) => !attachmentInput.checked || record.hasAttachment);
    return { ok: true, total: filtered.length, records: filtered.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE) };
  }

  async function search() {
    const tenantId = options.getTenantId().trim();
    if (!tenantId && !DESIGN_PREVIEW) {
      records = [];
      total = 0;
      selectedIndex = -1;
      setStatus("설정에서 교사 로그인으로 학급을 먼저 연결해 주세요.");
      renderRecords();
      return;
    }
    loading = true;
    setStatus("로컬 DB에서 검색 중입니다.");
    renderRecords();
    try {
      const range = dateRange(periodSelect.value);
      let result: SearchResult;
      if (!DESIGN_PREVIEW && groupSelect.value === "archive-boards") {
        const archiveResult = await searchArchiveBoards(tenantId, [queryInput.value, studentInput.value].map((value) => value.trim()).filter(Boolean).join(" "), 100);
        const filtered = archiveResult.boards
          .filter((board) => !range.dateFrom || new Date(board.importedAt).toISOString().slice(0, 10) >= range.dateFrom)
          .filter((board) => !range.dateTo || new Date(board.importedAt).toISOString().slice(0, 10) <= range.dateTo)
          .filter((board) => !attachmentInput.checked || board.fileCount > 0)
          .map(archiveBoardRecord);
        result = { ok: true, total: filtered.length, records: filtered.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE) };
      } else result = DESIGN_PREVIEW ? previewSearch() : await invoke<SearchResult>("search_local_data", {
        input: {
          tenantId,
          group: groupSelect.value,
          sectionKey,
          studentQuery: studentInput.value.trim(),
          textQuery: queryInput.value.trim(),
          dateFrom: range.dateFrom,
          dateTo: range.dateTo,
          hasAttachment: attachmentInput.checked,
          offset: page * PAGE_SIZE,
          limit: PAGE_SIZE,
        },
      });
      if (result?.ok === false) throw new Error(result.error || "local_data_search_failed");
      records = Array.isArray(result.records) ? result.records : [];
      total = numeric(result.total);
      selectedIndex = records.length ? 0 : -1;
      setStatus(total ? `로컬 DB에서 ${total.toLocaleString("ko-KR")}건을 찾았습니다.` : "조건에 맞는 저장 자료가 없습니다.");
    } catch (error) {
      records = [];
      total = 0;
      selectedIndex = -1;
      setStatus(`자료 검색 실패: ${String((error as Error)?.message || error)}`);
    } finally {
      loading = false;
      renderRecords();
      renderDetail();
    }
  }

  async function refresh() {
    await Promise.all([loadCounts(), search()]);
    element("dataExplorerLocalStatus").textContent = "로컬 DB에서 불러옴 · 인터넷 연결 없이 열람 가능";
  }

  function reset() {
    queryInput.value = "";
    groupSelect.value = "";
    studentInput.value = "";
    periodSelect.value = "30";
    attachmentInput.checked = false;
    sectionKey = "";
    page = 0;
    renderGroups({
      care: numeric(element("dataGroupCareCount").textContent),
      attendance: numeric(element("dataGroupAttendanceCount").textContent),
      learning: numeric(element("dataGroupLearningCount").textContent),
      "student-record": numeric(element("dataGroupStudentRecordCount").textContent),
      "work-notes": numeric(element("dataGroupWorkNotesCount").textContent),
      "archive-boards": numeric(element("dataGroupArchiveBoardsCount").textContent),
    });
    void search();
  }

  element<HTMLFormElement>("dataExplorerSearchForm").addEventListener("submit", (event) => {
    event.preventDefault();
    sectionKey = "";
    page = 0;
    void search();
  });
  groupSelect.addEventListener("change", () => { sectionKey = ""; page = 0; renderGroups({ care: numeric(element("dataGroupCareCount").textContent), attendance: numeric(element("dataGroupAttendanceCount").textContent), learning: numeric(element("dataGroupLearningCount").textContent), "student-record": numeric(element("dataGroupStudentRecordCount").textContent), "work-notes": numeric(element("dataGroupWorkNotesCount").textContent), "archive-boards": numeric(element("dataGroupArchiveBoardsCount").textContent) }); void search(); });
  studentInput.addEventListener("change", () => { page = 0; void search(); });
  periodSelect.addEventListener("change", () => { page = 0; void search(); });
  attachmentInput.addEventListener("change", () => { page = 0; void search(); });
  element("dataExplorerGroups").addEventListener("click", (event) => {
    const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>("[data-data-group]");
    if (!button) return;
    groupSelect.value = button.dataset.dataGroup || "";
    sectionKey = "";
    page = 0;
    renderGroups({ care: numeric(element("dataGroupCareCount").textContent), attendance: numeric(element("dataGroupAttendanceCount").textContent), learning: numeric(element("dataGroupLearningCount").textContent), "student-record": numeric(element("dataGroupStudentRecordCount").textContent), "work-notes": numeric(element("dataGroupWorkNotesCount").textContent), "archive-boards": numeric(element("dataGroupArchiveBoardsCount").textContent) });
    void search();
  });
  element("localDataRecordList").addEventListener("click", (event) => {
    const row = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>("[data-data-record-index]");
    if (!row) return;
    selectedIndex = Number(row.dataset.dataRecordIndex || 0) || 0;
    renderRecords();
    renderDetail();
  });
  element("dataAttachmentList").addEventListener("click", async (event) => {
    const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>("[data-open-media]");
    if (!button || !button.dataset.openMedia) return;
    button.disabled = true;
    try {
      if (!DESIGN_PREVIEW) {
        const result = await invoke<{ ok?: boolean; error?: string }>("open_local_data_attachment", { tenantId: options.getTenantId().trim(), mediaId: button.dataset.openMedia, attachmentKind: button.dataset.attachmentKind });
        if (result?.ok === false) throw new Error(result.error || "media_open_failed");
      }
      element("dataExplorerLocalStatus").textContent = "첨부파일을 이 PC의 기본 프로그램으로 열었습니다.";
    } catch (error) {
      element("dataExplorerLocalStatus").textContent = `첨부파일 열기 실패: ${String((error as Error)?.message || error)}`;
    } finally {
      button.disabled = false;
    }
  });
  element("dataOpenArchiveBoard").addEventListener("click", () => {
    const archiveId = firstText(records[selectedIndex]?.payload || {}, ["archiveId"]);
    if (!archiveId) return;
    void openArchiveBoardViewer(options.getTenantId().trim(), archiveId).catch((error) => {
      element("dataExplorerLocalStatus").textContent = `보관 보드 열기 실패: ${String((error as Error)?.message || error)}`;
    });
  });
  element("dataExplorerReset").addEventListener("click", reset);
  element("dataExplorerRefresh").addEventListener("click", () => void refresh());
  element("dataExplorerPrevious").addEventListener("click", () => { if (page > 0) { page -= 1; void search(); } });
  element("dataExplorerNext").addEventListener("click", () => { if ((page + 1) * PAGE_SIZE < total) { page += 1; void search(); } });

  if (DESIGN_PREVIEW) {
    element("homeTenantLabel").textContent = "수영초등학교 5학년 1반";
    element("homeConnectionText").textContent = "연결됨";
    element("homeBackupText").textContent = "어제 오후 5:58";
  }

  return {
    async open(openOptions: ExplorerOpenOptions = {}) {
      queryInput.value = openOptions.query || queryInput.value;
      sectionKey = openOptions.sectionKey || "";
      groupSelect.value = openOptions.group || groupForSection(sectionKey);
      attachmentInput.checked = openOptions.hasAttachment === true;
      page = 0;
      await refresh();
    },
    refresh,
  };
}
