import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";

const DEFAULT_API_URL = "https://classaimate-v3.pages.dev";

export type ArchiveSummary = {
  id: string;
  sourceType: "assignment" | "board" | string;
  title: string;
  recordCount: number;
  contentCount?: number;
  fileCount: number;
  totalFileBytes: number;
  importedAt: number;
};

export type ArchiveRecord = {
  ordinal: number;
  type: string;
  payload: unknown;
};

export type ArchiveFile = {
  ordinal: number;
  originalName: string;
  contentType: string;
  byteSize: number;
};

export type ArchiveDetail = {
  meta: ArchiveSummary;
  records: ArchiveRecord[];
  files: ArchiveFile[];
};

export type ArchiveCommandResult = {
  ok: boolean;
  error?: string;
  archives?: ArchiveSummary[];
  archive?: ArchiveDetail;
  archiveId?: string;
  title?: string;
  recordCount?: number;
  fileCount?: number;
};

export type ArchiveBridge = {
  list(): Promise<ArchiveCommandResult>;
  detail(archiveId: string): Promise<ArchiveCommandResult>;
  import(code: string): Promise<ArchiveCommandResult>;
  exportArchive(archiveId: string, title: string): Promise<ArchiveCommandResult>;
  openFile(archiveId: string, ordinal: number): Promise<ArchiveCommandResult>;
};

type RecordPresentation = {
  ordinal: number;
  label: string;
  time: string;
  title: string;
  body: string;
  feedback: string;
};

const tauriBridge: ArchiveBridge = {
  list: () => invoke<ArchiveCommandResult>("list_shared_archives"),
  detail: (archiveId) => invoke<ArchiveCommandResult>("get_shared_archive", { archiveId }),
  import: (code) => invoke<ArchiveCommandResult>("import_shared_archive", { baseUrl: DEFAULT_API_URL, code }),
  async exportArchive(archiveId, title) {
    const targetPath = await save({
      defaultPath: `${title || "공유자료"}-보관본.json`,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!targetPath) return { ok: false, error: "archive_export_cancelled" };
    return invoke<ArchiveCommandResult>("export_shared_archive", { archiveId, targetPath });
  },
  openFile: (archiveId, ordinal) => invoke<ArchiveCommandResult>("open_shared_archive_file", { archiveId, ordinal }),
};

let activeBridge = tauriBridge;
let archives: ArchiveSummary[] = [];
let selectedArchiveId = "";
let selectedRecordOrdinal = -1;
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
  if (!value) return "-";
  return new Intl.DateTimeFormat("ko-KR", {
    year: "numeric", month: "long", day: "numeric", hour: "numeric", minute: "2-digit",
  }).format(new Date(value));
}

function shortDateText(value: number) {
  if (!value) return "-";
  const date = new Date(value);
  const now = new Date();
  const time = new Intl.DateTimeFormat("ko-KR", { hour: "numeric", minute: "2-digit" }).format(date);
  if (date.toDateString() === now.toDateString()) return `오늘 ${time}`;
  return `${date.getMonth() + 1}월 ${date.getDate()}일 ${time}`;
}

function errorText(value: unknown) {
  const code = String(value || "archive_failed");
  const messages: Array<[string, string]> = [
    ["archive_code_invalid", "보관 코드는 영문·숫자 43자로 입력해 주세요."],
    ["archive_http_401", "보관 코드가 만료되었거나 이미 사용되었습니다. 웹에서 새 코드를 발급해 주세요."],
    ["archive_http_404", "보관 자료를 찾지 못했습니다."],
    ["archive_network_error", "서버에 연결하지 못했습니다. 인터넷 연결을 확인해 주세요."],
    ["archive_file_network_error", "첨부파일을 내려받지 못했습니다. 연결을 확인한 뒤 같은 코드로 다시 시도하세요."],
    ["archive_manifest_verify_failed", "보관 목록 무결성 검증에 실패해 내려받기를 중단했습니다."],
    ["archive_record_verify_failed", "보관 내용 무결성 검증에 실패해 내려받기를 중단했습니다."],
    ["archive_file_verify_failed", "첨부파일 무결성 검증에 실패해 완료 처리하지 않았습니다."],
    ["archive_file_not_found", "선택한 첨부파일을 찾지 못했습니다."],
    ["archive_file_path_failed", "첨부파일이 이동되었거나 손상되었습니다."],
    ["archive_file_not_regular", "일반 파일만 열 수 있습니다."],
    ["archive_file_open_failed", "첨부파일을 기본 프로그램으로 열지 못했습니다."],
    ["archive_export_cancelled", "JSON 내보내기를 취소했습니다."],
    ["archive_export_write_failed", "선택한 위치에 JSON 파일을 저장하지 못했습니다."],
  ];
  return messages.find(([prefix]) => code === prefix || code.startsWith(`${prefix}:`))?.[1]
    || "처리하지 못했습니다. 잠시 후 다시 시도해 주세요.";
}

function setStatus(message: string, tone: "neutral" | "ok" | "error" = "neutral") {
  const status = el("sharedArchiveStatus");
  status.className = `archive-import-status archive-status-${tone}`;
  status.innerHTML = `<i class="fa-solid ${tone === "error" ? "fa-circle-exclamation" : "fa-shield"}" aria-hidden="true"></i><span>${escapeHtml(message)}</span>`;
}

function sourceTypeLabel(sourceType: string) {
  return sourceType === "assignment" ? "과제" : "보드";
}

function primaryCount(archive: ArchiveSummary) {
  return Number(archive.contentCount ?? archive.recordCount ?? 0) || 0;
}

function primaryCountLabel(archive: ArchiveSummary) {
  return archive.sourceType === "assignment" ? `제출 ${primaryCount(archive)}건` : `게시글 ${primaryCount(archive)}건`;
}

function payloadObject(payload: unknown): Record<string, unknown> {
  return payload && typeof payload === "object" && !Array.isArray(payload) ? payload as Record<string, unknown> : {};
}

function firstText(payload: Record<string, unknown>, keys: string[]) {
  for (const key of keys) {
    const value = payload[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
}

function firstNumber(payload: Record<string, unknown>, keys: string[]) {
  for (const key of keys) {
    const value = Number(payload[key] || 0) || 0;
    if (value) return value;
  }
  return 0;
}

function recordLabel(record: ArchiveRecord, index: number) {
  const payload = payloadObject(record.payload);
  const name = firstText(payload, ["student_name_snapshot", "author_display_name", "student_name", "display_name", "name"]);
  const number = firstNumber(payload, ["class_no", "student_number", "student_no"]);
  if (name) return number ? `${number}번 ${name}` : name;
  if (record.type === "assignment") return "과제 안내";
  if (record.type === "board") return "게시판 안내";
  return `보관 기록 ${index + 1}`;
}

function recordPresentation(record: ArchiveRecord, index: number): RecordPresentation {
  const payload = payloadObject(record.payload);
  const body = firstText(payload, ["note", "content", "description_plain", "tasks_plain", "teacher_note", "value_text"])
    || "이 기록에는 별도의 본문이 없습니다.";
  return {
    ordinal: record.ordinal,
    label: recordLabel(record, index),
    time: shortDateText(firstNumber(payload, ["student_submitted_at", "submitted_at", "created_at", "updated_at"])),
    title: firstText(payload, ["title", "assignment_title_snapshot", "subject", "label"]) || "보관된 내용",
    body,
    feedback: firstText(payload, ["teacher_feedback", "feedback", "moderation_reason"]),
  };
}

function presentableRecords(archive: ArchiveDetail) {
  const preferred = archive.meta.sourceType === "assignment"
    ? new Set(["assignment_submission"])
    : new Set(["board_post"]);
  const preferredRecords = archive.records.filter((record) => preferred.has(record.type));
  const source = preferredRecords.length
    ? preferredRecords
    : archive.records.filter((record) => !record.type.includes("file_snapshot") && !/(target|reader|writer|reaction)$/u.test(record.type));
  return source.map(recordPresentation);
}

function fileIcon(file: ArchiveFile) {
  const name = file.originalName.toLowerCase();
  const type = file.contentType.toLowerCase();
  if (type.includes("pdf") || name.endsWith(".pdf")) return "fa-file-pdf";
  if (type.startsWith("image/") || /\.(png|jpe?g|gif|webp)$/u.test(name)) return "fa-file-image";
  if (type.includes("presentation") || /\.(pptx?|odp)$/u.test(name)) return "fa-file-powerpoint";
  return "fa-file";
}

function renderArchiveList() {
  const list = el("sharedArchiveList");
  el("sharedArchiveListStatus").textContent = busy ? "확인 중" : `${archives.length}개 · 무결성 확인됨`;
  if (!archives.length) {
    list.innerHTML = '<p class="data-empty">아직 이 PC에 보관한 공유자료가 없습니다.<br>웹에서 과제·보드 보관 코드를 발급해 내려받아 보세요.</p>';
    return;
  }
  list.innerHTML = archives.map((archive) => {
    const selected = archive.id === selectedArchiveId;
    return `<button class="archive-row${selected ? " is-selected" : ""}" type="button" role="radio" aria-checked="${selected}" data-archive-id="${escapeHtml(archive.id)}">
      <i class="fa-solid ${selected ? "fa-circle-dot" : "fa-circle"} archive-row__select" aria-hidden="true"></i>
      <i class="fa-solid fa-file-lines archive-row__icon" aria-hidden="true"></i>
      <span class="archive-row__main"><span class="archive-row__title"><strong>${escapeHtml(archive.title)}</strong><span class="archive-kind-badge${archive.sourceType === "board" ? " is-board" : ""}">${sourceTypeLabel(archive.sourceType)}</span></span><span class="archive-row__meta">${primaryCountLabel(archive)} · 파일 ${archive.fileCount}개 · ${escapeHtml(byteText(archive.totalFileBytes))}</span></span>
      <span class="archive-row__time">${escapeHtml(shortDateText(archive.importedAt))}</span>
    </button>`;
  }).join("");
}

function renderDetailHeading() {
  const heading = el("sharedArchiveDetailHeading");
  if (!detail) {
    heading.innerHTML = '<h2 id="sharedArchiveDetailTitle">보관 자료를 선택하세요</h2><p>읽기 전용 내용과 첨부파일을 이 PC에서 확인할 수 있습니다.</p>';
    return;
  }
  heading.innerHTML = `<h2 id="sharedArchiveDetailTitle"><span>${escapeHtml(detail.meta.title)}</span><span class="archive-kind-badge${detail.meta.sourceType === "board" ? " is-board" : ""}">${sourceTypeLabel(detail.meta.sourceType)}</span><span class="archive-readonly-badge">읽기 전용</span><span class="archive-verified-badge">무결성 확인됨</span></h2><p>${escapeHtml(dateText(detail.meta.importedAt))} 보관 · 인터넷 없이 열람 가능</p>`;
}

function renderDetail() {
  renderDetailHeading();
  const detailNode = el("sharedArchiveDetail");
  if (!detail) {
    detailNode.innerHTML = '<p class="data-empty">왼쪽에서 보관 자료를 선택하면 사람이 읽는 내용과 로컬 첨부파일을 바로 확인할 수 있습니다.</p>';
    return;
  }
  const records = presentableRecords(detail);
  if (!records.some((record) => record.ordinal === selectedRecordOrdinal)) selectedRecordOrdinal = records[0]?.ordinal ?? -1;
  const selected = records.find((record) => record.ordinal === selectedRecordOrdinal) || null;
  const recordList = records.length ? records.map((record) => `<button class="archive-record-row${record.ordinal === selectedRecordOrdinal ? " is-selected" : ""}" type="button" data-archive-record="${record.ordinal}" aria-pressed="${record.ordinal === selectedRecordOrdinal}"><strong>${escapeHtml(record.label)}</strong><span>${escapeHtml(record.time)}</span></button>`).join("") : '<p class="data-empty">보관된 원문 기록이 없습니다.</p>';
  const body = selected?.body.split(/\n+/u).filter(Boolean).map((line) => `<p>${escapeHtml(line)}</p>`).join("") || '<p>선택한 기록에 표시할 본문이 없습니다.</p>';
  const files = detail.files.length ? detail.files.map((file) => `<button type="button" class="archive-file-row" data-archive-file="${file.ordinal}"><i class="fa-solid ${fileIcon(file)}" aria-hidden="true"></i><span>${escapeHtml(file.originalName)}</span><small>${escapeHtml(byteText(file.byteSize))}</small><b>열기</b></button>`).join("") : '<p class="data-empty">첨부파일이 없습니다.</p>';
  detailNode.innerHTML = `<dl class="archive-summary-grid"><div><i class="fa-solid fa-user" aria-hidden="true"></i><dt>${detail.meta.sourceType === "assignment" ? "제출" : "게시글"}</dt><dd>${primaryCount(detail.meta)}건</dd></div><div><i class="fa-solid fa-paperclip" aria-hidden="true"></i><dt>첨부파일</dt><dd>${detail.meta.fileCount}개</dd></div><div><i class="fa-solid fa-hard-drive" aria-hidden="true"></i><dt>용량</dt><dd>${escapeHtml(byteText(detail.meta.totalFileBytes))}</dd></div></dl>
    <div class="archive-content-grid"><section class="archive-record-list-panel"><h3>${detail.meta.sourceType === "assignment" ? "제출 목록" : "게시글 목록"}</h3><div class="archive-record-list">${recordList}</div></section><section class="archive-record-main"><article class="archive-record-content"><h3>보관 내용</h3>${selected ? `<div class="archive-record-eyebrow"><strong>${escapeHtml(selected.label)}</strong><span>${escapeHtml(selected.time)}</span></div><h4>${escapeHtml(selected.title)}</h4><div class="archive-record-body">${body}</div>${selected.feedback ? `<p class="archive-record-feedback"><strong>교사 의견</strong><br>${escapeHtml(selected.feedback)}</p>` : ""}` : '<p class="data-empty">보관된 원문 기록이 없습니다.</p>'}</article><section class="archive-files"><h3>첨부파일</h3><div class="archive-file-list">${files}</div></section></section></div>`;
}

function render() {
  el<HTMLButtonElement>("sharedArchiveImportButton").disabled = busy;
  el<HTMLButtonElement>("sharedArchiveRefreshButton").disabled = busy;
  el<HTMLButtonElement>("sharedArchiveExportButton").disabled = busy || !selectedArchiveId;
  renderArchiveList();
  renderDetail();
}

async function loadArchives(options: { announce?: boolean } = {}) {
  busy = true;
  render();
  let loadFirst = false;
  try {
    const result = await activeBridge.list();
    if (!result.ok) throw new Error(result.error);
    archives = result.archives || [];
    if (selectedArchiveId && !archives.some((item) => item.id === selectedArchiveId)) {
      selectedArchiveId = "";
      selectedRecordOrdinal = -1;
      detail = null;
    }
    if (!selectedArchiveId && archives[0]) {
      selectedArchiveId = archives[0].id;
      loadFirst = true;
    }
    if (options.announce !== false) setStatus(`${archives.length}개의 로컬 보관본을 확인했습니다.`, "ok");
  } catch (error) {
    setStatus(errorText((error as Error).message), "error");
  } finally {
    busy = false;
    render();
  }
  if (loadFirst) {
    await selectArchive(selectedArchiveId).catch((error) => setStatus(errorText((error as Error).message), "error"));
  }
}

async function selectArchive(id: string) {
  selectedArchiveId = id;
  selectedRecordOrdinal = -1;
  detail = null;
  render();
  const result = await activeBridge.detail(id);
  if (!result.ok || !result.archive) throw new Error(result.error);
  detail = result.archive;
  render();
}

async function importArchive() {
  const input = el<HTMLInputElement>("sharedArchiveCodeInput");
  const code = input.value.trim();
  if (code.length !== 43 || !/^[A-Za-z0-9_-]+$/u.test(code)) {
    setStatus(errorText("archive_code_invalid"), "error");
    return;
  }
  busy = true;
  render();
  setStatus("목록과 첨부파일의 SHA-256을 확인하며 내려받는 중입니다.");
  try {
    const result = await activeBridge.import(code);
    if (!result.ok) throw new Error(result.error);
    input.value = "";
    selectedArchiveId = result.archiveId || "";
    selectedRecordOrdinal = -1;
    setStatus(`“${result.title || "공유자료"}” 보관을 완료했습니다. 서버 첨부파일은 72시간 유예 후 삭제됩니다.`, "ok");
    await loadArchives({ announce: false });
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
  const title = archives.find((item) => item.id === selectedArchiveId)?.title || "공유자료";
  const result = await activeBridge.exportArchive(selectedArchiveId, title);
  setStatus(result.ok ? "읽기 전용 보관본을 JSON으로 내보냈습니다." : errorText(result.error), result.ok ? "ok" : result.error === "archive_export_cancelled" ? "neutral" : "error");
}

export function initSharedArchive(options: { bridge?: ArchiveBridge } = {}) {
  activeBridge = options.bridge || tauriBridge;
  archives = [];
  selectedArchiveId = "";
  selectedRecordOrdinal = -1;
  detail = null;
  busy = false;
  el<HTMLButtonElement>("sharedArchiveImportButton").addEventListener("click", () => void importArchive());
  el<HTMLInputElement>("sharedArchiveCodeInput").addEventListener("keydown", (event) => {
    if (event.key === "Enter") void importArchive();
  });
  el<HTMLButtonElement>("sharedArchiveRefreshButton").addEventListener("click", () => void loadArchives());
  el<HTMLButtonElement>("sharedArchiveExportButton").addEventListener("click", () => void exportArchive());
  el("sharedArchiveList").addEventListener("click", (event) => {
    const row = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-archive-id]");
    if (row) void selectArchive(row.dataset.archiveId || "").catch((error) => setStatus(errorText((error as Error).message), "error"));
  });
  el("sharedArchiveDetail").addEventListener("click", (event) => {
    const target = event.target as HTMLElement;
    const recordRow = target.closest<HTMLButtonElement>("[data-archive-record]");
    if (recordRow) {
      selectedRecordOrdinal = Number(recordRow.dataset.archiveRecord ?? -1);
      renderDetail();
      return;
    }
    const fileRow = target.closest<HTMLButtonElement>("[data-archive-file]");
    if (!fileRow || !selectedArchiveId) return;
    void activeBridge.openFile(selectedArchiveId, Number(fileRow.dataset.archiveFile))
      .then((result) => { if (!result.ok) setStatus(errorText(result.error), "error"); });
  });
  render();
  void loadArchives({ announce: false });
}
