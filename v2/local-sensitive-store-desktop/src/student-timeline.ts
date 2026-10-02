import { updateTeacherRecordAction, teacherRecordLocator } from "./desk-record-editor";
import { invoke } from "@tauri-apps/api/core";
import { createStudentPrivacyToggle, isStudentPrivacyEnabled } from "./desk-privacy";
import { type QuickContext } from "./quick-observation";
import { isDeskRestoreBlocked } from "./desk-restore-lock";
import {
  byteText,
  contentParts,
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

export type LocalStudent = {
  studentId: string;
  studentName: string;
  recordCount: number;
  lastUpdatedMs: number;
  classNo?: number | null;
};

type StudentListResult = {
  ok?: boolean;
  total?: number;
  students?: LocalStudent[];
  error?: string;
};

type StudentTimelineOptions = {
  getTenantId: () => string;
  beforeStudentChange?: () => Promise<boolean>;
};

export type StudentRecordTarget = { kind: 'observation' | 'counseling'; recordId: string; studentId: string };
export function studentRecordDateRange(period: string, nowMs = Date.now()) {
  if (period === 'all') return {dateFrom:'',dateTo:''};
  const days = Math.max(1, Number(period || 30) || 30);
  const format = new Intl.DateTimeFormat('en-CA', {year:'numeric',month:'2-digit',day:'2-digit',timeZone:'Asia/Seoul'});
  const dateTo = format.format(new Date(nowMs));
  const midnight = Date.parse(`${dateTo}T00:00:00+09:00`);
  return {dateFrom:format.format(new Date(midnight - (days - 1) * 86_400_000)),dateTo};
}
export function mergeRosterStudents(tenantId: string, saved: LocalStudent[], context: QuickContext): LocalStudent[] {
  if (!context.ok || context.tenantId !== tenantId || !context.roster) return saved.slice();
  const archived = new Set(context.roster.students.filter(student => student.status === 'archived').map(student => student.id));
  const merged = new Map(saved.filter(student => !archived.has(student.studentId)).map(student => [student.studentId, {...student}]));
  for (const student of context.roster.students) {
    if (!student.id || student.status === 'archived') continue;
    const prior = merged.get(student.id);
    merged.set(student.id, { studentId: student.id, studentName: student.displayName, classNo: student.classNo, recordCount: prior?.recordCount || 0, lastUpdatedMs: prior?.lastUpdatedMs || 0 });
  }
  return [...merged.values()].sort((a, b) => (a.classNo || Number.MAX_SAFE_INTEGER) - (b.classNo || Number.MAX_SAFE_INTEGER) || a.studentId.localeCompare(b.studentId, 'ko'));
}
export function matchesStudentRecordTarget(tenantId: string, target: StudentRecordTarget, record: Record<string, unknown>): boolean {
  const key = target.kind === 'observation' ? 'docId' : 'sessionId';
  return record[key] === target.recordId && record.studentCode === target.studentId && (!record.tenantId || record.tenantId === tenantId);
}

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
  if (isStudentPrivacyEnabled()) return `학생 ${student.classNo ? String(student.classNo).padStart(2, "0") : "기록"}`;
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
  let rosterStudents: LocalStudent[] = [];
  let archivedStudentIds = new Set<string>();
  let studentTotal = 0;
  let selectedStudentId = "";
  let records: LocalDataRecord[] = [];
  let recordTotal = 0;
  let selectedRecordIndex = -1;
  let page = 0;
  let loading = false;
  let rosterGeneration = 0;
  let timelineGeneration = 0;
  let directRecord: LocalDataRecord | undefined;
  let directGeneration = 0;
  let rosterMessage = '';
  let activeTenant = '';
  const scrollMemory = new Map<string, number>();

  const queryInput = element<HTMLInputElement>("studentTimelineQuery");
  const periodSelect = element<HTMLSelectElement>("studentTimelinePeriod");
  const groupSelect = element<HTMLSelectElement>("studentTimelineGroup");

  function selectedStudent() {
    return rosterStudents.find((student) => student.studentId === selectedStudentId);
  }

  function setStatus(message: string) {
    element("studentTimelineStatus").textContent = message;
  }

  const heading = document.querySelector('.student-view-heading');
  element('studentViewTitle').textContent = '학생 기록';
  heading?.append(createStudentPrivacyToggle());
  const quickEntry = document.createElement('button');
  quickEntry.type = 'button'; quickEntry.id = 'studentTimelineQuickToggle'; quickEntry.className = 'desk-primary'; quickEntry.textContent = '빠른 관찰'; heading?.append(quickEntry);
  const counselEntry = document.createElement('button');
  counselEntry.type = 'button'; counselEntry.id = 'studentTimelineCounseling'; counselEntry.className = 'desk-outline'; counselEntry.textContent = '상담 작성'; heading?.append(counselEntry);
  const panelEntry = document.createElement('button');
  panelEntry.type = 'button'; panelEntry.id = 'studentTimelinePanelToggle'; panelEntry.className = 'desk-outline'; panelEntry.textContent = '상세 접기'; heading?.append(panelEntry);
  const kindTabs = document.createElement('div'); kindTabs.id = 'studentTimelineKindTabs'; kindTabs.className = 'student-kind-tabs'; kindTabs.setAttribute('role', 'group'); kindTabs.setAttribute('aria-label', '기록 유형');
  kindTabs.innerHTML = '<button type="button" data-kind="" aria-pressed="true">전체</button><button type="button" data-kind="observations" aria-pressed="false">관찰</button><button type="button" data-kind="teacher-counseling-sessions" aria-pressed="false">상담</button>';
  document.querySelector('.student-records-head')?.append(kindTabs);
  let recordKind = '';
  const rosterColumn = document.querySelector('.student-roster-column');
  const searchForm = element<HTMLFormElement>('studentTimelineSearchForm');
  rosterColumn?.prepend(searchForm);
  const queryMemory = new Map<string, string>();
  queryInput.placeholder = '학생 이름 또는 번호 검색';
  const rosterNote = document.createElement('p'); rosterNote.className = 'student-roster-notice'; rosterNote.setAttribute('role', 'status'); rosterColumn?.append(rosterNote);

  function renderStudents() {
    rosterNote.textContent = rosterMessage;
    counselEntry.disabled = isStudentPrivacyEnabled() || !selectedStudentId || isDeskRestoreBlocked();
    element("studentRosterCount").textContent = `학생 ${studentTotal.toLocaleString("ko-KR")}명`;
    const list = element("studentRosterList");
    if (!students.length) {
      list.innerHTML = `<div class="student-list-message"><i class="fa-solid fa-user-slash" aria-hidden="true"></i><span>조건에 맞는 학생이 없습니다.</span></div>`;
      return;
    }
    list.innerHTML = students.map((student, index) => `
      <button type="button" class="student-roster-row${student.studentId === selectedStudentId ? ' is-selected' : ''}" data-student-slot="${index}" aria-pressed="${student.studentId === selectedStudentId}">
        <span>${escapeHtml(student.classNo ? `${student.classNo}번` : isStudentPrivacyEnabled() ? String(index + 1).padStart(2, '0') : studentIdentifierLabel(student.studentId))}</span><strong>${escapeHtml(isStudentPrivacyEnabled() ? `학생 ${String(student.classNo || index + 1).padStart(2, '0')}` : studentLabel(student))}</strong>
      </button>
    `).join('');
  }

  function renderTimeline() {
    element<HTMLElement>('studentTimelinePagination').hidden = true;
    const student = selectedStudent();
    element("studentTimelineName").textContent = studentLabel(student);
    element("studentTimelineNumber").textContent = student ? student.classNo ? `${student.classNo}번` : isStudentPrivacyEnabled() ? "" : studentIdentifierLabel(student.studentId) : "";
    element("studentTimelineSummary").textContent = student
      ? `저장 자료 ${student.recordCount.toLocaleString("ko-KR")}건 · 최근 기록 ${records[0]?.dateKey ? records[0].dateKey.replace(/-/g, ". ") + "." : "-"}`
      : "학생을 선택하면 저장 기록이 표시됩니다.";
    const list = element("studentTimelineList");
    if (loading) {
      list.innerHTML = `<div class="student-list-message"><i class="fa-solid fa-spinner fa-spin" aria-hidden="true"></i><span>학생 기록을 불러오고 있습니다.</span></div>`;
      return;
    }
    if (!records.length) {
      list.innerHTML = `<div class="student-list-message"><i class="fa-solid fa-clock-rotate-left" aria-hidden="true"></i><strong>${student ? '선택한 조건의 기록이 없습니다' : '학생을 선택하세요'}</strong><span>${student ? '이 학생의 첫 관찰이나 상담을 작성할 수 있습니다.' : '왼쪽 학생을 선택하면 기록을 모아 볼 수 있습니다.'}</span>${student ? `<button type="button" data-empty-action="quick">새 관찰</button><button type="button" data-empty-action="counseling"${isStudentPrivacyEnabled() || isDeskRestoreBlocked() ? ' disabled' : ''}>상담 작성</button>` : ''}</div>`;
      return;
    }
    list.innerHTML = records.map((record, index) => `
      <button type="button" class="student-timeline-row is-${escapeHtml(record.groupKey)}${index === selectedRecordIndex ? " is-selected" : ""}" data-student-record-index="${index}" aria-pressed="${index === selectedRecordIndex}">
        <i class="fa-solid fa-circle student-timeline-dot" aria-hidden="true"></i>
        <span class="student-timeline-icon"><i class="fa-solid ${timelineIcon(record)}" aria-hidden="true"></i></span>
        <span class="student-timeline-copy"><small>${escapeHtml(timelineDate(record))}<b>·</b>${escapeHtml(timelineCategory(record))}</small><strong>${escapeHtml(isStudentPrivacyEnabled() ? "가림을 해제하면 원문을 볼 수 있습니다" : timelineTitle(record))}</strong></span>
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
    const attachments = isStudentPrivacyEnabled() ? [] : recordAttachments(record);
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
    const record = directRecord || records[selectedRecordIndex];
    empty.hidden = Boolean(record);
    detail.hidden = !record;
    updateTeacherRecordAction(detail, isStudentPrivacyEnabled() ? undefined : record, options.getTenantId().trim());
    if (!record) {
      for (const id of ['studentTimelineDetailTitle', 'studentTimelineDetailStudent', 'studentTimelineDetailKind', 'studentTimelineDetailDate', 'studentTimelineDetailSavedAt', 'studentTimelineDetailBody', 'studentTimelineAttachmentList', 'studentTimelineJson']) element(id).replaceChildren();
      return;
    }
    const student = selectedStudent();
    const name = studentLabel(student);
    element("studentTimelineDetailTitle").textContent = `${name} ${sectionLabel(record)}`;
    element("studentTimelineDetailStudent").textContent = name;
    element("studentTimelineDetailKind").textContent = timelineCategory(record);
    element("studentTimelineDetailDate").textContent = record.dateKey ? formatDate(0, record.dateKey) : "-";
    element("studentTimelineDetailSavedAt").textContent = record.updatedAtMs
      ? new Intl.DateTimeFormat('ko-KR', {dateStyle:'medium',timeStyle:'short',timeZone:'Asia/Seoul'}).format(new Date(record.updatedAtMs))
      : '저장 시각 미확인';
    const parts = isStudentPrivacyEnabled() ? ["학생 기록 가림이 켜져 있습니다. 가림을 해제하면 원문·첨부파일을 보고 입력할 수 있습니다."] : contentParts(record.payload, record.sectionKey);
    element("studentTimelineDetailBody").innerHTML = (parts.length ? parts : ["저장된 원문 필드가 없습니다. 원본 JSON에서 전체 내용을 확인할 수 있습니다."])
      .map((part) => `<p>${escapeHtml(part)}</p>`).join("");
    renderAttachments(record);
    element("studentTimelineJson").textContent = isStudentPrivacyEnabled() ? "" : JSON.stringify(record.payload, null, 2);
    element("studentTimelineJson").closest("details")!.hidden = isStudentPrivacyEnabled();
  }

  function previewStudentList(): StudentListResult {
    const query = queryInput.value.trim().toLocaleLowerCase("ko-KR");
    const filtered = PREVIEW_STUDENTS.filter((student) => !query || `${student.studentId} ${student.studentName}`.toLocaleLowerCase("ko-KR").includes(query));
    return { ok: true, total: query ? filtered.length : 24, students: filtered };
  }

  async function loadTimeline() {
    const generation = ++timelineGeneration;
    const tenantId = options.getTenantId().trim();
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
    const prior = teacherRecordLocator(tenantId, records[selectedRecordIndex]);
    renderTimeline();
    try {
      const range = studentRecordDateRange(periodSelect.value);
      const result = DESIGN_PREVIEW
        ? (() => {
            const filtered = (student.studentId === "3" ? PREVIEW_TIMELINE : [])
              .filter((record) => !groupSelect.value || record.groupKey === groupSelect.value)
              .filter((record) => !recordKind || record.sectionKey === recordKind)
              .filter((record) => !range.dateFrom || String(record.dateKey || "") >= range.dateFrom)
              .filter((record) => !range.dateTo || String(record.dateKey || "") <= range.dateTo);
            return { ok: true, total: filtered.length, records: filtered } as SearchResult;
          })()
        : await invoke<SearchResult>("search_local_data", {
            input: {
              tenantId, studentId: student.studentId, group: groupSelect.value,
              sectionKey: recordKind, studentQuery: "", textQuery: "", dateFrom: range.dateFrom, dateTo: range.dateTo,
              hasAttachment: false, offset: page * PAGE_SIZE, limit: PAGE_SIZE,
            },
          });
      if (generation !== timelineGeneration || tenantId !== options.getTenantId().trim()) return;
      if (result?.ok !== true) throw new Error(result.error || "local_student_timeline_failed");
      records = Array.isArray(result.records) ? result.records : [];
      recordTotal = numeric(result.total);
      selectedRecordIndex = prior ? records.findIndex(record => { const current = teacherRecordLocator(tenantId, record); return current?.kind === prior.kind && current.recordId === prior.recordId; }) : -1;
      if (selectedRecordIndex < 0) selectedRecordIndex = records.length ? 0 : -1;
      setStatus(DESIGN_PREVIEW
        ? "로컬 DB에서 불러옴 · 인터넷 연결 없이 열람 가능"
        : recordTotal
          ? `선택한 학생의 저장 자료 ${recordTotal.toLocaleString("ko-KR")}건을 불러왔습니다.`
          : "선택한 조건에 저장 자료가 없습니다.");
    } catch (error) {
      if (generation !== timelineGeneration) return;
      records = [];
      recordTotal = 0;
      selectedRecordIndex = -1;
      setStatus("학생 기록을 불러오지 못했습니다. 연결 상태를 확인하고 다시 조회하세요.");
    } finally {
      if (generation === timelineGeneration) {
        loading = false; renderTimeline(); renderDetail();
        element('studentTimelineList').scrollTop = scrollMemory.get(`${tenantId}:${selectedStudentId}`) || 0;
      }
    }
  }

  async function loadStudents() {
    const tenantId = options.getTenantId().trim();
    const generation = ++rosterGeneration;
    if (activeTenant && activeTenant !== tenantId) { selectedStudentId = ''; records = []; directRecord = undefined; timelineGeneration += 1; }
    activeTenant = tenantId;
    if (!tenantId && !DESIGN_PREVIEW) {
      students = []; rosterStudents = []; studentTotal = 0; selectedStudentId = '';
      setStatus('교사 홈에서 학급을 먼저 연결하세요.'); renderStudents(); await loadTimeline(); return;
    }
    try {
      const [result, context] = DESIGN_PREVIEW ? [previewStudentList(), {ok:true} as QuickContext] : await Promise.all([
        invoke<StudentListResult>('list_local_students', {input:{tenantId,query:'',offset:0,limit:200}}),
        invoke<QuickContext>('get_quick_observation_context').catch(() => ({ok:false} as QuickContext)),
      ]);
      if (generation !== rosterGeneration || tenantId !== options.getTenantId().trim()) return;
      if (result?.ok !== true) throw new Error(result.error || 'local_student_list_failed');
      const all = mergeRosterStudents(tenantId, result.students || [], context);
      rosterStudents = all;
      archivedStudentIds = new Set(context.tenantId === tenantId ? context.roster?.students.filter(student => student.status === 'archived').map(student => student.id) || [] : []);
      const query = queryInput.value.trim().toLocaleLowerCase('ko-KR');
      students = all.filter(student => !query || `${student.classNo || ''} ${isStudentPrivacyEnabled() ? '' : student.studentName}`.toLocaleLowerCase('ko-KR').includes(query));
      studentTotal = students.length;
      rosterMessage = context.tenantId === tenantId && context.roster ? context.roster.stale ? '보관 명단이 24시간을 지났습니다. 교사 홈에서 최신 명단을 확인하세요.' : '이 PC의 학급 명단 · 기록이 없는 학생도 표시합니다.' : '학급 명단을 확인하지 못했습니다. 저장 기록이 있는 학생만 표시합니다.';
      if (selectedStudentId && !all.some(student => student.studentId === selectedStudentId)) { selectedStudentId = ''; directRecord = undefined; page = 0; }
      // Searching a roster never changes the selected record target.
      renderStudents(); await loadTimeline();
    } catch {
      if (generation !== rosterGeneration) return;
      setStatus('학생 목록을 확인하지 못했습니다. 현재 조회 조건과 선택을 유지했습니다. 다시 시도하세요.'); renderStudents();
    }
  }
  async function openRecord(target: StudentRecordTarget) {
    if (!target?.recordId || !target.studentId || !['observation','counseling'].includes(target.kind)) return false;
    if (isDeskRestoreBlocked() || target.studentId !== selectedStudentId && options.beforeStudentChange && !await options.beforeStudentChange()) return false;
    const tenantId = options.getTenantId().trim();
    const generation = ++directGeneration;
    timelineGeneration += 1;
    directRecord = undefined; records = []; selectedRecordIndex = -1; renderDetail();
    try {
      const result = await invoke<{ok?:boolean;record?:Record<string,unknown>;revision?:string|number;error?:string}>('get_local_teacher_record', {tenantId,kind:target.kind,recordId:target.recordId});
      if (generation !== directGeneration || tenantId !== options.getTenantId().trim()) return false;
      if (result.ok !== true || !result.record || result.revision == null || !matchesStudentRecordTarget(tenantId, target, result.record)) throw new Error('record_target_mismatch');
      await loadStudents();
      if (generation !== directGeneration || tenantId !== options.getTenantId().trim()) return false;
      if (archivedStudentIds.has(target.studentId)) throw new Error('student_archived');
      selectedStudentId = target.studentId;
      if (!rosterStudents.some(student => student.studentId === target.studentId)) rosterStudents.push({studentId:target.studentId,studentName:String(result.record.studentName || ''),classNo:Number(result.record.classNo)||undefined,recordCount:1,lastUpdatedMs:Number(result.record.updatedAtMs)||0});
      if (!students.some(student => student.studentId === target.studentId)) students.push(rosterStudents.find(student => student.studentId === target.studentId)!);
      studentTotal = students.length;
      const dateMs = Number(result.record.counselingAtMs || result.record.updatedAtMs || result.record.createdAtMs || 0);
      directRecord = {sectionKey:target.kind === 'observation' ? 'observations' : 'teacher-counseling-sessions',sectionLabel:target.kind === 'observation' ? '관찰 기록' : '상담 기록',groupKey:'care',payload:result.record,updatedAtMs:Number(result.record.updatedAtMs)||0,dateKey:typeof result.record.date === 'string' ? result.record.date : dateMs ? new Intl.DateTimeFormat('en-CA',{year:'numeric',month:'2-digit',day:'2-digit',timeZone:'Asia/Seoul'}).format(new Date(dateMs)) : undefined,hasAttachment:recordAttachments({payload:result.record} as LocalDataRecord).length > 0};
      renderStudents(); await loadTimeline(); renderDetail();
      setStatus('선택한 기록의 정본을 새로 조회했습니다. 현재 기간·유형 필터와 관계없이 해당 원문을 표시합니다.');
      window.dispatchEvent(new CustomEvent('desk:student-selected',{detail:{studentId:selectedStudentId}})); return true;
    } catch {
      if (generation === directGeneration) { directRecord = undefined; renderDetail(); setStatus('해당 기록의 실제 학생과 정본을 확인하지 못했습니다. 다시 조회하세요.'); }
      return false;
    }
  }
  const selectStudent = async (studentId: string) => {
    if (!studentId || studentId === selectedStudentId || isDeskRestoreBlocked()) return;
    if (options.beforeStudentChange && !await options.beforeStudentChange()) return;
    scrollMemory.set(`${activeTenant}:${selectedStudentId}`, element('studentTimelineList').scrollTop);
    directGeneration += 1; directRecord = undefined; selectedStudentId = studentId; page = 0; selectedRecordIndex = -1;
    renderStudents(); await loadTimeline();
    window.dispatchEvent(new CustomEvent('desk:student-selected', {detail:{studentId}}));
  };
  async function refresh() {
    const locator = teacherRecordLocator(options.getTenantId().trim(), directRecord);
    if (locator && selectedStudentId) await openRecord({kind:locator.kind,recordId:locator.recordId,studentId:selectedStudentId});
    else await loadStudents();
  }
  const beginCounseling = () => { if (!selectedStudentId || isStudentPrivacyEnabled() || isDeskRestoreBlocked()) return; window.dispatchEvent(new CustomEvent('desk:create-counseling',{detail:{studentId:selectedStudentId}})); };
  const beginQuick = () => { if (isDeskRestoreBlocked()) return; window.dispatchEvent(new CustomEvent('desk:open-student-quick',{detail:{studentIds:selectedStudentId ? [selectedStudentId] : []}})); };
  quickEntry.addEventListener('click', beginQuick);
  counselEntry.addEventListener('click', beginCounseling);
  panelEntry.addEventListener('click', () => {
    const workbench = document.querySelector('.student-timeline-workbench'); const collapsed = workbench?.classList.toggle('is-detail-collapsed');
    document.querySelector<HTMLElement>('.student-detail-column')!.hidden = Boolean(collapsed); panelEntry.textContent = collapsed ? '상세 펼치기' : '상세 접기'; panelEntry.setAttribute('aria-expanded', String(!collapsed));
  });
  kindTabs.addEventListener('click', event => {
    const button = (event.target as HTMLElement)?.closest<HTMLButtonElement>('[data-kind]'); if (!button) return;
    recordKind = button.dataset.kind || ''; page = 0; directGeneration += 1; directRecord = undefined;
    kindTabs.querySelectorAll('button').forEach(node => node.setAttribute('aria-pressed', String(node === button))); void loadTimeline();
  });
  element<HTMLFormElement>("studentTimelineSearchForm").addEventListener("submit", (event) => {
    event.preventDefault();
    void loadStudents();
  });
  periodSelect.addEventListener("change", () => { page = 0; directRecord = undefined; directGeneration += 1; void loadTimeline(); });
  groupSelect.addEventListener("change", () => { page = 0; directRecord = undefined; directGeneration += 1; void loadTimeline(); });
  element('studentRosterList').addEventListener('click', event => {
    const row = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>('[data-student-slot]');
    if (row) void selectStudent(students[Number(row.dataset.studentSlot)]?.studentId || '');
  });
  element("studentTimelineList").addEventListener("click", (event) => {
    const row = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>("[data-student-record-index]");
    const action = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>('[data-empty-action]');
    if (action) { action.dataset.emptyAction === 'quick' ? beginQuick() : beginCounseling(); return; }
    if (!row) return;
    directRecord = undefined; directGeneration += 1;
    selectedRecordIndex = Number(row.dataset.studentRecordIndex || 0) || 0;
    renderTimeline();
    renderDetail();
  });
  element("studentTimelineAttachmentList").addEventListener("click", async (event) => {
    const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>("[data-open-student-media]");
    if (!button || !button.dataset.openStudentMedia || isStudentPrivacyEnabled() || isDeskRestoreBlocked()) return;
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
      setStatus("첨부파일을 열지 못했습니다. 파일 상태를 확인하고 다시 시도하세요.");
    } finally {
      if (button.isConnected && !isStudentPrivacyEnabled()) button.disabled = false;
    }
  });
  element("studentTimelinePrevious").addEventListener("click", () => { if (page > 0) { page -= 1; void loadTimeline(); } });
  element("studentTimelineNext").addEventListener("click", () => { if ((page + 1) * PAGE_SIZE < recordTotal) { page += 1; void loadTimeline(); } });

  if (DESIGN_PREVIEW) {
    element("homeTenantLabel").textContent = "수영초등학교 5학년 1반";
    element("homeConnectionText").textContent = "연결됨";
    element("homeBackupText").textContent = "어제 오후 5:58";
  }

  window.addEventListener('desk:student-privacy-changed', () => {
    if (isStudentPrivacyEnabled()) { queryMemory.set('query', queryInput.value); queryInput.value = ''; }
    else { queryInput.value = queryMemory.get('query') || ''; queryMemory.delete('query'); }
    queryInput.placeholder = isStudentPrivacyEnabled() ? '학생 번호 검색' : '학생 이름 또는 번호 검색';
    counselEntry.disabled = isStudentPrivacyEnabled() || !selectedStudentId || isDeskRestoreBlocked();
    renderStudents(); renderTimeline(); renderDetail();
  });
  element('studentTimelineList').addEventListener('scroll', () => scrollMemory.set(`${activeTenant}:${selectedStudentId}`, element('studentTimelineList').scrollTop));
  window.addEventListener('desk:records-changed', () => void refresh());
  window.addEventListener('desk:restore-lock-changed', renderStudents);
  window.addEventListener('desk:open-student-record', event => {
    void openRecord((event as CustomEvent<StudentRecordTarget>).detail);
  });
  queryInput.placeholder = isStudentPrivacyEnabled() ? '학생 번호 검색' : '학생 이름 또는 번호 검색';
  counselEntry.disabled = isStudentPrivacyEnabled();
  return { open: loadStudents, refresh, openRecord, getSelectedStudent: selectedStudent };

}
