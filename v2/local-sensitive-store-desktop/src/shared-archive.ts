import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";

const DEFAULT_API_URL = "https://classaimate-v3.pages.dev";

type ArchiveSummary = {
  id: string;
  tenantId: string;
  sourceType: "assignment" | "board" | string;
  title: string;
  recordCount: number;
  fileCount: number;
  totalFileBytes: number;
  importedAt: number;
};

type ArchiveDetail = {
  meta: ArchiveSummary & { sourceId: string };
  records: Array<{ ordinal: number; type: string; payload: unknown }>;
  files: Array<{ ordinal: number; originalName: string; contentType: string; byteSize: number }>;
};

type CommandResult = {
  ok: boolean;
  error?: string;
  archives?: ArchiveSummary[];
  archive?: ArchiveDetail;
  archiveId?: string;
  title?: string;
  recordCount?: number;
  fileCount?: number;
};

let archives: ArchiveSummary[] = [];
let selectedArchiveId = "";
let detail: ArchiveDetail | null = null;
let busy = false;

function el<T extends HTMLElement>(id: string) {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element: ${id}`);
  return node as T;
}

function escapeHtml(value: unknown) {
  return String(value ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function byteText(value: number) {
  const bytes = Math.max(0, Number(value || 0));
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

function dateText(value: number) {
  return value ? new Date(value).toLocaleString("ko-KR") : "-";
}

function errorText(value: unknown) {
  const code = String(value || "archive_failed");
  const messages: Record<string, string> = {
    archive_code_invalid: "보관 코드는 영문·숫자 43자로 입력해 주세요.",
    archive_http_401: "보관 코드가 만료되었거나 이미 사용되었습니다. 웹에서 새 코드를 발급해 주세요.",
    archive_http_404: "보관 자료를 찾지 못했습니다.",
    archive_network_error: "V3 서버에 연결하지 못했습니다. 인터넷 연결을 확인해 주세요.",
    archive_manifest_verify_failed: "보관 목록 무결성 검증에 실패했습니다. 내려받기를 중단했습니다.",
  };
  return messages[code] || `처리하지 못했습니다: ${code}`;
}

function setStatus(message: string, tone: "neutral" | "ok" | "error" = "neutral") {
  const status = el("sharedArchiveStatus");
  status.textContent = message;
  status.className = `status-message archive-status-${tone}`;
}

function render() {
  el<HTMLButtonElement>("sharedArchiveImportButton").disabled = busy;
  el<HTMLButtonElement>("sharedArchiveRefreshButton").disabled = busy;
  el<HTMLButtonElement>("sharedArchiveExportButton").disabled = busy || !selectedArchiveId;
  const list = el("sharedArchiveList");
  list.innerHTML = archives.length ? archives.map((archive) => `
    <button class="archive-row${archive.id === selectedArchiveId ? " is-selected" : ""}" type="button" data-archive-id="${escapeHtml(archive.id)}">
      <strong>${escapeHtml(archive.title)}</strong>
      <span>${archive.sourceType === "assignment" ? "과제" : "보드"} · 기록 ${archive.recordCount}건 · 파일 ${archive.fileCount}개</span>
      <small>${escapeHtml(dateText(archive.importedAt))} · ${escapeHtml(byteText(archive.totalFileBytes))}</small>
    </button>`).join("") : '<p class="data-empty">아직 이 PC에 보관한 공유자료가 없습니다.</p>';
  const detailNode = el("sharedArchiveDetail");
  if (!detail) {
    detailNode.innerHTML = '<p class="data-empty">보관 자료를 선택하면 읽기 전용 내용과 첨부파일을 확인할 수 있습니다.</p>';
    return;
  }
  detailNode.innerHTML = `
    <div class="archive-detail-head"><div><strong>${escapeHtml(detail.meta.title)}</strong><span>${escapeHtml(detail.meta.tenantId)}</span></div><span class="status-badge badge-ok">읽기 전용</span></div>
    <dl class="compact-list"><div><dt>기록</dt><dd>${detail.records.length}건</dd></div><div><dt>첨부파일</dt><dd>${detail.files.length}개 · ${byteText(detail.meta.totalFileBytes)}</dd></div></dl>
    <div class="archive-file-list">${detail.files.length ? detail.files.map((file) => `
      <button type="button" class="archive-file-row" data-archive-file="${file.ordinal}">
        <span>${escapeHtml(file.originalName)}</span><small>${escapeHtml(byteText(file.byteSize))}</small>
      </button>`).join("") : '<p class="data-empty">첨부파일이 없습니다.</p>'}</div>
    <details class="archive-records"><summary>보관 기록 보기</summary><pre>${escapeHtml(JSON.stringify(detail.records, null, 2))}</pre></details>`;
}

async function loadArchives() {
  busy = true;
  render();
  try {
    const result = await invoke<CommandResult>("list_shared_archives");
    if (!result.ok) throw new Error(result.error);
    archives = result.archives || [];
    if (selectedArchiveId && !archives.some((item) => item.id === selectedArchiveId)) {
      selectedArchiveId = "";
      detail = null;
    }
    setStatus(`${archives.length}개의 로컬 공유자료 보관본을 확인했습니다.`, "ok");
  } catch (error) {
    setStatus(errorText((error as Error).message), "error");
  } finally {
    busy = false;
    render();
  }
}

async function selectArchive(id: string) {
  selectedArchiveId = id;
  detail = null;
  render();
  const result = await invoke<CommandResult>("get_shared_archive", { archiveId: id });
  if (!result.ok || !result.archive) throw new Error(result.error);
  detail = result.archive;
  render();
}

async function importArchive() {
  const input = el<HTMLInputElement>("sharedArchiveCodeInput");
  const code = input.value.trim();
  if (!code) {
    setStatus("웹에서 발급한 10분 유효 보관 코드를 입력해 주세요.", "error");
    return;
  }
  busy = true;
  render();
  setStatus("목록과 첨부파일의 SHA-256을 확인하며 내려받는 중입니다.");
  try {
    const result = await invoke<CommandResult>("import_shared_archive", { baseUrl: DEFAULT_API_URL, code });
    if (!result.ok) throw new Error(result.error);
    input.value = "";
    selectedArchiveId = result.archiveId || "";
    setStatus(`“${result.title || "공유자료"}” 보관을 완료했습니다. 서버 첨부파일은 72시간 유예 후 삭제됩니다.`, "ok");
    await loadArchives();
    if (selectedArchiveId) await selectArchive(selectedArchiveId);
  } catch (error) {
    setStatus(errorText((error as Error).message), "error");
  } finally {
    busy = false;
    render();
  }
}

async function exportArchive() {
  if (!selectedArchiveId) return;
  const selected = archives.find((item) => item.id === selectedArchiveId);
  const path = await save({ defaultPath: `${selected?.title || "공유자료"}-보관본.json`, filters: [{ name: "JSON", extensions: ["json"] }] });
  if (!path) return;
  const result = await invoke<CommandResult>("export_shared_archive", { archiveId: selectedArchiveId, targetPath: path });
  setStatus(result.ok ? "읽기 전용 보관본을 JSON으로 내보냈습니다." : errorText(result.error), result.ok ? "ok" : "error");
}

export function initSharedArchive() {
  el<HTMLButtonElement>("sharedArchiveImportButton").addEventListener("click", () => void importArchive());
  el<HTMLButtonElement>("sharedArchiveRefreshButton").addEventListener("click", () => void loadArchives());
  el<HTMLButtonElement>("sharedArchiveExportButton").addEventListener("click", () => void exportArchive());
  el("sharedArchiveList").addEventListener("click", (event) => {
    const row = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-archive-id]");
    if (row) void selectArchive(row.dataset.archiveId || "").catch((error) => setStatus(errorText((error as Error).message), "error"));
  });
  el("sharedArchiveDetail").addEventListener("click", (event) => {
    const row = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-archive-file]");
    if (!row || !selectedArchiveId) return;
    void invoke<CommandResult>("open_shared_archive_file", { archiveId: selectedArchiveId, ordinal: Number(row.dataset.archiveFile) })
      .then((result) => { if (!result.ok) setStatus(errorText(result.error), "error"); });
  });
  render();
  void loadArchives();
}
