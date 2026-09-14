import { invoke } from "@tauri-apps/api/core";
import { escapeHtml, formatDate } from "./data-explorer";
import { cleanupReadbackMatches, cleanupSelection, duplicateErrorMessage, type CleanupEntry, type CleanupRequest, type DuplicateGroup, type DuplicateScan } from "./record-duplicates-model";
import { initDuplicatesTutorial } from "./record-duplicates-tutorial";
import "./record-duplicates.css";

type Options = {
  getTenantId: () => string;
  getSelectedStudent: () => { studentId: string; studentName: string } | undefined;
  onChanged: () => Promise<unknown>;
};
type CommandResult = { ok?: boolean; error?: string };

export function initRecordDuplicates(options: Options) {
  const dialog = document.createElement("dialog");
  dialog.id = "recordDuplicatesDialog";
  dialog.className = "record-duplicates-dialog";
  dialog.setAttribute("aria-labelledby", "recordDuplicatesTitle");
  dialog.innerHTML = `
    <header class="duplicates-heading"><div><small>로컬 자료함</small><h2 id="recordDuplicatesTitle">중복 자료 정리</h2><p id="recordDuplicatesTenant"></p></div><div class="duplicates-heading-actions"><button id="recordDuplicatesHelp" type="button">사용 안내</button><button id="recordDuplicatesClose" type="button" aria-label="중복 자료 정리 닫기">닫기</button></div></header>
    <div class="duplicates-content">
      <p class="duplicates-policy">같은 학생의 기록일·본문·맥락이 정확히 같은 <strong>관찰 기록</strong>을 찾습니다. 상담·평가·업무자료는 현재 검색 대상에 포함되지 않습니다.</p>
      <nav class="duplicates-tabs" aria-label="중복 자료 정리 화면"><button id="recordDuplicatesReviewTab" type="button" aria-pressed="true">중복 검색</button><button id="recordDuplicatesHistoryTab" type="button" aria-pressed="false">정리 내역·되돌리기</button></nav>
      <section id="recordDuplicatesReview" aria-label="중복 검색과 검토">
        <div class="duplicates-scan-bar"><label for="recordDuplicatesScope"><span>검색 범위 · 전체 기간</span><select id="recordDuplicatesScope"><option value="">현재 학급 전체 학생</option></select></label><button id="recordDuplicatesScan" class="duplicates-primary" type="button">중복 검색</button></div>
        <div class="duplicates-summary" id="recordDuplicatesSummary" tabindex="-1"><strong id="recordDuplicatesSummaryText">중복 자료를 검색해 주세요.</strong><span>검색만으로는 자료가 바뀌지 않습니다.</span></div>
        <div id="recordDuplicatesGroups" class="duplicates-groups"></div>
        <footer class="duplicates-apply-bar"><p>대표 기록 한 건을 남기고 선택한 중복을 보관합니다.<br>원문과 이력을 유지하며 정리 내역에서 되돌릴 수 있습니다.</p><button id="recordDuplicatesApply" class="duplicates-primary" type="button" disabled>선택한 중복 보관</button></footer>
      </section>
      <section id="recordDuplicatesHistory" aria-label="정리 내역" hidden><div class="duplicates-history-heading"><h3>이 학급의 정리 내역</h3><button id="recordDuplicatesHistoryRefresh" type="button">내역 새로고침</button></div><p>최근 정리 50회를 보여줍니다. 정리 후 자료가 바뀐 경우에는 자동으로 되돌리지 않습니다. 내역은 로컬 DB에 보존됩니다.</p><div id="recordDuplicatesHistoryList"></div></section>
      <p id="recordDuplicatesStatus" class="duplicates-status" role="status" aria-live="polite"></p>
    </div>
    <aside id="recordDuplicatesTutorial" class="duplicates-tutorial" aria-label="중복 자료 정리 안내" hidden><div><small data-tutorial-step></small><button data-tutorial-close type="button" aria-label="중복 정리 안내 닫기">닫기</button></div><h3></h3><p></p><footer><button data-tutorial-previous type="button">이전</button><button data-tutorial-next type="button">다음</button></footer></aside>`;
  document.body.append(dialog);
  const el = <T extends HTMLElement>(id: string) => dialog.querySelector<T>(`#${id}`)!;
  const scope = el<HTMLSelectElement>("recordDuplicatesScope");
  const applyButton = el<HTMLButtonElement>("recordDuplicatesApply");
  const tutorial = initDuplicatesTutorial(dialog, () => showTab("review"));
  let tenantId = "";
  let epoch = 0;
  let busy = false;
  let scan: DuplicateScan | null = null;
  let selected = new Set<string>();
  let history: CleanupEntry[] = [];
  let pending: { request: CleanupRequest; ids: string[] } | null = null;
  let tenantTimer = 0;
  let returnFocus: HTMLElement | null = null;

  async function command<T extends CommandResult>(name: string, input: object): Promise<T> {
    const result = await invoke<T>(name, { input });
    if (!result || result.ok === false) throw new Error(result?.error || "local_duplicate_command_failed");
    return result;
  }
  function current(token: number) { return dialog.open && token === epoch && tenantId === options.getTenantId().trim(); }
  function validTenant() {
    if (tenantId && tenantId !== options.getTenantId().trim()) invalidateTenant();
    return Boolean(tenantId);
  }
  function status(text: string, tone = "") { el("recordDuplicatesStatus").textContent = text; el("recordDuplicatesStatus").dataset.tone = tone; }
  function updateControls() {
    const selection = cleanupSelection(scan, selected);
    const validTenant = Boolean(tenantId) && tenantId === options.getTenantId().trim();
    scope.disabled = busy || Boolean(pending);
    el<HTMLButtonElement>("recordDuplicatesScan").disabled = busy || !validTenant || Boolean(pending);
    el<HTMLButtonElement>("recordDuplicatesClose").disabled = busy;
    el<HTMLButtonElement>("recordDuplicatesHelp").disabled = busy;
    el<HTMLButtonElement>("recordDuplicatesHistoryRefresh").disabled = busy || !validTenant;
    applyButton.disabled = busy || !validTenant || (!pending && !selection.valid);
    applyButton.textContent = pending ? "같은 정리 결과 다시 확인" : selection.count ? `선택한 중복 ${selection.count}건 보관` : "선택한 중복 보관";
    dialog.querySelectorAll<HTMLInputElement>("[data-duplicate-select]").forEach(input => { input.disabled = busy || Boolean(pending); });
    dialog.querySelectorAll<HTMLButtonElement>("[data-duplicate-undo]").forEach(button => { button.disabled = busy || !validTenant || !history.find(entry => entry.cleanupId === button.dataset.duplicateUndo)?.canUndo; });
    el("recordDuplicatesReview").setAttribute("aria-busy", String(busy));
    el("recordDuplicatesHistory").setAttribute("aria-busy", String(busy));
  }
  function showTab(tab: "review" | "history") {
    el("recordDuplicatesReview").hidden = tab !== "review";
    el("recordDuplicatesHistory").hidden = tab !== "history";
    el("recordDuplicatesReviewTab").setAttribute("aria-pressed", String(tab === "review"));
    el("recordDuplicatesHistoryTab").setAttribute("aria-pressed", String(tab === "history"));
  }
  function renderGroup(group: DuplicateGroup) {
    const canApply = group.canApply && group.archiveIds.length > 0;
    return `<article class="duplicates-group${canApply ? "" : " is-protected"}">
      <header><label>${canApply ? `<input type="checkbox" data-duplicate-select="${escapeHtml(group.groupId)}"${selected.has(group.groupId) ? " checked" : ""}>` : '<span aria-hidden="true">▣</span>'}<strong>${escapeHtml(group.studentName || "이름 미확인")}</strong><span>${escapeHtml(group.date)} · 관찰 ${group.records.length}건</span></label><b>${canApply ? `${group.archiveIds.length}건 보관 대상` : "자동 정리 제외"}</b></header>
      <p class="duplicates-match">${escapeHtml(group.matchReason)}</p>
      <p class="duplicates-body">${escapeHtml(group.body)}</p>
      <ul class="duplicates-records">${group.records.map(record => `<li><span class="duplicates-record-role${record.docId === group.keeperId ? " is-keeper" : ""}">${record.docId === group.keeperId ? "남길 기록" : canApply && group.archiveIds.includes(record.docId) ? "보관 대상" : "보호된 기록"}</span><span>${escapeHtml(formatDate(record.savedAtMs))}${record.referenced ? " · 학생기록 근거로 사용 중" : ""}</span><small>식별번호 ${escapeHtml(record.docId)}</small><details><summary>이 기록 원문 보기</summary><p>${escapeHtml(record.body)}</p></details></li>`).join("")}</ul>
      <p class="duplicates-differences">저장 시간과 식별번호가 달라도 원문·첨부·기록 맥락이 같을 때만 정리합니다.</p>
      ${group.blockedReasons.length ? `<p class="duplicates-blocked">${group.blockedReasons.map(escapeHtml).join(" · ")}</p>` : ""}
    </article>`;
  }
  function renderScan() {
    const groups = scan?.groups || [];
    const selection = cleanupSelection(scan, selected);
    el("recordDuplicatesSummaryText").textContent = scan
      ? groups.length ? `관찰 ${scan.scannedCount}건에서 중복 ${groups.length}묶음 · 보관 선택 ${selection.count}건` : `관찰 ${scan.scannedCount}건 확인 · 정확히 같은 중복이 없습니다.`
      : "중복 자료를 검색해 주세요.";
    el("recordDuplicatesGroups").innerHTML = groups.map(renderGroup).join("");
    if (scan && selection.count > scan.maxArchiveCount) status(`한 번에 ${scan.maxArchiveCount}건까지 정리할 수 있습니다. 일부 묶음의 선택을 해제해 주세요.`, "warning");
    updateControls();
  }
  function renderHistory() {
    el("recordDuplicatesHistoryList").innerHTML = history.length ? history.map(entry => `
      <article class="duplicates-history-entry"><header><div><strong>${escapeHtml(formatDate(entry.createdAtMs))}</strong><p>${entry.archivedCount}건 · ${entry.state === "restored" ? "되돌리기 완료" : entry.state === "changed" ? "정리 이후 자료 변경됨" : "보관됨"}</p></div><button type="button" data-duplicate-undo="${escapeHtml(entry.cleanupId)}"${entry.canUndo ? "" : " disabled"}>이 정리 되돌리기</button></header>
      <details><summary>정리한 기록 ${entry.records.length}건 보기</summary><ul>${entry.records.map(record => `<li><strong>${escapeHtml(record.studentName || "이름 미확인")} · ${escapeHtml(record.date)}</strong><p>${escapeHtml(record.body)}</p><small>식별번호 ${escapeHtml(record.docId)}</small></li>`).join("")}</ul></details>
      ${entry.blockedReason ? `<p class="duplicates-blocked">${escapeHtml(entry.blockedReason)}</p>` : ""}</article>`).join("") : '<p class="duplicates-empty">아직 중복 정리 내역이 없습니다.</p>';
    updateControls();
  }
  async function readHistory(token: number) {
    if (!current(token)) return null;
    const result = await command<CommandResult & { entries: CleanupEntry[] }>("list_record_duplicate_history", { tenantId });
    if (!current(token)) return null;
    history = result.entries;
    renderHistory();
    return history;
  }
  async function loadHistory() {
    if (busy || !validTenant()) return;
    const token = epoch;
    busy = true; updateControls(); status("정리 내역을 불러오는 중입니다.");
    try { if (await readHistory(token)) status("로컬 DB에서 정리 내역을 확인했습니다."); }
    catch (error) { if (current(token)) { history = []; el("recordDuplicatesHistoryList").innerHTML = ""; status(duplicateErrorMessage(error), "error"); } }
    finally { if (current(token)) { busy = false; updateControls(); } }
  }
  async function search() {
    if (busy || pending || !validTenant()) return;
    const token = epoch;
    busy = true; scan = null; selected.clear(); renderScan(); status("전체 기간의 관찰 기록에서 중복을 검색하는 중입니다.");
    try {
      const result = await command<DuplicateScan>("scan_record_duplicates", { tenantId, ...(scope.value ? { studentId: scope.value } : {}) });
      if (!current(token)) return;
      if (result.tenantId !== tenantId) throw new Error("duplicate_tenant_mismatch");
      scan = result; selected = new Set(result.groups.filter(group => group.canApply).map(group => group.groupId));
      renderScan(); status(result.groups.length ? "원문과 남길 기록을 확인한 뒤 보관할 묶음을 선택해 주세요." : "검색을 마쳤습니다. 변경된 자료는 없습니다.");
      if (cleanupSelection(scan, selected).count > scan.maxArchiveCount) status(`한 번에 ${scan.maxArchiveCount}건까지 정리할 수 있습니다. 일부 묶음의 선택을 해제해 주세요.`, "warning");
    } catch (error) { if (current(token)) { scan = null; renderScan(); status(duplicateErrorMessage(error), "error"); } }
    finally { if (current(token)) { busy = false; updateControls(); } }
  }
  async function apply() {
    if (busy || !validTenant()) return;
    const selection = cleanupSelection(scan, selected);
    if (!pending && !selection.valid) return;
    if (!pending) pending = { request: { tenantId, cleanupId: crypto.randomUUID(), groups: selection.groups.map(group => ({ groupId: group.groupId, snapshotHash: group.snapshotHash })) }, ids: selection.groups.flatMap(group => group.archiveIds) };
    const operation = pending;
    const token = epoch;
    busy = true; updateControls(); status("검토한 중복을 보관하고 실제 저장 결과를 다시 확인하는 중입니다.");
    try {
      await command("apply_record_duplicates", operation.request);
      const entries = await readHistory(token);
      if (!current(token) || !entries) return;
      if (!cleanupReadbackMatches(entries.find(entry => entry.cleanupId === operation.request.cleanupId), operation.ids, "archived")) throw new Error("duplicate_readback_failed");
      pending = null; scan = null; selected.clear(); renderScan(); showTab("history");
      status(`중복 ${operation.ids.length}건을 보관하고 로컬 DB 재조회를 확인했습니다. 정리 내역에서 되돌릴 수 있습니다.`, "success");
      await options.onChanged().catch(() => { if (current(token)) status("보관 결과를 확인했습니다. 자료 목록이 갱신되지 않아 목록 새로고침이 필요합니다.", "warning"); });
    } catch (error) {
      if (!current(token)) return;
      // A failed response may follow a committed write. Reconcile this exact cleanup before allowing another one.
      try {
        const entries = await readHistory(token);
        if (!current(token) || !entries) return;
        if (cleanupReadbackMatches(entries.find(entry => entry.cleanupId === operation.request.cleanupId), operation.ids, "archived")) {
          pending = null; scan = null; selected.clear(); renderScan(); showTab("history");
          status(`중복 ${operation.ids.length}건의 보관 결과를 로컬 DB에서 확인했습니다.`, "success");
          await options.onChanged().catch(() => { if (current(token)) status("보관 결과를 확인했습니다. 자료 목록이 갱신되지 않아 목록 새로고침이 필요합니다.", "warning"); });
          return;
        }
        if (/stale|conflict|changed|revision|snapshot|reference|limit|authority|integrity|recovery_required/i.test(String(error))) { pending = null; scan = null; selected.clear(); renderScan(); }
      } catch { /* Keep the same cleanup request available for an idempotent retry. */ }
      if (current(token)) status(`${duplicateErrorMessage(error)}${pending ? " ‘같은 정리 결과 다시 확인’으로 이어서 확인할 수 있습니다." : ""}`, "error");
    } finally { if (current(token)) { busy = false; updateControls(); } }
  }
  async function undo(cleanupId: string) {
    if (busy || !validTenant()) return;
    const entry = history.find(item => item.cleanupId === cleanupId);
    if (!entry?.canUndo) return;
    const token = epoch;
    const ids = entry.records.map(record => record.docId);
    busy = true; updateControls(); status("정리한 기록을 복원하고 실제 저장 결과를 다시 확인하는 중입니다.");
    try {
      await command("undo_record_duplicate_cleanup", { tenantId, cleanupId });
      const entries = await readHistory(token);
      if (!current(token) || !entries) return;
      if (!cleanupReadbackMatches(entries.find(item => item.cleanupId === cleanupId), ids, "restored")) throw new Error("duplicate_readback_failed");
      scan = null; selected.clear(); renderScan(); status(`${ids.length}건의 되돌리기를 로컬 DB 재조회로 확인했습니다.`, "success");
      await options.onChanged().catch(() => { if (current(token)) status("되돌리기 결과를 확인했습니다. 자료 목록이 갱신되지 않아 목록 새로고침이 필요합니다.", "warning"); });
    } catch (error) {
      if (!current(token)) return;
      const entries = await readHistory(token).catch(() => null);
      if (!current(token)) return;
      if (entries && cleanupReadbackMatches(entries.find(item => item.cleanupId === cleanupId), ids, "restored")) {
        scan = null; selected.clear(); renderScan(); status(`${ids.length}건의 되돌리기를 로컬 DB 재조회로 확인했습니다.`, "success");
        await options.onChanged().catch(() => { if (current(token)) status("되돌리기 결과를 확인했습니다. 자료 목록이 갱신되지 않아 목록 새로고침이 필요합니다.", "warning"); });
      } else status(`${duplicateErrorMessage(error)} 정리 내역을 새로고침해 현재 상태를 확인해 주세요.`, "error");
    } finally { if (current(token)) { busy = false; updateControls(); } }
  }
  function invalidateTenant() {
    epoch += 1; tenantId = ""; busy = false; pending = null; scan = null; selected.clear(); history = [];
    tutorial.close(); renderScan(); renderHistory(); status("학급 연결이 바뀌었습니다. 이 창을 닫고 현재 학급에서 다시 열어 주세요.", "warning");
  }
  async function open(studentOnly = false) {
    if (dialog.open) return;
    returnFocus = document.activeElement as HTMLElement | null;
    epoch += 1; tenantId = options.getTenantId().trim(); busy = false; pending = null; scan = null; selected.clear(); history = [];
    const student = studentOnly ? options.getSelectedStudent() : undefined;
    scope.innerHTML = `<option value="">현재 학급 전체 학생</option>${student ? `<option value="${escapeHtml(student.studentId)}">${escapeHtml(student.studentName || "선택 학생")} 학생만</option>` : ""}`;
    scope.value = student?.studentId || "";
    el("recordDuplicatesTenant").textContent = document.getElementById("homeTenantLabel")?.textContent || "현재 학급";
    showTab("review"); renderScan(); renderHistory(); status(tenantId ? "중복 검색은 읽기 전용입니다. 전체 기간의 관찰을 확인합니다." : "설정에서 교사 로그인으로 학급을 먼저 연결해 주세요.", tenantId ? "" : "warning");
    dialog.showModal();
    tenantTimer = window.setInterval(() => { if (tenantId && options.getTenantId().trim() !== tenantId) invalidateTenant(); }, 300);
    tutorial.offer();
  }
  el("recordDuplicatesScan").addEventListener("click", () => void search());
  el("recordDuplicatesApply").addEventListener("click", () => void apply());
  el("recordDuplicatesClose").addEventListener("click", () => { if (!busy) dialog.close(); });
  el("recordDuplicatesReviewTab").addEventListener("click", () => showTab("review"));
  el("recordDuplicatesHistoryTab").addEventListener("click", () => { showTab("history"); void loadHistory(); });
  el("recordDuplicatesHistoryRefresh").addEventListener("click", () => void loadHistory());
  el("recordDuplicatesGroups").addEventListener("change", event => {
    const checkbox = (event.target as HTMLElement).closest<HTMLInputElement>("[data-duplicate-select]");
    if (!checkbox || busy || pending) return;
    const id = checkbox.dataset.duplicateSelect!;
    if (checkbox.checked) selected.add(id); else selected.delete(id);
    renderScan();
  });
  el("recordDuplicatesHistoryList").addEventListener("click", event => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-duplicate-undo]");
    if (button) void undo(button.dataset.duplicateUndo!);
  });
  scope.addEventListener("change", () => { if (busy) return; epoch += 1; scan = null; selected.clear(); renderScan(); status("검색 범위가 바뀌었습니다. 중복을 다시 검색해 주세요."); });
  dialog.addEventListener("cancel", event => { if (busy) event.preventDefault(); });
  dialog.addEventListener("close", () => { epoch += 1; tutorial.close(); clearInterval(tenantTimer); pending = null; scan = null; history = []; returnFocus?.focus(); });
  document.getElementById("dataExplorerDuplicates")?.addEventListener("click", () => void open());
  document.getElementById("studentTimelineDuplicates")?.addEventListener("click", () => void open(true));
  return { open };
}
