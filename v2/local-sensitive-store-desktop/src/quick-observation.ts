import { invoke, isTauri } from "@tauri-apps/api/core";
import { createStudentPrivacyToggle, isStudentPrivacyEnabled } from "./desk-privacy";
import { isDeskRestoreBlocked } from "./desk-restore-lock";

const TUTORIAL_KEY = "localQuickObservationTutorial:v3";
const DESIGN_PREVIEW = new URLSearchParams(window.location.search).get("designPreview") === "quick-observation";

export type RosterStudent = { id: string; displayName: string; classNo?: number | null; status: string };
export type RosterSnapshot = { students: RosterStudent[]; syncedAtMs: number; stale: boolean };
export type QuickContext = {
  ok?: boolean;
  connected?: boolean;
  tenantId?: string;
  tenantName?: string;
  roster?: RosterSnapshot | null;
  recent?: Record<string, unknown>[];
  pendingReceiptCount?: number;
  error?: string;
};

export type QuickObservationInput = {
  tenantId: string; mutationId: string; studentIds: string[]; contextType: string; status: string; note: string;
  date: string; eventTimePrecision: string; eventTimeZone: string; eventAtMs: number | null;
  period: number; subject: string; recordDomain: string; creativeArea: string; tags: string[];
};
export type QuickObservationResult = { ok?: boolean; savedCount?: number; records?: Record<string, unknown>[]; error?: string };
const CONTEXT_LABELS: Record<string, string> = { lesson: '수업', recess: '쉬는 시간', counseling: '상담', daily_guidance: '생활지도', other: '기타' };
export function canonicalRecordJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalRecordJson).join(',')}]`;
  if (value && typeof value === 'object') return `{${Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([key, entry]) => `${JSON.stringify(key)}:${canonicalRecordJson(entry)}`).join(',')}}`;
  return JSON.stringify(value);
}
/** A successful invocation alone is insufficient: each selected actual student must have a matching canonical record. */
export function verifyQuickObservationRecords(input: QuickObservationInput, result: QuickObservationResult): boolean {
  if (result.ok !== true || result.savedCount !== input.studentIds.length || !Array.isArray(result.records) || result.records.length !== input.studentIds.length) return false;
  const ids = new Set<string>();
  const docs = new Set<string>();
  for (const record of result.records) {
    if (typeof record.studentCode !== 'string' || !input.studentIds.includes(record.studentCode) || ids.has(record.studentCode)) return false;
    if (typeof record.docId !== 'string' || !record.docId || docs.has(record.docId) || record.tenantId !== input.tenantId || record.batchId !== input.mutationId) return false;
    ids.add(record.studentCode); docs.add(record.docId);
    for (const key of ['note', 'status', 'date', 'period', 'subject', 'recordDomain', 'creativeArea', 'tags', 'eventTimePrecision', 'eventTimeZone', 'eventAtMs'] as const) {
      if (canonicalRecordJson(record[key]) !== canonicalRecordJson(input[key])) return false;
    }
    if (input.contextType !== 'lesson' && record.contextType !== input.contextType) return false;
  }
  return ids.size === input.studentIds.length;
}

function required<T extends HTMLElement>(id: string) {
  const element = document.getElementById(id);
  if (!element) throw new Error(`missing quick observation element: ${id}`);
  return element as T;
}

function todayKst() {
  return new Intl.DateTimeFormat("en-CA", { year: "numeric", month: "2-digit", day: "2-digit", timeZone: "Asia/Seoul" }).format(new Date());
}

function formatDateTime(value: number) {
  if (!value) return "-";
  return new Intl.DateTimeFormat("ko-KR", { dateStyle: "medium", timeStyle: "short", timeZone: "Asia/Seoul" }).format(new Date(value));
}

function fixtureContext(): QuickContext {
  const names = ["김도윤", "김서연", "박시우", "이서진", "정민준", "최유나", "한지호", "강예린", "윤태민", "이다현", "조하준", "김민서", "오지후", "신가은", "배주원", "황민채", "권우진", "양수빈", "임지후", "전하윤", "문시온", "최예준", "남지민", "유서윤"];
  return {
    ok: true,
    connected: true,
    tenantId: "tenant-preview",
    tenantName: "수영초등학교 5학년 1반",
    roster: { students: names.map((displayName, index) => ({ id: `S${String(index + 1).padStart(2, "0")}`, displayName, classNo: index + 1, status: "active" })), syncedAtMs: Date.now() - 13 * 60_000, stale: false },
    recent: [
      { studentName: "김도윤", contextLabel: "수업", note: "친구의 발표를 끝까지 듣고 핵심을 정리함", updatedAtMs: Date.now() - 18 * 60_000 },
      { studentName: "박시우", contextLabel: "쉬는 시간", note: "놀이 규칙을 친구와 조율함", updatedAtMs: Date.now() - 64 * 60_000 },
    ],
  };
}

export function initQuickObservation(options: { onConnect?: () => void } = {}) {
  const form = required<HTMLFormElement>("quickObservationForm");
  const roster = required<HTMLElement>("quickObservationRoster");
  const search = required<HTMLInputElement>("quickObservationSearch");
  const note = required<HTMLTextAreaElement>("quickObservationNote");
  const save = required<HTMLButtonElement>("quickObservationSave");
  const contextGroup = required<HTMLElement>("quickObservationContext");
  const statusGroup = required<HTMLElement>("quickObservationStatus");
  const details = required<HTMLDetailsElement>("quickObservationDetails");
  const date = required<HTMLInputElement>("quickObservationDate");
  const precision = required<HTMLSelectElement>("quickObservationTimePrecision");
  const eventTime = required<HTMLInputElement>("quickObservationTime");
  const period = required<HTMLInputElement>("quickObservationPeriod");
  const subject = required<HTMLInputElement>("quickObservationSubject");
  const domain = required<HTMLSelectElement>("quickObservationDomain");
  const creative = required<HTMLInputElement>("quickObservationCreative");
  const tags = required<HTMLInputElement>("quickObservationTags");
  const tutorial = required<HTMLElement>("quickObservationTutorial");
  let state: QuickContext = { ok: true, connected: false, roster: null, recent: [] };
  let selected = new Set<string>();
  let contextType = "";
  let status = "none";
  let busy = false;
  let saving = false;
  let composing = false;
  let dirty = false;
  let loadGeneration = 0;
  const privateValues = new Map<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement, string>();
  const privacyToggle = createStudentPrivacyToggle();
  document.querySelector('.quick-observation-heading > div:last-child')?.prepend(privacyToggle);
  const sensitiveFields = [search, note, date, precision, eventTime, period, subject, domain, creative, tags];
  const valueOf = (field: typeof sensitiveFields[number]) => privateValues.get(field) ?? field.value;
  const setValue = (field: typeof sensitiveFields[number], value: string) => {
    if (isStudentPrivacyEnabled()) { privateValues.set(field, value); field.value = ''; }
    else field.value = value;
  };
  const blocked = () => busy || isStudentPrivacyEnabled() || isDeskRestoreBlocked();
  function markDirty() { dirty = true; mutationId = crypto.randomUUID(); }
  function applyPrivacy() {
    for (const field of sensitiveFields) {
      if (isStudentPrivacyEnabled()) { if (!privateValues.has(field)) privateValues.set(field, field.value); field.value = ''; }
      else if (privateValues.has(field)) { field.value = privateValues.get(field)!; privateValues.delete(field); }
    }
    render();
  }
  function lockInputs() {
    const locked = blocked();
    form.setAttribute('aria-busy', String(busy));
    roster.setAttribute('aria-busy', String(busy));
    form.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement | HTMLButtonElement>('input, textarea, select, button').forEach(control => { control.disabled = locked; });
    for (const id of ['quickObservationSelectAll', 'quickObservationClear', 'quickObservationRefresh']) required<HTMLButtonElement>(id).disabled = locked;
    search.disabled = locked;
    roster.querySelectorAll<HTMLInputElement>('input').forEach(input => { input.disabled = locked; });
    eventTime.disabled = locked || valueOf(precision) === 'unknown';
    save.disabled = locked || !state.roster || !selected.size || !contextType || !valueOf(note).trim() || [...selected].some(id => !activeStudents().some(student => student.id === id));
  }
  let tutorialIndex = 0;
  let mutationId = crypto.randomUUID();

  setValue(date, todayKst());

  function setFormStatus(text: string, tone = "") {
    const element = required<HTMLElement>("quickObservationFormStatus");
    element.textContent = text;
    element.dataset.tone = tone;
  }

  function activeStudents() {
    return (state.roster?.students || []).filter((student) => student.status !== "archived");
  }

  function renderSelection() {
    required("quickObservationSelected").textContent = `선택 ${selected.size}명`;
    save.innerHTML = `<i class="fa-solid fa-floppy-disk" aria-hidden="true"></i> ${selected.size ? `선택한 학생 ${selected.size}명 기록 저장` : "선택한 학생 기록 저장"}`;
    lockInputs();
    required('quickObservationNoteCount').textContent = `${isStudentPrivacyEnabled() ? 0 : valueOf(note).length} / 1000`;
  }

  function renderRoster() {
    const query = (isStudentPrivacyEnabled() ? "" : search.value).trim().toLocaleLowerCase("ko");
    roster.replaceChildren();
    const rows = activeStudents().filter((student) => !query || `${student.classNo || ""} ${student.displayName}`.toLocaleLowerCase("ko").includes(query));
    if (!rows.length) {
      const empty = document.createElement("p");
      empty.className = "quick-roster-empty";
      empty.textContent = state.roster ? "검색 조건에 맞는 학생이 없습니다." : "연결된 학급 명단이 없습니다.";
      roster.append(empty);
      renderSelection();
      return;
    }
    for (const student of rows) {
      const label = document.createElement("label");
      label.className = `quick-student-tile${selected.has(student.id) ? " is-selected" : ""}`;
      const input = document.createElement("input");
      input.type = "checkbox";
      input.checked = selected.has(student.id);
      input.disabled = blocked();
      input.addEventListener("change", () => {
        if (blocked()) return;
        markDirty();
        input.checked ? selected.add(student.id) : selected.delete(student.id);
        renderRoster();
      });
      const number = document.createElement("small");
      number.textContent = student.classNo ? String(student.classNo) : "-";
      const avatar = document.createElement("i");
      avatar.className = "fa-solid fa-user";
      avatar.setAttribute("aria-hidden", "true");
      const name = document.createElement("strong");
      name.textContent = isStudentPrivacyEnabled() ? `학생 ${student.classNo ? String(student.classNo).padStart(2, "0") : activeStudents().indexOf(student) + 1}` : student.displayName;
      const check = document.createElement("span");
      check.className = "quick-student-check";
      check.innerHTML = '<i class="fa-solid fa-check" aria-hidden="true"></i>';
      label.append(input, avatar, number, name, check);
      roster.append(label);
    }
    renderSelection();
  }

  function renderRecent() {
    const container = required("quickObservationRecent");
    container.replaceChildren();
    const records = Array.isArray(state.recent) ? state.recent : [];
    if (!records.length) {
      const empty = document.createElement("p");
      empty.className = "quick-recent-empty";
      empty.textContent = "아직 빠른 관찰기록이 없습니다.";
      container.append(empty);
      return;
    }
    for (const record of records.slice(0, 4)) {
      const item = document.createElement("article");
      const heading = document.createElement("strong");
      heading.textContent = isStudentPrivacyEnabled() ? "학생 관찰 기록" : `${String(record.studentName || record.studentCode || "학생")} · ${String(record.contextLabel || record.subject || "관찰")}`;
      const summary = document.createElement("span");
      summary.textContent = isStudentPrivacyEnabled() ? "가림을 해제하면 원문을 확인할 수 있습니다." : String(record.note || "");
      const time = document.createElement("time");
      time.textContent = `발생 ${record.date || "날짜 미확인"} ${record.eventTimePrecision === "unknown" || !record.eventAtMs ? "시각 모름" : `${record.eventTimePrecision === "approximate" ? "대략 " : ""}${formatDateTime(Number(record.eventAtMs))}`} · 저장 ${formatDateTime(Number(record.createdAtMs || record.updatedAtMs || 0))}`;
      item.append(heading, summary, time);
      container.append(item);
    }
  }

  function renderAlert() {
    const alert = required("quickObservationAlert");
    alert.replaceChildren();
    if (!state.connected || !state.roster) {
      alert.hidden = false;
      alert.dataset.tone = "warning";
      const copy = document.createElement("span");
      copy.textContent = state.connected
        ? "이 PC에 저장된 학생 명단이 없습니다. 교사 홈에서 관찰기록을 한 번 열어 명단을 연결해 주세요."
        : "먼저 교사 홈에서 이 PC 저장소를 연결해 주세요.";
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = "교사 홈에서 연결";
      button.addEventListener("click", () => options.onConnect?.());
      alert.append(copy, button);
      return;
    }
    if (state.roster.stale) {
      alert.hidden = false;
      alert.dataset.tone = "warning";
      alert.textContent = "명단을 갱신한 지 24시간이 지났습니다. 기록은 가능하지만 교사 홈에서 최신 명단을 확인해 주세요.";
      return;
    }
    alert.hidden = true;
  }

  function renderContext() {
    contextGroup.querySelectorAll<HTMLButtonElement>("button[data-value]").forEach((button) => button.setAttribute("aria-pressed", String(!isStudentPrivacyEnabled() && button.dataset.value === contextType)));
    statusGroup.querySelectorAll<HTMLButtonElement>("button[data-value]").forEach((button) => button.setAttribute("aria-pressed", String(!isStudentPrivacyEnabled() && button.dataset.value === status)));
    document.querySelectorAll<HTMLElement>(".quick-lesson-field").forEach((field) => { field.hidden = contextType !== "lesson"; });
    required("quickObservationTimeFields").hidden = contextType === "lesson";
    eventTime.disabled = blocked() || valueOf(precision) === "unknown";
    if (contextType === "lesson" && !details.open) details.open = true;
    required("quickObservationCreativeField").hidden = valueOf(domain) !== "creative";
    renderSelection();
  }

  function render() {
    required("quickObservationTenant").textContent = state.tenantName || state.tenantId || "연결된 학급 없음";
    required("quickObservationRosterTime").textContent = state.roster ? `명단 갱신 ${formatDateTime(state.roster.syncedAtMs)}${state.pendingReceiptCount ? ` · 서버 접수 대기 ${state.pendingReceiptCount}건` : ""}` : "명단 연결 필요";
    renderAlert();
    renderRoster();
    renderRecent();
    renderContext();
  }

  async function fetchContext(): Promise<QuickContext> {
    const next = DESIGN_PREVIEW ? fixtureContext() : isTauri() ? await invoke<QuickContext>('get_quick_observation_context') : { ok: false, error: 'tauri_required' };
    if (next?.ok !== true) throw new Error(next.error || 'quick_observation_load_failed');
    return next;
  }
  async function load() {
    if (saving || composing) return;
    const generation = ++loadGeneration;
    busy = true; renderSelection();
    try {
      const next = await fetchContext();
      if (generation !== loadGeneration) return;
      if (dirty && state.tenantId && next.tenantId !== state.tenantId) throw new Error('quick_tenant_changed');
      state = next;
      const unavailable = [...selected].some(id => !activeStudents().some(student => student.id === id));
      setFormStatus(unavailable ? '선택한 학생이 최신 명단에 없습니다. 입력을 유지했으니 명단과 대상을 확인하세요.' : state.roster ? '학생·상황·메모를 선택하고 필수 입력을 확인하세요. 저장 전 입력은 현재 창에만 있습니다.' : '최신 학생 명단을 연결해야 기록할 수 있습니다.', unavailable || !state.roster ? 'warning' : '');
    } catch {
      setFormStatus('로컬 명단을 확인하지 못했습니다. 현재 선택과 입력은 유지했습니다. 연결 상태를 확인하고 다시 시도하세요.', 'bad');
    } finally {
      if (generation === loadGeneration) { busy = false; render(); }
    }
  }

  const tutorialSteps = [
    { target: "roster", title: "학생 선택", body: "학생 타일을 눌러 한 명 또는 여러 명을 선택합니다. 선택한 학생마다 독립 기록이 만들어집니다." },
    { target: "context", title: "상황과 상태", body: "관찰한 상황과 상태를 선택합니다. 수업을 고르면 날짜·교시·과목을 확인한 뒤 저장합니다." },
    { target: "occurrence", title: "실제 발생 일시", body: "실제 있었던 날짜를 확인합니다. 일상 관찰은 정확한 시각·대략적인 시각·시각 모름 중 선택하며, 지금 버튼은 현재 한국 시각을 입력합니다. 작성 시각은 별도로 자동 기록됩니다." },
    { target: "memo", title: "관찰 메모", body: "관찰한 행동을 짧게 적습니다. 학생 기록 가림을 해제하면 입력할 수 있습니다. 가림을 켜도 입력은 현재 창 메모리에 유지됩니다." },
    { target: "save", title: "원본과 저장 이력", body: "저장 중에는 학생 선택과 입력을 잠급니다. 학생별 정본을 다시 읽어 확인한 뒤 입력을 비웁니다. 저장할 때 원본과 검증값을 함께 보존합니다. 이후 정정은 사유와 새 버전으로 남습니다. 서버 접수는 온라인 연결 시 자동 요청되며, 외부 시점확인은 아직 활성화되지 않았습니다. 이 안내는 저장을 실행하지 않습니다." },
  ];

  function renderTutorial() {
    const step = tutorialSteps[tutorialIndex];
    const selectors: Record<string, string> = { roster: ".quick-student-tile", context: "#quickObservationContext", occurrence: "#quickObservationDate", memo: "#quickObservationNote", save: "#quickObservationSave" };
    document.querySelectorAll<HTMLElement>(".quick-tutorial-target").forEach((element) => element.classList.remove("quick-tutorial-target"));
    const field = document.querySelector<HTMLElement>(selectors[step.target]);
    // Highlight the date label and input, so the explanatory note does not consume the compact viewport.
    const target = step.target === "occurrence" ? field?.closest<HTMLElement>("label") : field;
    target?.classList.add("quick-tutorial-target");
    required("quickObservationTutorialStep").textContent = `${tutorialIndex + 1} / ${tutorialSteps.length}`;
    required("quickObservationTutorialTitle").textContent = step.title;
    required("quickObservationTutorialBody").textContent = step.body;
    required<HTMLButtonElement>("quickObservationTutorialPrevious").disabled = tutorialIndex === 0;
    required<HTMLButtonElement>("quickObservationTutorialNext").textContent = tutorialIndex === tutorialSteps.length - 1 ? "안내 완료" : "다음";
    if (!target) return;
    tutorial.style.width = window.innerWidth <= 760 ? "calc(100vw - 32px)" : "360px";
    tutorial.style.left = "auto";
    tutorial.style.right = "16px";
    tutorial.style.top = "auto";
    tutorial.style.bottom = "16px";
    target.scrollIntoView({ block: "center", behavior: "instant" });
    let rect = target.getBoundingClientRect();
    const panel = tutorial.getBoundingClientRect();
    if (rect.left >= panel.width + 32) {
      tutorial.style.left = "16px";
      tutorial.style.right = "auto";
    } else if (rect.right + panel.width + 32 > window.innerWidth) {
      const toolbarBottom = document.querySelector<HTMLElement>(".workspace-topbar")?.getBoundingClientRect().bottom || 0;
      const topInset = Math.max(56, toolbarBottom + 12);
      let scroller = target.parentElement;
      while (scroller && !(scroller.scrollHeight > scroller.clientHeight && /auto|scroll/.test(getComputedStyle(scroller).overflowY))) scroller = scroller.parentElement;
      if (scroller) scroller.scrollTo({ top: scroller.scrollTop + rect.top - topInset, behavior: "instant" });
      else window.scrollBy({ top: rect.top - topInset, behavior: "instant" });
      rect = target.getBoundingClientRect();
      if (rect.bottom + panel.height + 28 > window.innerHeight && rect.top >= panel.height + topInset + 12) {
        tutorial.style.top = `${topInset}px`;
        tutorial.style.bottom = "auto";
      }
    }
  }

  function openTutorial() {
    tutorialIndex = 0;
    tutorial.hidden = false;
    renderTutorial();
  }

  function closeTutorial() {
    tutorial.hidden = true;
    document.querySelectorAll<HTMLElement>(".quick-tutorial-target").forEach((element) => element.classList.remove("quick-tutorial-target"));
    try { localStorage.setItem(TUTORIAL_KEY, "complete"); } catch { /* Tutorial preference is optional. */ }
  }

  async function open({ focus = false, studentIds }: { focus?: boolean; studentIds?: string[] } = {}) {
    if (saving || composing) return;
    await load();
    if (studentIds) {
      const validIds = new Set(activeStudents().map(student => student.id));
      if (studentIds.some(id => !validIds.has(id))) { setFormStatus('현재 학급 명단에서 선택한 학생을 확인하지 못했습니다. 대상 선택을 확인하세요.', 'bad'); return; }
      const next = new Set(studentIds);
      if (canonicalRecordJson([...next].sort()) !== canonicalRecordJson([...selected].sort())) {
        if (dirty && !await canLeave()) return;
        selected = next; mutationId = crypto.randomUUID(); renderRoster();
      }
    }
    if (focus && !search.disabled) search.focus();
    try { if (!DESIGN_PREVIEW && localStorage.getItem(TUTORIAL_KEY) !== 'complete' && state.roster) openTutorial(); } catch { /* Optional preference */ }
  }
  async function canLeave() {
    if (busy || composing || isDeskRestoreBlocked()) { setFormStatus(composing ? '한글 입력을 마친 뒤 이동하세요.' : '저장·복원과 재조회가 끝난 뒤 이동하세요. 현재 입력을 보호하고 있습니다.', 'warning'); return false; }
    if (dirty && !window.confirm('현재 창에 보관 중인 관찰 입력이 있습니다. 입력을 버리고 이동할까요?')) return false;
    if (dirty) { selected.clear(); for (const field of [note, subject, creative, tags, period]) setValue(field, ''); dirty = false; mutationId = crypto.randomUUID(); render(); }
    return true;
  }
  form.addEventListener('compositionstart', () => { composing = true; });
  form.addEventListener('compositionend', () => { composing = false; });
  search.addEventListener('compositionstart', () => { composing = true; });
  search.addEventListener('compositionend', () => { composing = false; });
  search.addEventListener('input', renderRoster);
  form.addEventListener('input', () => { if (!blocked()) markDirty(); });
  form.addEventListener('change', () => { if (!blocked()) markDirty(); });
  note.addEventListener('input', renderSelection);
  contextGroup.addEventListener('click', (event) => {
    const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>('button[data-value]');
    if (!button || blocked()) return;
    contextType = button.dataset.value || ''; markDirty();
    setValue(domain, contextType === 'lesson' ? 'subjects' : 'behavior'); renderContext();
  });
  statusGroup.addEventListener('click', (event) => {
    const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>('button[data-value]');
    if (!button || blocked()) return;
    status = button.dataset.value || 'none'; markDirty(); renderContext();
  });
  domain.addEventListener('change', renderContext);
  precision.addEventListener('change', () => { if (blocked()) return; if (precision.value === 'unknown') eventTime.value = ''; renderContext(); });
  required('quickObservationNow').addEventListener('click', () => {
    if (blocked()) return;
    date.value = todayKst();
    eventTime.value = new Intl.DateTimeFormat('en-GB', { hour: '2-digit', minute: '2-digit', timeZone: 'Asia/Seoul', hourCycle: 'h23' }).format(new Date());
    precision.value = 'exact'; markDirty(); renderContext();
  });
  required('quickObservationSelectAll').addEventListener('click', () => { if (blocked()) return; selected = new Set(activeStudents().map(student => student.id)); markDirty(); renderRoster(); });
  required('quickObservationClear').addEventListener('click', () => { if (blocked()) return; selected.clear(); markDirty(); renderRoster(); });
  required('quickObservationRefresh').addEventListener('click', () => void load());
  required("quickObservationHelp").addEventListener("click", openTutorial);
  required("quickObservationTutorialClose").addEventListener("click", closeTutorial);
  required("quickObservationTutorialPrevious").addEventListener("click", () => { tutorialIndex = Math.max(0, tutorialIndex - 1); renderTutorial(); });
  required("quickObservationTutorialNext").addEventListener("click", () => {
    if (tutorialIndex < tutorialSteps.length - 1) { tutorialIndex += 1; renderTutorial(); } else closeTutorial();
  });
  form.addEventListener('submit', async event => {
    event.preventDefault();
    if (blocked() || composing) return;
    if (!selected.size) return setFormStatus('기록할 학생을 한 명 이상 선택하세요.', 'bad');
    if (!contextType) return setFormStatus('관찰 상황을 선택하세요.', 'bad');
    if (!note.value.trim()) return setFormStatus('관찰 메모를 입력하세요.', 'bad');
    if (!date.value) return setFormStatus('실제 발생 날짜를 입력하세요.', 'bad');
    if (contextType !== 'lesson' && precision.value !== 'unknown' && !eventTime.value) return setFormStatus('발생 시각을 입력하거나 시각 모름을 선택하세요.', 'bad');
    if (contextType === 'lesson' && (!Number.isInteger(Number(period.value)) || Number(period.value) < 1 || Number(period.value) > 20 || !subject.value.trim())) { details.open = true; return setFormStatus('수업 관찰은 교시와 과목을 입력하세요.', 'bad'); }
    if (domain.value === 'creative' && !creative.value.trim()) { details.open = true; return setFormStatus('창체 영역을 입력하세요.', 'bad'); }
    if ([...selected].some(id => !activeStudents().some(student => student.id === id))) return setFormStatus('현재 학급 명단에서 선택한 학생을 다시 확인하세요.', 'bad');
    const tagValues = tags.value.split(',').map(value => value.trim()).filter(Boolean);
    if (note.value.length > 1000 || tagValues.length > 20 || tagValues.some(value => value.length > 60)) return setFormStatus('관찰 메모는 1000자, 태그는 20개까지 각 60자 이내로 입력하세요.', 'bad');
    const input: QuickObservationInput = {
      tenantId: state.tenantId || '', mutationId, studentIds: [...selected], contextType, status,
      note: note.value.trim(), date: date.value, eventTimePrecision: contextType === 'lesson' ? 'unknown' : precision.value,
      eventTimeZone: 'Asia/Seoul', eventAtMs: contextType === 'lesson' || precision.value === 'unknown' ? null : new Date(`${date.value}T${eventTime.value}:00+09:00`).getTime(),
      period: contextType === 'lesson' ? Number(period.value) : 0, subject: contextType === 'lesson' ? subject.value.trim() : CONTEXT_LABELS[contextType], recordDomain: domain.value, creativeArea: creative.value.trim(), tags: tagValues,
    };
    if (!input.tenantId || (input.eventAtMs !== null && !Number.isFinite(input.eventAtMs))) return setFormStatus('학급 연결과 실제 발생 일시를 확인하세요.', 'bad');
    busy = true; saving = true; loadGeneration += 1; renderSelection();
    setFormStatus(`${input.studentIds.length}명 저장 · 정본 재조회 중입니다. 학생 선택과 입력을 잠시 잠갔습니다.`);
    try {
      if (DESIGN_PREVIEW) throw new Error('design_preview_read_only');
      const result = await invoke<QuickObservationResult>('save_quick_observation_batch', { input });
      if (!verifyQuickObservationRecords(input, result)) throw new Error(result.error || 'quick_observation_save_unverified');
      for (const record of result.records!) {
        const readback = await invoke<{ok?:boolean;record?:Record<string, unknown>}>('get_local_teacher_record', {tenantId: input.tenantId, kind: 'observation', recordId: record.docId});
        if (readback.ok !== true || canonicalRecordJson(readback.record) !== canonicalRecordJson(record)) throw new Error('quick_observation_readback_conflict');
      }
      const refreshed = await fetchContext();
      if (refreshed.tenantId !== input.tenantId) throw new Error('quick_tenant_changed');
      state = refreshed;
      selected.clear(); setValue(note, ''); dirty = false; mutationId = crypto.randomUUID();
      render(); setFormStatus(`${result.savedCount}명 각각의 관찰 정본을 다시 조회해 확인했습니다. 이 PC에 저장했습니다.`, 'good');
      window.dispatchEvent(new CustomEvent('desk:records-changed'));
    } catch (error) {
      const code = String((error as Error)?.message || error || '');
      setFormStatus(code === 'design_preview_read_only' ? '시안 모드에서는 실제 저장하지 않습니다.' : '저장 여부를 확인하지 못했습니다. 선택과 입력을 현재 창에 유지했습니다. 연결 상태를 확인하고 같은 입력으로 다시 저장하세요.', code === 'design_preview_read_only' ? 'warning' : 'bad');
      dirty = true;
    } finally { saving = false; busy = false; renderSelection(); }
  });
  window.addEventListener('keydown', event => {
    if ((event.ctrlKey || event.metaKey) && event.key === 'Enter' && !event.repeat && !event.isComposing && !composing && !save.disabled && tutorial.hidden && form.getClientRects().length) { event.preventDefault(); form.requestSubmit(); }
  });
  window.addEventListener('beforeunload', event => { if (dirty || busy || composing) { event.preventDefault(); event.returnValue = ''; } });
  window.addEventListener('desk:student-privacy-changed', applyPrivacy);
  window.addEventListener('desk:restore-lock-changed', renderSelection);
  window.addEventListener("resize", () => { if (!tutorial.hidden) renderTutorial(); });

  applyPrivacy();
  return { open, canLeave, getSelectedStudentIds: () => [...selected], getBusy: () => busy || composing };
}
