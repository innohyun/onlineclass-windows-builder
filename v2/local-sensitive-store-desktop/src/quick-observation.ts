import { invoke, isTauri } from "@tauri-apps/api/core";

const TUTORIAL_KEY = "localQuickObservationTutorial:v1";
const DESIGN_PREVIEW = new URLSearchParams(window.location.search).get("designPreview") === "quick-observation";

type RosterStudent = { id: string; displayName: string; classNo?: number | null; status: string };
type RosterSnapshot = { students: RosterStudent[]; syncedAtMs: number; stale: boolean };
type QuickContext = {
  ok?: boolean;
  connected?: boolean;
  tenantId?: string;
  tenantName?: string;
  roster?: RosterSnapshot | null;
  recent?: Record<string, unknown>[];
  error?: string;
};

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
  return new Intl.DateTimeFormat("ko-KR", { dateStyle: "medium", timeStyle: "short" }).format(new Date(value));
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
  let tutorialIndex = 0;

  date.value = todayKst();

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
    save.disabled = busy || !state.roster || !selected.size || !contextType || !note.value.trim();
  }

  function renderRoster() {
    const query = search.value.trim().toLocaleLowerCase("ko");
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
      input.addEventListener("change", () => {
        input.checked ? selected.add(student.id) : selected.delete(student.id);
        renderRoster();
      });
      const number = document.createElement("small");
      number.textContent = student.classNo ? String(student.classNo) : "-";
      const avatar = document.createElement("i");
      avatar.className = "fa-solid fa-user";
      avatar.setAttribute("aria-hidden", "true");
      const name = document.createElement("strong");
      name.textContent = student.displayName;
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
      heading.textContent = `${String(record.studentName || record.studentCode || "학생")} · ${String(record.contextLabel || record.subject || "관찰")}`;
      const summary = document.createElement("span");
      summary.textContent = String(record.note || "");
      const time = document.createElement("time");
      time.textContent = formatDateTime(Number(record.updatedAtMs || 0));
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
    contextGroup.querySelectorAll<HTMLButtonElement>("button[data-value]").forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.value === contextType)));
    statusGroup.querySelectorAll<HTMLButtonElement>("button[data-value]").forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.value === status)));
    document.querySelectorAll<HTMLElement>(".quick-lesson-field").forEach((field) => { field.hidden = contextType !== "lesson"; });
    if (contextType === "lesson" && !details.open) details.open = true;
    required("quickObservationCreativeField").hidden = domain.value !== "creative";
    renderSelection();
  }

  function render() {
    required("quickObservationTenant").textContent = state.tenantName || state.tenantId || "연결된 학급 없음";
    required("quickObservationRosterTime").textContent = state.roster ? `명단 갱신 ${formatDateTime(state.roster.syncedAtMs)}` : "명단 연결 필요";
    renderAlert();
    renderRoster();
    renderRecent();
    renderContext();
  }

  async function load() {
    busy = true;
    renderSelection();
    try {
      const next = DESIGN_PREVIEW ? fixtureContext() : isTauri() ? await invoke<QuickContext>("get_quick_observation_context") : { ok: false, error: "tauri_required" };
      if (next?.ok === false) throw new Error(next.error || "quick_observation_load_failed");
      state = next;
      const validIds = new Set(activeStudents().map((student) => student.id));
      selected = new Set([...selected].filter((id) => validIds.has(id)));
      setFormStatus(state.roster ? "학생·상황·메모를 선택하면 바로 저장할 수 있습니다." : "최신 학생 명단을 연결해야 기록할 수 있습니다.", state.roster ? "" : "warning");
    } catch {
      state = { ok: false, connected: false, roster: null, recent: [] };
      setFormStatus("로컬 관찰기록을 불러오지 못했습니다. 앱을 다시 열어 주세요.", "bad");
    } finally {
      busy = false;
      render();
    }
  }

  const tutorialSteps = [
    { target: "roster", title: "학생 선택", body: "학생 타일을 눌러 한 명 또는 여러 명을 선택합니다. 선택한 학생마다 독립 기록이 만들어집니다." },
    { target: "context", title: "상황과 상태", body: "관찰한 상황과 상태를 선택합니다. 수업을 고르면 날짜·교시·과목을 확인한 뒤 저장합니다." },
    { target: "memo", title: "관찰 메모", body: "관찰한 행동을 짧게 적습니다. 입력 내용은 저장 전까지 이 화면의 메모리에만 있습니다." },
    { target: "save", title: "로컬 DB 저장", body: "저장 버튼은 기존 관찰기록 DB에 학생별 기록을 저장하고 다시 읽어 확인합니다. 이 안내는 저장을 대신 실행하지 않습니다." },
  ];

  function renderTutorial() {
    const step = tutorialSteps[tutorialIndex];
    document.querySelectorAll<HTMLElement>("[data-quick-observation-tutorial]").forEach((element) => element.classList.toggle("quick-tutorial-target", element.dataset.quickObservationTutorial === step.target));
    required("quickObservationTutorialStep").textContent = `${tutorialIndex + 1} / ${tutorialSteps.length}`;
    required("quickObservationTutorialTitle").textContent = step.title;
    required("quickObservationTutorialBody").textContent = step.body;
    required<HTMLButtonElement>("quickObservationTutorialPrevious").disabled = tutorialIndex === 0;
    required<HTMLButtonElement>("quickObservationTutorialNext").textContent = tutorialIndex === tutorialSteps.length - 1 ? "안내 완료" : "다음";
  }

  function openTutorial() {
    tutorialIndex = 0;
    tutorial.hidden = false;
    renderTutorial();
  }

  function closeTutorial() {
    tutorial.hidden = true;
    document.querySelectorAll<HTMLElement>("[data-quick-observation-tutorial]").forEach((element) => element.classList.remove("quick-tutorial-target"));
    localStorage.setItem(TUTORIAL_KEY, "complete");
  }

  async function open({ focus = false } = {}) {
    await load();
    if (focus) search.focus();
    if (!DESIGN_PREVIEW && localStorage.getItem(TUTORIAL_KEY) !== "complete" && state.roster) openTutorial();
  }

  search.addEventListener("input", renderRoster);
  note.addEventListener("input", () => {
    required("quickObservationNoteCount").textContent = `${note.value.length} / 1000`;
    renderSelection();
  });
  contextGroup.addEventListener("click", (event) => {
    const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>("button[data-value]");
    if (!button) return;
    contextType = button.dataset.value || "";
    domain.value = contextType === "lesson" ? "subjects" : "behavior";
    renderContext();
  });
  statusGroup.addEventListener("click", (event) => {
    const button = (event.target as HTMLElement | null)?.closest<HTMLButtonElement>("button[data-value]");
    if (!button) return;
    status = button.dataset.value || "none";
    renderContext();
  });
  domain.addEventListener("change", renderContext);
  required("quickObservationSelectAll").addEventListener("click", () => { selected = new Set(activeStudents().map((student) => student.id)); renderRoster(); });
  required("quickObservationClear").addEventListener("click", () => { selected.clear(); renderRoster(); });
  required("quickObservationRefresh").addEventListener("click", () => void load());
  required("quickObservationHelp").addEventListener("click", openTutorial);
  required("quickObservationTutorialClose").addEventListener("click", closeTutorial);
  required("quickObservationTutorialPrevious").addEventListener("click", () => { tutorialIndex = Math.max(0, tutorialIndex - 1); renderTutorial(); });
  required("quickObservationTutorialNext").addEventListener("click", () => {
    if (tutorialIndex < tutorialSteps.length - 1) { tutorialIndex += 1; renderTutorial(); } else closeTutorial();
  });
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!selected.size) return setFormStatus("기록할 학생을 한 명 이상 선택하세요.", "bad");
    if (!contextType) return setFormStatus("관찰 상황을 선택하세요.", "bad");
    if (!note.value.trim()) return setFormStatus("관찰 메모를 입력하세요.", "bad");
    if (contextType === "lesson" && (!Number(period.value) || !subject.value.trim())) { details.open = true; return setFormStatus("수업 관찰은 교시와 과목을 입력하세요.", "bad"); }
    if (domain.value === "creative" && !creative.value.trim()) { details.open = true; return setFormStatus("창체 영역을 입력하세요.", "bad"); }
    busy = true;
    renderSelection();
    setFormStatus(`${selected.size}명의 기록을 로컬 DB에 저장하고 다시 확인하고 있습니다.`);
    try {
      if (DESIGN_PREVIEW) throw new Error("design_preview_read_only");
      const result = await invoke<{ ok?: boolean; savedCount?: number; error?: string }>("save_quick_observation_batch", { input: {
        tenantId: state.tenantId,
        studentIds: [...selected],
        contextType,
        status,
        note: note.value.trim(),
        date: date.value,
        period: Number(period.value || 0),
        subject: subject.value.trim(),
        recordDomain: domain.value,
        creativeArea: creative.value.trim(),
        tags: tags.value.split(",").map((value) => value.trim()).filter(Boolean),
      } });
      if (result?.ok === false) throw new Error(result.error || "quick_observation_save_failed");
      const savedCount = Number(result.savedCount || selected.size);
      selected.clear();
      note.value = "";
      required("quickObservationNoteCount").textContent = "0 / 1000";
      await load();
      setFormStatus(`${savedCount}명의 관찰기록을 로컬 DB에서 다시 확인했습니다.`, "good");
      search.focus();
    } catch (error) {
      const code = String((error as Error)?.message || error || "");
      const message = code === "design_preview_read_only" ? "시안 모드에서는 실제 저장하지 않습니다." : code === "quick_roster_missing" ? "학생 명단이 없습니다. 교사 홈에서 명단을 다시 연결해 주세요." : "관찰기록을 저장하지 못했습니다. 입력 내용은 유지했습니다.";
      setFormStatus(message, code === "design_preview_read_only" ? "warning" : "bad");
    } finally {
      busy = false;
      renderSelection();
    }
  });
  window.addEventListener("keydown", (event) => {
    if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !save.disabled && !required<HTMLElement>("quickObservationTutorial").hidden) return;
    if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !save.disabled) { event.preventDefault(); form.requestSubmit(); }
  });

  renderContext();
  return { open };
}
