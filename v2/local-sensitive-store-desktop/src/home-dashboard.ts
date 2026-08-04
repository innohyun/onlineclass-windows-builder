import { invoke } from "@tauri-apps/api/core";

type HomeSection = {
  key: string;
  label: string;
  count?: number;
  updatedAtMs?: number;
  route?: string;
};

type HomeOverview = {
  ok?: boolean;
  sections?: HomeSection[];
  error?: string;
};

export type HomeStatus = {
  connected: boolean;
  healthy: boolean;
  tenantLabel: string;
  syncAtMs?: number;
  backupAtMs?: number;
  pending?: number;
};

type DashboardOptions = {
  onViewChange?: (view: string, context: { group?: string; sectionKey?: string }) => void | Promise<void>;
  onSearch?: (query: string) => void | Promise<void>;
};

type RecentRecord = {
  icon: string;
  title: string;
  student: string;
  savedAtMs: number;
  sectionKey: string;
};

const DESIGN_PREVIEW = new URLSearchParams(window.location.search).get("designPreview") === "1";

const HOME_GROUPS = {
  care: ["observations", "teacher-counseling-sessions", "student-private-details"],
  attendance: ["attendance-records", "attendance-nais-checks", "attendance-document-requests"],
  learning: ["eval-assignments", "eval-results", "math-daily-attempts"],
  "student-record": ["student-record-drafts", "student-record-draft-sets"],
} as const;

function element<T extends HTMLElement>(id: string) {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element: ${id}`);
  return found as T;
}

function safeText(value: unknown) {
  return String(value || "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function numeric(value: unknown) {
  return Math.max(0, Number(value || 0) || 0);
}

function formatDateTime(ms?: number) {
  const value = numeric(ms);
  if (!value) return "-";
  const date = new Date(value);
  const now = new Date();
  const startOfDay = (input: Date) => new Date(input.getFullYear(), input.getMonth(), input.getDate()).getTime();
  const dayDiff = Math.round((startOfDay(now) - startOfDay(date)) / 86400000);
  const time = date.toLocaleTimeString("ko-KR", { hour: "numeric", minute: "2-digit" });
  if (dayDiff === 0) return `오늘 ${time}`;
  if (dayDiff === 1) return `어제 ${time}`;
  return `${date.toLocaleDateString("ko-KR", { month: "numeric", day: "numeric" })} ${time}`;
}

function showView(view: string) {
  document.querySelectorAll<HTMLElement>("[data-app-view]").forEach((panel) => {
    const active = panel.dataset.appView === view;
    panel.hidden = !active;
    panel.classList.toggle("is-active", active);
  });
  document.querySelectorAll<HTMLButtonElement>(".sidebar-link[data-app-view-target]").forEach((button) => {
    const active = button.dataset.appViewTarget === view;
    button.classList.toggle("is-active", active);
    if (active) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  });
  document.querySelector<HTMLElement>(".workspace-scroll")?.scrollTo({ top: 0, behavior: "smooth" });
}

function groupCount(sections: HomeSection[], keys: readonly string[]) {
  return sections.reduce((total, section) => total + (keys.includes(section.key) ? numeric(section.count) : 0), 0);
}

function recordTime(row: Record<string, unknown>, fallback: number) {
  return numeric(row.updatedAtMs || row.createdAtMs || row.savedAtMs || row.importedAtMs || row.observedAtMs || fallback);
}

function recordTitle(row: Record<string, unknown>, section: HomeSection, index: number) {
  const raw = row.title
    || row.planName
    || row.subject
    || row.content
    || row.memo
    || row.note
    || row.observation
    || row.summary
    || row.dateKey
    || `${section.label || section.key} ${index + 1}`;
  const text = String(raw || "").replace(/\s+/g, " ").trim();
  return text.length > 48 ? `${text.slice(0, 48)}…` : text;
}

function recordStudent(row: Record<string, unknown>) {
  return String(row.studentName || row.displayName || row.name || row.studentCode || row.studentId || "학급 자료");
}

function iconForSection(key: string) {
  if (HOME_GROUPS.care.includes(key as never)) return "fa-comments";
  if (HOME_GROUPS.attendance.includes(key as never)) return "fa-calendar-check";
  if (HOME_GROUPS.learning.includes(key as never)) return "fa-chart-line";
  return "fa-folder-open";
}

function renderRecent(records: RecentRecord[]) {
  const container = element<HTMLElement>("homeRecentRecords");
  if (!records.length) {
    container.innerHTML = `<p class="home-empty">아직 표시할 저장 자료가 없습니다.</p>`;
    return;
  }
  container.innerHTML = records.slice(0, 4).map((record) => `
    <button class="recent-row" type="button" data-app-view-target="data" data-home-section="${safeText(record.sectionKey)}">
      <span class="recent-icon"><i class="fa-solid ${safeText(record.icon)}" aria-hidden="true"></i></span>
      <span class="recent-title"><strong>${safeText(record.title)}</strong><small>로컬 DB에 안전하게 저장됨</small></span>
      <span class="recent-student">${safeText(record.student)}</span>
      <time>${safeText(formatDateTime(record.savedAtMs))}</time>
      <i class="fa-solid fa-chevron-right recent-arrow" aria-hidden="true"></i>
    </button>
  `).join("");
}

function renderCounts(sections: HomeSection[]) {
  element("homeCareCount").textContent = String(groupCount(sections, HOME_GROUPS.care));
  element("homeAttendanceCount").textContent = String(groupCount(sections, HOME_GROUPS.attendance));
  element("homeLearningCount").textContent = String(groupCount(sections, HOME_GROUPS.learning));
  element("homeStudentRecordCount").textContent = String(groupCount(sections, HOME_GROUPS["student-record"]));
}

function renderPreview() {
  const now = Date.now();
  applyHomeStatus({ connected: true, healthy: true, tenantLabel: "수영초등학교 5학년 1반", syncAtMs: now - 18 * 60000, backupAtMs: now - 19 * 60 * 60000, pending: 0 });
  renderCounts([
    { key: "observations", label: "관찰 기록", count: 124 },
    { key: "teacher-counseling-sessions", label: "상담 기록", count: 38 },
    { key: "attendance-records", label: "출결 기록", count: 86 },
    { key: "eval-results", label: "평가 결과", count: 74 },
    { key: "math-daily-results", label: "매일수학", count: 41 },
    { key: "student-record-drafts", label: "학생부 초안", count: 29 },
  ]);
  renderRecent([
    { icon: "fa-comments", title: "수학 단원평가 후 학습 태도 관찰", student: "김민준", savedAtMs: now - 18 * 60000, sectionKey: "observations" },
    { icon: "fa-calendar-check", title: "교외체험학습 증빙 자료", student: "이서윤", savedAtMs: now - 74 * 60000, sectionKey: "attendance-records" },
    { icon: "fa-chart-line", title: "분수의 덧셈 단원평가 결과", student: "박지후", savedAtMs: now - 3 * 60 * 60000, sectionKey: "eval-results" },
    { icon: "fa-folder-open", title: "행동특성 및 종합의견 초안", student: "최하은", savedAtMs: now - 24 * 60 * 60000, sectionKey: "student-record-drafts" },
  ]);
}

export function initHomeDashboard(options: DashboardOptions = {}) {
  document.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement | null)?.closest<HTMLElement>("[data-app-view-target]");
    if (!target) return;
    const view = target.dataset.appViewTarget || "home";
    showView(view);
    void options.onViewChange?.(view, { group: target.dataset.homeKind, sectionKey: target.dataset.homeSection });
  });

  element<HTMLFormElement>("homeSearchForm").addEventListener("submit", (event) => {
    event.preventDefault();
    const query = element<HTMLInputElement>("homeSearchInput").value.trim();
    showView("data");
    void options.onSearch?.(query);
  });

  showView("home");
  if (DESIGN_PREVIEW) renderPreview();
}

function applyHomeStatus(status: HomeStatus) {
  element("homeTenantLabel").textContent = status.tenantLabel || "연결된 학급 없음";
  element("homeConnectionText").textContent = status.connected ? "정상 연결" : "연결 필요";
  element("homeBackupText").textContent = formatDateTime(status.backupAtMs);
  element("homeSyncText").textContent = formatDateTime(status.syncAtMs);
  element("homeFooterBackupText").textContent = formatDateTime(status.backupAtMs);
  element("homePendingText").textContent = `${numeric(status.pending)}건`;
  element("homeHealthText").textContent = status.healthy ? "연결 정상" : status.connected ? "확인 필요" : "연결 필요";
  document.body.dataset.homeHealth = status.healthy ? "ok" : "warning";
}

export function renderHomeStatus(status: HomeStatus) {
  if (DESIGN_PREVIEW) return;
  applyHomeStatus(status);
}

export async function loadHomeOverview(tenantId: string) {
  if (DESIGN_PREVIEW) return;
  const safeTenantId = String(tenantId || "").trim();
  if (!safeTenantId) {
    renderCounts([]);
    renderRecent([]);
    return;
  }

  try {
    const overview = await invoke<HomeOverview>("get_local_overview", { tenantId: safeTenantId });
    if (overview?.ok === false) throw new Error(overview.error || "local_overview_failed");
    const sections = Array.isArray(overview.sections) ? overview.sections : [];
    renderCounts(sections);

    const candidates = [...sections]
      .filter((section) => section.route && numeric(section.count) > 0)
      .sort((a, b) => numeric(b.updatedAtMs) - numeric(a.updatedAtMs));
    const recent: RecentRecord[] = [];
    for (const section of candidates.slice(0, 6)) {
      const payload = await invoke<{ ok?: boolean; records?: unknown[]; error?: string }>("list_local_data_section", {
        tenantId: safeTenantId,
        route: section.route,
        limit: 4,
      });
      if (payload?.ok === false) continue;
      const rows = Array.isArray(payload.records) ? payload.records : [];
      rows.slice(0, 2).forEach((record, index) => {
        const row = (record && typeof record === "object" ? record : {}) as Record<string, unknown>;
        recent.push({
          icon: iconForSection(section.key),
          title: recordTitle(row, section, index),
          student: recordStudent(row),
          savedAtMs: recordTime(row, numeric(section.updatedAtMs)),
          sectionKey: section.key,
        });
      });
    }
    recent.sort((a, b) => b.savedAtMs - a.savedAtMs);
    renderRecent(recent);
  } catch (_) {
    renderCounts([]);
    renderRecent([]);
  }
}
