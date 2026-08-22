import { invoke } from "@tauri-apps/api/core";
import {
  byteText,
  contentParts,
  dateRange,
  element,
  escapeHtml,
  fileTypeLabel,
  firstText,
  formatDate,
  numeric,
  recordAttachments,
  recordSummary,
  sectionLabel,
  shortDate,
  type LocalDataRecord,
  type SearchResult,
} from "./data-explorer";

type LocalStudent = {
  studentId: string;
  studentName: string;
  recordCount: number;
  lastUpdatedMs: number;
};

type StudentListResult = {
  ok?: boolean;
  total?: number;
  students?: LocalStudent[];
  error?: string;
};

type StudentTimelineOptions = {
  getTenantId: () => string;
};

const PAGE_SIZE = 40;
const DESIGN_PREVIEW = new URLSearchParams(window.location.search).get("designPreview") === "students";
const PREVIEW_STUDENTS: LocalStudent[] = [
  ["1", "김하늘"], ["2", "박도윤"], ["3", "이서윤"], ["4", "정민준"],
  ["5", "최하은"], ["6", "김지우"], ["7", "이준호"], ["8", "한서연"],
  ["9", "박지민"], ["10", "윤서아"], ["11", "양태민"], ["12", "고예린"],
  ["13", "김도현"], ["14", "서지안"], ["15", "오현우"], ["16", "임유나"],
  ["17", "장시우"], ["18", "백채원"], ["19", "문준서"], ["20", "신예은"],
  ["21", "권우진"], ["22", "조아린"], ["23", "노지호"], ["24", "황서현"],
].map(([studentId, studentName], index) => ({
  studentId,
  studentName,
  recordCount: studentId === "3" ? 9 : Math.max(1, 7 - (index % 5)),
  lastUpdatedMs: Date.parse(`2026-08-${String(Math.max(1, 4 - (index % 4))).padStart(2, "0")}T09:36:00+09:00`),
}));

const PREVIEW_TIMELINE: LocalDataRecord[] = [
  {
    sectionKey: "attendance-document-requests", sectionLabel: "출결 증빙 요청", groupKey: "attendance",
    updatedAtMs: Date.parse("2026-08-04T09:36:00+09:00"), dateKey: "2026-08-04", hasAttachment: true,
    payload: { studentName: "이서윤", studentCode: "3", status: "병결", title: "병결 처리 · 진료확인서 제출", reason: "8월 4일 병결로 처리했습니다. 보호자가 진료확인서를 제출했습니다.", attachments: [{ mediaId: "preview-proof", fileName: "진료확인서_이서윤.pdf", contentType: "application/pdf", size: 253952 }] },
  },
  {
    sectionKey: "teacher-counseling-sessions", sectionLabel: "교사 상담기록", groupKey: "care",
    updatedAtMs: Date.parse("2026-08-03T16:12:00+09:00"), dateKey: "2026-08-03",
    payload: { studentName: "이서윤", studentCode: "3", title: "진로 고민과 학습 계획", summary: "진로 고민과 학습 계획을 함께 정리했습니다.", topics: ["진로", "학습 계획"] },
  },
  {
    sectionKey: "eval-results", sectionLabel: "평가 기록", groupKey: "learning",
    updatedAtMs: Date.parse("2026-08-02T15:05:00+09:00"), dateKey: "2026-08-02",
    payload: { studentName: "이서윤", studentId: "3", title: "수학 단원평가 피드백", summary: "분수의 덧셈 단원평가 결과와 다음 학습 목표를 확인했습니다.", result: "풀이 과정을 정확히 설명했습니다." },
  },
  {
    sectionKey: "observations", sectionLabel: "수업 관찰", groupKey: "care",
    updatedAtMs: Date.parse("2026-07-30T11:24:00+09:00"), dateKey: "2026-07-30",
    payload: { studentName: "이서윤", studentCode: "3", title: "모둠 활동 참여 태도", observation: "모둠 활동 참여 태도를 관찰했습니다.", memo: "친구 의견을 경청하고 자신의 생각을 차분히 설명했습니다." },
  },
  {
    sectionKey: "student-record-drafts", sectionLabel: "학생부 초안", groupKey: "student-record",
    updatedAtMs: Date.parse("2026-07-25T14:20:00+09:00"), dateKey: "2026-07-25",
    payload: { studentName: "이서윤", studentCode: "3", title: "행동특성 초안", content: "책임감 있게 학급 활동에 참여하고 친구를 배려하는 태도가 돋보입니다." },
  },
];

function studentLabel(student: LocalStudent | undefined) {
  if (!student) return "학생";
  return student.studentName && student.studentName !== student.studentId ? student.studentName : "이름 미확인";
}

function studentIdentifierLabel(studentId: string) {
  if (/^\d{1,3}$/u.test(studentId)) return `${studentId}번`;
  const short = studentId.length > 16 ? `${studentId.slice(0, 16)}…` : studentId;
  return `식별번호 ${short}`;
}

function timelineIcon(record: LocalDataRecord) {
  if (record.groupKey === "attendance") return "fa-calendar-check";
  if (record.groupKey === "learning") return "fa-chart-simple";
  if (record.groupKey === "student-record") return "fa-file-lines";
  if (record.sectionKey === "observations") return "fa-eye";
  return "fa-comment";
}

function timelineTitle(record: LocalDataRecord) {
  return firstText(record.payload, ["title", "status"]) || recordSummary(record);
}

function timelineCategory(record: LocalDataRecord) {
  if (record.groupKey === "attendance") return "출결·증빙";
  if (record.groupKey === "learning") return "평가·학습";
  if (record.groupKey === "student-record") return "학생부";
  if (record.sectionKey === "observations") return "관찰";
  return "상담";
}

function timelineDate(record: LocalDataRecord) {
  const match = String(record.dateKey || "").match(/^\d{4}-(\d{2})-(\d{2})$/);
  if (!match) return shortDate(record);
  return `${Number(match[1])}월 ${Number(match[2])}일`;
}

export function initStudentTimeline(options: StudentTimelineOptions) {
  let students: LocalStudent[] = [];
  let studentTotal = 0;
  let selectedStudentId = "";
  let records: LocalDataRecord[] = [];
  let recordTotal = 0;
  let selectedRecordIndex = -1;
  let page = 0;
  let loading = false;

  const queryInput = element<HTMLInputElement>("studentTimelineQuery");
  const periodSelect = element<HTMLSelectElement>("studentTimelinePeriod");
  const groupSelect = element<HTMLSelectElement>("studentTimelineGroup");

  function selectedStudent() {
    return students.find((student) => student.studentId === selectedStudentId);
  }

  function setStatus(message: string) {
    element("studentTimelineStatus").textContent = message;
  }

  function renderStudents() {
    element("studentRosterCount").textContent = `학생 ${studentTotal.toLocaleString("ko-KR")}명`;
    const list = element("studentRosterList");
    if (!students.length) {
      list.innerHTML = `<div class="student-list-message"><i class="fa-solid fa-user-slash" aria-hidden="true"></i><span>조건에 맞는 학생이 없습니다.</span></div>`;
      return;
    }
    list.innerHTML = students.map((student) => `
      <button type="button" class="student-roster-row${student.studentId === selectedStudentId ? " is-selected" : ""}" data-student-id="${escapeHtml(student.studentId)}" aria-pressed="${student.studentId === selectedStudentId}">
        <span title="${escapeHtml(student.studentId)}">${escapeHtml(studentIdentifierLabel(student.studentId))}</span><strong>${escapeHtml(studentLabel(student))}</strong>
      </button>
    `).join("");
  }

  function renderTimeline() {
    const student = selectedStudent();
    element("studentTimelineName").textContent = studentLabel(student);
    element("studentTimelineNumber").textContent = student ? studentIdentifierLabel(student.studentId) : "";
    element("studentTimelineSummary").textContent = student
      ? `저장 자료 ${student.recordCount.toLocaleString("ko-KR")}건 · 최근 기록 ${records[0]?.dateKey ? records[0].dateKey.replace(/-/g, ". ") + "." : "-"}`
      : "학생을 선택하면 저장 기록이 표시됩니다.";
    const list = element("studentTimelineList");
    if (loading) {
      list.innerHTML = `<div class="student-list-message"><i class="fa-solid fa-spinner fa-spin" aria-hidden="true"></i><span>학생 기록을 불러오고 있습니다.</span></div>`;
      return;
    }
    if (!records.length) {
      list.innerHTML = `<div class="student-list-message"><i class="fa-solid fa-clock-rotate-left" aria-hidden="true"></i><span>조건에 맞는 학생 기록이 없습니다.</span></div>`;
      return;
    }
    list.innerHTML = records.map((record, index) => `
      <button type="button" class="student-timeline-row is-${escapeHtml(record.groupKey)}${index === selectedRecordIndex ? " is-selected" : ""}" data-student-record-index="${index}" aria-pressed="${index === selectedRecordIndex}">
        <i class="fa-solid fa-circle student-timeline-dot" aria-hidden="true"></i>
        <span class="student-timeline-icon"><i class="fa-solid ${timelineIcon(record)}" aria-hidden="true"></i></span>
        <span class="student-timeline-copy"><small>${escapeHtml(timelineDate(record))}<b>·</b>${escapeHtml(timelineCategory(record))}</small><strong>${escapeHtml(timelineTitle(record))}</strong></span>
        ${record.hasAttachment ? '<i class="fa-solid fa-paperclip student-timeline-attachment" aria-label="첨부파일 있음"></i>' : ""}
        <i class="fa-solid fa-chevron-right student-timeline-arrow" aria-hidden="true"></i>
      </button>
    `).join("");
    const pages = Math.max(1, Math.ceil(recordTotal / PAGE_SIZE));
    const pagination = element<HTMLElement>("studentTimelinePagination");
    pagination.hidden = pages <= 1;
    element("studentTimelinePage").textContent = `${page + 1} / ${pages}`;
    element<HTMLButtonElement>("studentTimelinePrevious").disabled = page <= 0;
    element<HTMLButtonElement>("studentTimelineNext").disabled = page + 1 >= pages;
  }

  function renderAttachments(record: LocalDataRecord) {
    const attachments = recordAttachments(record);
    const section = element<HTMLElement>("studentTimelineAttachments");
    section.hidden = !attachments.length;
    element("studentTimelineAttachmentCount").textContent = `${attachments.length}개`;
    element("studentTimelineAttachmentList").innerHTML = attachments.map((attachment) => `
      <div class="student-attachment-row">
        <span class="student-file-icon"><i class="fa-solid fa-file-pdf" aria-hidden="true"></i></span>
        <span><strong>${escapeHtml(attachment.fileName)}</strong><small>${escapeHtml(fileTypeLabel(attachment.contentType, attachment.fileName))} · ${escapeHtml(byteText(attachment.size))}</small></span>
        <button type="button" data-open-student-media="${escapeHtml(attachment.mediaId)}" data-attachment-kind="${escapeHtml(attachment.attachmentKind)}"${attachment.mediaId ? "" : " disabled"}>${attachment.mediaId ? "열기" : "파일 정보만 있음"}</button>
      </div>
    `).join("");
  }

  function renderDetail() {
    const empty = element<HTMLElement>("studentTimelineDetailEmpty");
    const detail = element<HTMLElement>("studentTimelineDetail");
    const record = records[selectedRecordIndex];
    empty.hidden = Boolean(record);
    detail.hidden = !record;
    if (!record) return;
    const student = selectedStudent();
    const name = studentLabel(student);
    element("studentTimelineDetailTitle").textContent = `${name} ${sectionLabel(record)}`;
    element("studentTimelineDetailStudent").textContent = name;
    element("studentTimelineDetailKind").textContent = timelineCategory(record);
    element("studentTimelineDetailDate").textContent = record.dateKey ? formatDate(0, record.dateKey) : "-";
    element("studentTimelineDetailSavedAt").textContent = formatDate(record.updatedAtMs, record.dateKey);
    const parts = contentParts(record.payload, record.sectionKey);
    element("studentTimelineDetailBody").innerHTML = (parts.length ? parts : ["저장된 원문 필드가 없습니다. 원본 JSON에서 전체 내용을 확인할 수 있습니다."])
      .map((part) => `<p>${escapeHtml(part)}</p>`).join("");
    renderAttachments(record);
    element("studentTimelineJson").textContent = JSON.stringify(record.payload, null, 2);
  }

  function previewStudentList(): StudentListResult {
    const query = queryInput.value.trim().toLocaleLowerCase("ko-KR");
    const filtered = PREVIEW_STUDENTS.filter((student) => !query || `${student.studentId} ${student.studentName}`.toLocaleLowerCase("ko-KR").includes(query));
    return { ok: true, total: query ? filtered.length : 24, students: filtered };
  }

  async function loadTimeline() {
    const student = selectedStudent();
    if (!student) {
      records = [];
      recordTotal = 0;
      selectedRecordIndex = -1;
      renderTimeline();
      renderDetail();
      return;
    }
    loading = true;
    renderTimeline();
    try {
      const range = dateRange(periodSelect.value);
      const result = DESIGN_PREVIEW
        ? (() => {
            const filtered = (student.studentId === "3" ? PREVIEW_TIMELINE : [])
              .filter((record) => !groupSelect.value || record.groupKey === groupSelect.value)
              .filter((record) => !range.dateFrom || String(record.dateKey || "") >= range.dateFrom)
              .filter((record) => !range.dateTo || String(record.dateKey || "") <= range.dateTo);
            return { ok: true, total: filtered.length, records: filtered } as SearchResult;
          })()
        : await invoke<SearchResult>("search_local_data", {
            input: {
              tenantId: options.getTenantId().trim(), studentId: student.studentId, group: groupSelect.value,
              sectionKey: "", studentQuery: "", textQuery: "", dateFrom: range.dateFrom, dateTo: range.dateTo,
              hasAttachment: false, offset: page * PAGE_SIZE, limit: PAGE_SIZE,
            },
          });
      if (result?.ok === false) throw new Error(result.error || "local_student_timeline_failed");
      records = Array.isArray(result.records) ? result.records : [];
      recordTotal = numeric(result.total);
      selectedRecordIndex = records.length ? 0 : -1;
      setStatus(DESIGN_PREVIEW
        ? "로컬 DB에서 불러옴 · 인터넷 연결 없이 열람 가능"
        : recordTotal
          ? `${student.studentName} 학생의 저장 자료 ${recordTotal.toLocaleString("ko-KR")}건을 불러왔습니다.`
          : "선택한 조건에 저장 자료가 없습니다.");
    } catch (error) {
      records = [];
      recordTotal = 0;
      selectedRecordIndex = -1;
      setStatus(`학생 기록 조회 실패: ${String((error as Error)?.message || error)}`);
    } finally {
      loading = false;
      renderTimeline();
      renderDetail();
    }
  }

  async function loadStudents() {
    const tenantId = options.getTenantId().trim();
    if (!tenantId && !DESIGN_PREVIEW) {
      students = [];
      studentTotal = 0;
      selectedStudentId = "";
      setStatus("설정에서 교사 로그인으로 학급을 먼저 연결해 주세요.");
      renderStudents();
      await loadTimeline();
      return;
    }
    try {
      const result = DESIGN_PREVIEW ? previewStudentList() : await invoke<StudentListResult>("list_local_students", {
        input: { tenantId, query: queryInput.value.trim(), offset: 0, limit: 200 },
      });
      if (result?.ok === false) throw new Error(result.error || "local_student_list_failed");
      students = Array.isArray(result.students) ? result.students : [];
      studentTotal = numeric(result.total);
      if (!students.some((student) => student.studentId === selectedStudentId)) {
        selectedStudentId = students.find((student) => student.studentId === "3")?.studentId || students[0]?.studentId || "";
      }
      page = 0;
      renderStudents();
      await loadTimeline();
    } catch (error) {
      students = [];
      studentTotal = 0;
      selectedStudentId = "";
      setStatus(`학생 목록 조회 실패: ${String((error as Error)?.message || error)}`);
      renderStudents();
      await loadTimeline();
    }
  }

  element<HTMLFormElement>("studentTimelineSearchForm").addEventListener("submit", (event) => {
    event.preventDefault();
    void loadStudents();
  });
  periodSelect.addEventListener("change", () => { page = 0; void loadTimeline(); });
  groupSelect.addEventListener("change", () => { page = 0; void loadTimeline(); });
  element("studentRosterList").addEventListener("click", (event) => {
    const row = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>("[data-student-id]");
    if (!row || row.dataset.studentId === selectedStudentId) return;
    selectedStudentId = row.dataset.studentId || "";
    page = 0;
    renderStudents();
    void loadTimeline();
  });
  element("studentTimelineList").addEventListener("click", (event) => {
    const row = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>("[data-student-record-index]");
    if (!row) return;
    selectedRecordIndex = Number(row.dataset.studentRecordIndex || 0) || 0;
    renderTimeline();
    renderDetail();
  });
  element("studentTimelineAttachmentList").addEventListener("click", async (event) => {
    const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>("[data-open-student-media]");
    if (!button || !button.dataset.openStudentMedia) return;
    button.disabled = true;
    try {
      if (!DESIGN_PREVIEW) {
        const result = await invoke<{ ok?: boolean; error?: string }>("open_local_data_attachment", {
          tenantId: options.getTenantId().trim(), mediaId: button.dataset.openStudentMedia, attachmentKind: button.dataset.attachmentKind,
        });
        if (result?.ok === false) throw new Error(result.error || "media_open_failed");
      }
      setStatus("첨부파일을 이 PC의 기본 프로그램으로 열었습니다.");
    } catch (error) {
      setStatus(`첨부파일 열기 실패: ${String((error as Error)?.message || error)}`);
    } finally {
      button.disabled = false;
    }
  });
  element("studentTimelinePrevious").addEventListener("click", () => { if (page > 0) { page -= 1; void loadTimeline(); } });
  element("studentTimelineNext").addEventListener("click", () => { if ((page + 1) * PAGE_SIZE < recordTotal) { page += 1; void loadTimeline(); } });

  if (DESIGN_PREVIEW) {
    element("homeTenantLabel").textContent = "수영초등학교 5학년 1반";
    element("homeConnectionText").textContent = "연결됨";
    element("homeBackupText").textContent = "어제 오후 5:58";
  }

  return { open: loadStudents, refresh: loadStudents };
}
