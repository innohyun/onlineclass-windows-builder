import { invoke } from "@tauri-apps/api/core";
import { sectionLabel, type LocalDataRecord, type SearchResult } from "./data-explorer";

export type HomeStatus = {
  connected: boolean;
  healthy: boolean;
  storeReady?: boolean;
  tenantLabel: string;
  deviceName?: string;
  syncAtMs?: number;
  backupAtMs?: number;
  pending?: number;
};

type ViewContext = { group?: string; sectionKey?: string; attachment?: boolean };
type DashboardOptions = {
  beforeViewChange?: () => boolean | Promise<boolean>;
  onViewChange?: (view: string, context: ViewContext) => void | Promise<void>;
  onSearch?: (query: string) => void | Promise<void>;
};
type HomeDocument = {
  pageId: string;
  title: string;
  emoji?: string;
  systemKind?: string;
  updatedAtMs: number;
  attachmentCount?: number;
  workspace: "workNotes" | "lessonMaterials";
};
type WorkspaceResult = { ok?: boolean; total?: number; truncated?: boolean; pages?: Omit<HomeDocument, "workspace">[]; error?: string };
type TabReference = { kind: string; pageId: string; documentId?: string; openedAt?: number };

const DESIGN_PREVIEW = new URLSearchParams(window.location.search).get("designPreview") === "1";
let currentTenant = "";
let homeDocuments: HomeDocument[] = [];
let overviewGeneration = 0;
let overviewIncomplete = false;

function element<T extends HTMLElement>(id: string) {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element: ${id}`);
  return found as T;
}

function safeText(value: unknown) {
  return String(value || "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}
function numeric(value: unknown) { return Math.max(0, Number(value || 0) || 0); }
function formatDateTime(ms?: number) {
  const value = numeric(ms);
  if (!value) return "아직 없음";
  return new Date(value).toLocaleString("ko-KR", { month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit" });
}

function showView(view: string, context: ViewContext = {}) {
  document.body.dataset.appView = view;
  document.querySelectorAll<HTMLElement>("[data-app-view]").forEach((panel) => {
    const active = panel.dataset.appView === view;
    panel.hidden = !active;
    panel.classList.toggle("is-active", active);
  });
  document.querySelectorAll<HTMLButtonElement>(".sidebar-link[data-app-view-target]").forEach((button) => {
    const active = button.dataset.appViewTarget === view && (view !== "data" || (button.dataset.homeFilter === "attachments") === Boolean(context.attachment));
    button.classList.toggle("is-active", active);
    if (active) button.setAttribute("aria-current", "page"); else button.removeAttribute("aria-current");
  });
  document.querySelector<HTMLElement>(".workspace-scroll")?.scrollTo({ top: 0, behavior: "auto" });
}

// Match the web note tab preference contract. Persist references only, never document text.
function preferenceKey() { return `classaimate:work-notes:tabs:v2:${currentTenant}`; }
function tabPreferences(): { pinned: TabReference[]; recent: TabReference[] } {
  const references = (values: unknown, recent: boolean): TabReference[] => {
    if (!Array.isArray(values)) return [];
    const result: TabReference[] = [];
    const seen = new Set<string>();
    for (const ref of values) {
      if (!ref || typeof ref !== "object" || typeof ref.pageId !== "string" || !ref.pageId.trim() || ref.pageId.length > 160) continue;
      if (ref.kind !== "local" && ref.kind !== "cloud") continue;
      if (ref.kind === "cloud" && (typeof ref.documentId !== "string" || !ref.documentId.trim() || ref.documentId.length > 160)) continue;
      if (recent && (!Number.isFinite(Number(ref.openedAt)) || Number(ref.openedAt) <= 0)) continue;
      const key = `${ref.kind}:${ref.documentId || ''}:${ref.pageId}`;
      if (seen.has(key)) continue;
      seen.add(key);
      result.push({ kind: ref.kind, pageId: ref.pageId, ...(ref.kind === "cloud" ? { documentId: ref.documentId } : {}), ...(recent ? { openedAt: Number(ref.openedAt) } : {}) });
    }
    return result.slice(0, recent ? 20 : 200);
  };
  try {
    const value = JSON.parse(localStorage.getItem(preferenceKey()) || "null");
    return { pinned: references(value?.pinned, false), recent: references(value?.recent, true) };
  } catch { return { pinned: [], recent: [] }; }
}
function pinnedIds() {
  return new Set(tabPreferences().pinned.filter((ref) => ref.kind === "local" && typeof ref.pageId === "string").map((ref) => ref.pageId));
}
function documentCard(page: HomeDocument, favorite: boolean) {
  return `<div class="desk-document-row"><button class="home-record-card" type="button" data-desk-page="${safeText(page.pageId)}"><span class="home-record-icon">${safeText(page.emoji || (page.workspace === "lessonMaterials" ? "📚" : "📝"))}</span><span class="home-record-copy"><strong>${safeText(page.title || "제목 없음")}</strong><small>${page.workspace === "lessonMaterials" ? "수업자료" : "업무 노트"}${page.attachmentCount ? ` · 첨부 ${numeric(page.attachmentCount)}개` : ""}</small></span><span class="home-record-side"><span class="home-record-badge">이 PC 저장본</span><time>${safeText(formatDateTime(page.updatedAtMs))}</time></span></button><button class="desk-favorite-toggle${favorite ? " is-pinned" : ""}" type="button" data-desk-pin="${safeText(page.pageId)}" aria-label="${favorite ? "즐겨찾기 해제" : "즐겨찾기 추가"}" aria-pressed="${favorite}"><i class="fa-solid fa-star" aria-hidden="true"></i></button></div>`;
}
function renderDocuments() {
  const pinned = pinnedIds();
  const recent = [...homeDocuments].sort((a, b) => b.updatedAtMs - a.updatedAtMs).slice(0, 4);
  const favorites = homeDocuments.filter((page) => pinned.has(page.pageId));
  element("homeRecentWorkNotes").innerHTML = recent.length ? recent.map((page) => documentCard(page, pinned.has(page.pageId))).join("") : overviewIncomplete ? '<p class="home-empty" role="status">문서 목록을 확인하지 못했습니다. 자료함에서 다시 확인해 주세요.</p>' : '<p class="home-empty">아직 저장된 문서가 없습니다. 새 문서를 작성하거나 교사 홈에서 자료를 로컬로 전환하세요.</p>';
  element("homeFavoriteDocuments").innerHTML = favorites.length ? favorites.map((page) => documentCard(page, true)).join("") : overviewIncomplete ? '<p class="home-empty" role="status">즐겨찾는 문서의 저장본을 확인하지 못했습니다. 자료함을 다시 불러와 주세요.</p>' : '<p class="home-empty">문서 옆 별표를 눌러 자주 사용하는 자료를 모아보세요.<br>즐겨찾기는 이 PC의 학급별 화면 설정입니다.</p>';
  element("homeFavoriteCount").textContent = overviewIncomplete ? favorites.length ? `${favorites.length}+` : "—" : String(favorites.length);
  if (overviewIncomplete && recent.length) element("homeRecentWorkNotes").insertAdjacentHTML("beforeend", '<p class="desk-panel-note" role="status">일부 문서 목록을 확인하지 못했습니다. 자료함에서 새로고침하거나 검색으로 확인하세요.</p>');
}
function toggleFavorite(pageId: string) {
  if (!currentTenant || !homeDocuments.some((page) => page.pageId === pageId)) return;
  const preferences = tabPreferences();
  const exists = preferences.pinned.some((ref) => ref.kind === "local" && ref.pageId === pageId);
  preferences.pinned = preferences.pinned.filter((ref) => !(ref.kind === "local" && ref.pageId === pageId));
  if (!exists) preferences.pinned.push({ kind: "local", pageId });
  try {
    localStorage.setItem(preferenceKey(), JSON.stringify({ pinned: preferences.pinned.slice(-200), recent: preferences.recent.slice(0, 20) }));
    renderDocuments();
  } catch { element("homeFavoriteDocuments").innerHTML = '<p class="home-empty" role="alert">즐겨찾기 설정을 저장하지 못했습니다. 문서 원본은 변경되지 않았습니다.</p>'; }
}
function renderRecords(records: LocalDataRecord[]) {
  element("homeRecentSensitive").innerHTML = records.length ? records.slice(0, 4).map((record) => `<button class="home-record-card" type="button" data-app-view-target="data" data-home-kind="${safeText(record.groupKey)}" data-home-section="${safeText(record.sectionKey)}"><span class="home-record-icon is-sensitive"><i class="fa-solid fa-user-shield" aria-hidden="true"></i></span><span class="home-record-copy"><strong>${safeText(sectionLabel(record))}</strong><small>학생 이름·원문은 상세에서 확인</small></span><span class="home-record-side"><time>${safeText(formatDateTime(record.updatedAtMs))}</time></span><i class="fa-solid fa-chevron-right home-record-arrow" aria-hidden="true"></i></button>`).join("") : '<p class="home-empty">아직 표시할 학생 기록이 없습니다.</p>';
}

export function initHomeDashboard(options: DashboardOptions = {}) {
  let navigationGeneration = 0;
  const navigate = async (view: string, context: ViewContext = {}, searchQuery?: string) => {
    const generation = ++navigationGeneration;
    if (options.beforeViewChange && !await options.beforeViewChange()) return false;
    if (generation !== navigationGeneration) return false;
    showView(view, context);
    if (searchQuery !== undefined) await options.onSearch?.(searchQuery);
    else await options.onViewChange?.(view, context);
    return true;
  };
  document.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement | null)?.closest<HTMLElement>("[data-app-view-target]");
    if (!target) return;
    void navigate(target.dataset.appViewTarget || "home", { group: target.dataset.homeKind, sectionKey: target.dataset.homeSection, attachment: target.dataset.homeFilter === "attachments" });
  });
  element<HTMLFormElement>("homeSearchForm").addEventListener("submit", async (event) => {
    event.preventDefault();
    const query = element<HTMLInputElement>("homeSearchInput").value.trim();
    await navigate("data", {}, query);
  });
  document.addEventListener("click", (event) => {
    const target = event.target as HTMLElement | null;
    const pin = target?.closest<HTMLElement>("[data-desk-pin]");
    if (pin) { toggleFavorite(pin.dataset.deskPin || ""); return; }
    const row = target?.closest<HTMLElement>("[data-desk-page]");
    const page = homeDocuments.find((entry) => entry.pageId === row?.dataset.deskPage);
    if (page) document.dispatchEvent(new CustomEvent("desk:open-document", { detail: { pageId: page.pageId, workspace: page.workspace, tenantId: currentTenant } }));
  });
  window.addEventListener("desk:toggle-favorite", (event) => toggleFavorite(String((event as CustomEvent).detail?.pageId || "")));
  showView("home");
  if (DESIGN_PREVIEW) {
    currentTenant = "design-preview";
    homeDocuments = [
      { pageId: "preview-note", title: "오늘의 수업 준비", emoji: "📝", updatedAtMs: Date.now(), workspace: "workNotes" },
      { pageId: "preview-lesson", title: "과학 · 관찰과 탐구", emoji: "🔎", updatedAtMs: Date.now() - 3600000, workspace: "lessonMaterials" },
    ];
    renderDocuments();
    element("homeDocumentCount").textContent = "2";
    renderRecords([]);
    applyHomeStatus({ connected: false, healthy: false, storeReady: false, tenantLabel: "디자인 미리보기 · 예시 학급", deviceName: "예시 PC" });
    element("homeHealthText").textContent = "디자인 미리보기 · 실제 DB 연결 없음";
  }
  return { navigate };
}

function applyHomeStatus(status: HomeStatus) {
  element("homeTenantLabel").textContent = status.tenantLabel || "연결된 학급 없음";
  element("deskDeviceName").textContent = status.deviceName || "이 PC 자료함";
  element("homeConnectionText").textContent = status.connected ? "학급 연결됨" : "연결 필요";
  element("homeBackupText").textContent = formatDateTime(status.backupAtMs);
  element("homeSafetyBackupText").textContent = formatDateTime(status.backupAtMs);
  element("homeSyncText").textContent = formatDateTime(status.syncAtMs);
  element("homeFooterBackupText").textContent = formatDateTime(status.backupAtMs);
  element("homePendingText").textContent = status.pending == null ? "확인 필요" : `${numeric(status.pending)}건`;
  element("homeStoreText").textContent = status.storeReady ? "정상" : "확인 필요";
  element("homeHealthText").textContent = status.healthy ? "이 PC 저장소를 사용할 수 있습니다" : status.connected ? "저장·백업 상태를 확인해 주세요" : "교사 계정과 이 PC를 연결해 주세요";
  document.body.dataset.homeHealth = status.healthy ? "ok" : "warning";
  window.dispatchEvent(new CustomEvent("desk:status", { detail: status }));
}
export function renderHomeStatus(status: HomeStatus) { if (!DESIGN_PREVIEW) applyHomeStatus(status); }

export async function loadHomeOverview(tenantId: string) {
  if (DESIGN_PREVIEW) return;
  const generation = ++overviewGeneration;
  currentTenant = String(tenantId || "").trim();
  const tenant = currentTenant;
  homeDocuments = [];
  overviewIncomplete = false;
  element("homeDocumentCount").textContent = "—";
  element("homeFavoriteCount").textContent = "—";
  if (!tenant) { renderDocuments(); renderRecords([]); return; }
  element("homeRecentWorkNotes").innerHTML = '<p class="home-empty">저장된 문서 목록을 읽고 있습니다.</p>';
  const [work, lesson, records] = await Promise.allSettled([
    invoke<WorkspaceResult>("get_local_workspace_tree", { tenantId: tenant, workspace: "work_materials" }),
    invoke<WorkspaceResult>("get_local_workspace_tree", { tenantId: tenant, workspace: "lesson_materials" }),
    invoke<SearchResult>("search_local_data", { input: { tenantId: tenant, group: "care", sectionKey: "", studentQuery: "", textQuery: "", dateFrom: "", dateTo: "", hasAttachment: false, offset: 0, limit: 4 } }),
  ]);
  if (generation !== overviewGeneration || tenant !== currentTenant) return;
  let incomplete = false;
  for (const [result, workspace] of [[work, "workNotes"], [lesson, "lessonMaterials"]] as const) {
    if (result.status === "rejected" || result.value.ok === false) { incomplete = true; continue; }
    const pages = (result.value.pages || []).filter((page) => !["lesson_materials_folder", "student_learning_materials_folder"].includes(page.systemKind || ""));
    homeDocuments.push(...pages.map((page) => ({ ...page, workspace })));
    if (result.value.truncated) incomplete = true;
  }
  overviewIncomplete = incomplete;
  renderDocuments();
  element("homeDocumentCount").textContent = incomplete ? homeDocuments.length ? `${homeDocuments.length}+` : "—" : String(homeDocuments.length);
  if (records.status === "fulfilled" && records.value.ok !== false) renderRecords(records.value.records || []);
  else element("homeRecentSensitive").innerHTML = '<p class="home-empty" role="status">학생 기록을 불러오지 못했습니다. 연결·저장 상태를 확인해 주세요.</p>';
}
