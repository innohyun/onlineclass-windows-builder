import { invoke } from "@tauri-apps/api/core";
import {
  firstText,
  recordStatusLabel,
  recordSummary,
  sectionLabel,
  studentName,
  type LocalDataRecord,
  type SearchResult,
} from "./data-explorer";

export type HomeStatus = {
  connected: boolean;
  healthy: boolean;
  storeReady?: boolean;
  tenantLabel: string;
  syncAtMs?: number;
  backupAtMs?: number;
  pending?: number;
};

type DashboardOptions = {
  onViewChange?: (view: string, context: { group?: string; sectionKey?: string; attachment?: boolean }) => void | Promise<void>;
  onSearch?: (query: string) => void | Promise<void>;
};

const DESIGN_PREVIEW = new URLSearchParams(window.location.search).get("designPreview") === "1";

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
  document.body.dataset.appView = view;
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

function recordTitle(record: LocalDataRecord) {
  if (record.sectionKey === "work-notes") {
    return `${firstText(record.payload, ["emoji"])} ${firstText(record.payload, ["title"]) || "제목 없음"}`.trim();
  }
  return `${studentName(record)} · ${sectionLabel(record)}`;
}

function recordMeta(record: LocalDataRecord) {
  const summary = recordSummary(record);
  return record.sectionKey === "work-notes"
    ? `업무 노트 · ${summary}`
    : `${sectionLabel(record)} · ${summary}`;
}

function renderCards(containerId: string, records: LocalDataRecord[], kind: "work-notes" | "sensitive") {
  const container = element<HTMLElement>(containerId);
  if (!records.length) {
    container.innerHTML = `<p class="home-empty">${kind === "work-notes" ? "아직 저장된 업무 노트가 없습니다." : "아직 표시할 민감 자료가 없습니다."}</p>`;
    return;
  }
  container.innerHTML = records.slice(0, 4).map((record) => {
    const status = record.sectionKey === "work-notes" ? "로컬 보관" : recordStatusLabel(record) || "로컬 저장";
    return `
      <button class="home-record-card" type="button" data-app-view-target="data" data-home-kind="${safeText(record.groupKey)}" data-home-section="${safeText(record.sectionKey)}">
        <span class="home-record-icon${kind === "sensitive" ? " is-sensitive" : ""}"><i class="fa-solid ${record.sectionKey === "work-notes" ? "fa-note-sticky" : "fa-user-shield"}" aria-hidden="true"></i></span>
        <span class="home-record-copy"><strong>${safeText(recordTitle(record))}</strong><small>${safeText(recordMeta(record))}</small></span>
        <span class="home-record-side"><span class="home-record-badge">${safeText(status)}</span><time>${safeText(formatDateTime(record.updatedAtMs))}</time></span>
        <i class="fa-solid fa-chevron-right home-record-arrow" aria-hidden="true"></i>
      </button>`;
  }).join("");
}

function previewRecord(input: Partial<LocalDataRecord> & { sectionKey: string; groupKey: string; payload: Record<string, unknown>; updatedAtMs: number }): LocalDataRecord {
  return {
    sectionLabel: input.sectionKey === "work-notes" ? "업무 노트" : "상담 기록",
    hasAttachment: false,
    ...input,
  };
}

function renderPreview() {
  const now = Date.now();
  applyHomeStatus({ connected: true, healthy: true, storeReady: true, tenantLabel: "수영초등학교 5학년 1반", syncAtMs: now - 18 * 60000, backupAtMs: now - 19 * 60 * 60000, pending: 0 });
  renderCards("homeRecentWorkNotes", [
    previewRecord({ sectionKey: "work-notes", groupKey: "work-notes", updatedAtMs: now - 18 * 60000, payload: { emoji: "📝", title: "5학년 2학기 학습으로의 평가 계획", markdown: "교과별 평가 기준과 제출 일정을 정리했습니다." } }),
    previewRecord({ sectionKey: "work-notes", groupKey: "work-notes", updatedAtMs: now - 74 * 60000, payload: { emoji: "📌", title: "학부모 상담 주간 준비", markdown: "상담 일정과 확인할 내용을 정리했습니다." } }),
  ], "work-notes");
  renderCards("homeRecentSensitive", [
    previewRecord({ sectionKey: "teacher-counseling-sessions", groupKey: "care", updatedAtMs: now - 42 * 60000, payload: { studentName: "김하늘", summary: "학생 및 보호자 면담 내용을 기록했습니다.", status: "recorded" } }),
    previewRecord({ sectionKey: "student-private-details", groupKey: "care", updatedAtMs: now - 20 * 60 * 60000, payload: { studentName: "박도윤", specialNote: "건강 및 생활 정보를 확인했습니다.", status: "reviewed" } }),
  ], "sensitive");
}

export function initHomeDashboard(options: DashboardOptions = {}) {
  document.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement | null)?.closest<HTMLElement>("[data-app-view-target]");
    if (!target) return;
    const view = target.dataset.appViewTarget || "home";
    showView(view);
    void options.onViewChange?.(view, {
      group: target.dataset.homeKind,
      sectionKey: target.dataset.homeSection,
      attachment: target.dataset.homeFilter === "attachments",
    });
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
  element("homeSafetyBackupText").textContent = formatDateTime(status.backupAtMs);
  element("homeSyncText").textContent = formatDateTime(status.syncAtMs);
  element("homeFooterBackupText").textContent = formatDateTime(status.backupAtMs);
  element("homePendingText").textContent = `${numeric(status.pending)}건`;
  element("homeStoreText").textContent = status.storeReady === false ? "확인 필요" : "정상";
  element("homeHealthText").textContent = status.healthy ? "로컬 저장소 정상" : status.connected ? "확인 필요" : "연결 필요";
  document.body.dataset.homeHealth = status.healthy ? "ok" : "warning";
}

export function renderHomeStatus(status: HomeStatus) {
  if (DESIGN_PREVIEW) return;
  applyHomeStatus(status);
}

async function loadRecentGroup(tenantId: string, group: "work-notes" | "care") {
  const result = await invoke<SearchResult>("search_local_data", {
    input: {
      tenantId,
      group,
      sectionKey: "",
      studentQuery: "",
      textQuery: "",
      dateFrom: "",
      dateTo: "",
      hasAttachment: false,
      offset: 0,
      limit: 4,
    },
  });
  if (result?.ok === false) throw new Error(result.error || "local_data_search_failed");
  return Array.isArray(result.records) ? result.records : [];
}

export async function loadHomeOverview(tenantId: string) {
  if (DESIGN_PREVIEW) return;
  const safeTenantId = String(tenantId || "").trim();
  if (!safeTenantId) {
    renderCards("homeRecentWorkNotes", [], "work-notes");
    renderCards("homeRecentSensitive", [], "sensitive");
    return;
  }
  try {
    const [workNotes, sensitive] = await Promise.all([
      loadRecentGroup(safeTenantId, "work-notes"),
      loadRecentGroup(safeTenantId, "care"),
    ]);
    renderCards("homeRecentWorkNotes", workNotes, "work-notes");
    renderCards("homeRecentSensitive", sensitive, "sensitive");
  } catch {
    renderCards("homeRecentWorkNotes", [], "work-notes");
    renderCards("homeRecentSensitive", [], "sensitive");
  }
}
