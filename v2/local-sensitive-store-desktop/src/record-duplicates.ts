import { invoke } from "@tauri-apps/api/core";
import { escapeHtml, formatDate } from "./data-explorer";
import { cleanupReadbackMatches, cleanupSelection, duplicateErrorMessage, type CleanupEntry, type CleanupRequest, type DuplicateGroup, type DuplicateScan } from "./record-duplicates-model";
import { initDuplicatesTutorial } from "./record-duplicates-tutorial";
import "./record-duplicates.css";
import { isDeskRestoreBlocked } from "./desk-restore-lock";
import { isStudentPrivacyEnabled, createStudentPrivacyToggle } from "./desk-privacy";

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
        <div class="duplicates-workspace"><section id="recordDuplicatesGroups" class="duplicates-groups" aria-label="중복 묶음 목록"></section><article id="recordDuplicatesGroupDetail" class="duplicates-group-detail" aria-label="선택 묶음 원문 비교"></article></div>
        <footer class="duplicates-apply-bar"><p>대표 기록 한 건을 남기고 선택한 중복을 보관합니다.<br>원문과 이력을 유지하며 정리 내역에서 되돌릴 수 있습니다.</p><button id="recordDuplicatesApply" class="duplicates-primary" type="button" disabled>선택한 중복 보관</button></footer>
        <section id="recordDuplicatesConfirm" class="duplicates-confirm" aria-label="중복 보관 최종 확인" hidden><div><strong id="recordDuplicatesConfirmTitle"></strong><p>실행 직전에 원문·수정 상태·근거 연결을 다시 확인합니다. 바뀐 자료가 있으면 중단합니다. 대표 기록과 원문·ID·증거 이력을 보존합니다.</p></div><button id="recordDuplicatesConfirmApply" class="duplicates-primary" type="button">보관 확인</button><button id="recordDuplicatesConfirmCancel" type="button">취소</button></section>
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
  dialog.querySelector(".duplicates-heading-actions")?.prepend(createStudentPrivacyToggle());
  let tenantId = "";
  let epoch = 0;
  let busy = false;
  let scan: DuplicateScan | null = null;
  let selected = new Set<string>();
  let history: CleanupEntry[] = [];
  let pending: { request: CleanupRequest; ids: string[] } | null = null;
  let tenantTimer = 0;
  let returnFocus: HTMLElement | null = null;
  let activeGroupId = "";
  let selectedStudent: { studentId: string; studentName: string } | undefined;

  async function command<T extends CommandResult>(name: string, input: object): Promise<T> {
    if (["apply_record_duplicates", "undo_record_duplicate_cleanup"].includes(name) && (isDeskRestoreBlocked() || isStudentPrivacyEnabled())) throw new Error("restore_or_privacy_blocked");
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
    scope.disabled = busy || Boolean(pending) || isDeskRestoreBlocked();
    el<HTMLButtonElement>("recordDuplicatesScan").disabled = busy || isDeskRestoreBlocked() || !validTenant || Boolean(pending);
    el<HTMLButtonElement>("recordDuplicatesClose").disabled = busy || isDeskRestoreBlocked();
    el<HTMLButtonElement>("recordDuplicatesHelp").disabled = busy;
    el<HTMLButtonElement>("recordDuplicatesHistoryRefresh").disabled = busy || !validTenant;
    applyButton.disabled = busy || isDeskRestoreBlocked() || isStudentPrivacyEnabled() || !validTenant || (!pending && !selection.valid);
    el<HTMLButtonElement>("recordDuplicatesConfirmApply").disabled = applyButton.disabled;
    applyButton.textContent = pending ? "같은 정리 결과 다시 확인" : selection.count ? `선택한 중복 ${selection.count}건 보관` : "선택한 중복 보관";
    dialog.querySelectorAll<HTMLInputElement>("[data-duplicate-select]").forEach(input => { input.disabled = busy || Boolean(pending) || isDeskRestoreBlocked() || isStudentPrivacyEnabled(); });
    dialog.querySelectorAll<HTMLButtonElement>("[data-duplicate-undo]").forEach(button => { button.disabled = busy || isDeskRestoreBlocked() || isStudentPrivacyEnabled() || !validTenant || !history.find(entry => entry.cleanupId === button.dataset.duplicateUndo)?.canUndo; });
    el("recordDuplicatesReview").setAttribute("aria-busy", String(busy));
    el("recordDuplicatesHistory").setAttribute("aria-busy", String(busy));
  }
  function showTab(tab: "review" | "history") {
    el("recordDuplicatesReview").hidden = tab !== "review";
    el("recordDuplicatesHistory").hidden = tab !== "history";
    el("recordDuplicatesReviewTab").setAttribute("aria-pressed", String(tab === "review"));
    el("recordDuplicatesHistoryTab").setAttribute("aria-pressed", String(tab === "history"));
  }
  function studentLabel(name: string) { return isStudentPrivacyEnabled() ? "학생 이름 가림" : name || "이름 미확인"; }
  function renderGroup(group: DuplicateGroup) {
    const canApply = group.canApply && group.archiveIds.length > 0;
    return `<article class="duplicates-group${canApply ? "" : " is-protected"}${activeGroupId === group.groupId ? " is-active" : ""}">
      <header><label>${canApply ? `<input type="checkbox" data-duplicate-select="${escapeHtml(group.groupId)}"${selected.has(group.groupId) ? " checked" : ""} aria-label="이 묶음의 중복 보관 선택">` : '<span aria-hidden="true">▣</span>'}<strong>${escapeHtml(studentLabel(group.studentName))}</strong></label><b>${canApply ? `${group.archiveIds.length}건 보관 대상` : "보호 묶음"}</b></header>
      <p>${escapeHtml(group.date)} · 관찰 ${group.records.length}건</p>
      ${group.blockedReasons.length ? '<p class="duplicates-blocked">학생기록 근거로 연결된 기록을 보호합니다.</p>' : ''}
      <button class="duplicates-group-open" type="button" data-duplicate-open="${escapeHtml(group.groupId)}" aria-pressed="${activeGroupId === group.groupId}">원문·대표 기록 비교 <span aria-hidden="true">›</span></button>
    </article>`;
  }
  function renderGroupDetail() {
    const group = scan?.groups.find(item => item.groupId === activeGroupId) || scan?.groups[0];
    if (!group) { el("recordDuplicatesGroupDetail").innerHTML = '<p class="duplicates-empty">묶음을 선택하면 남길 기록과 보관할 기록을 비교합니다.</p>'; return; }
    activeGroupId = group.groupId;
    const hidden = isStudentPrivacyEnabled();
    const canApply = group.canApply && group.archiveIds.length > 0;
    el("recordDuplicatesGroupDetail").innerHTML = `<h3>${escapeHtml(studentLabel(group.studentName))} · ${escapeHtml(group.date)}</h3><p class="duplicates-match">일치 근거: ${hidden ? "학생·기록일·수업·본문·첨부·맥락 일치" : escapeHtml(group.matchReason)}</p><p class="duplicates-body">${hidden ? "학생 기록 가림이 켜져 있습니다. 원문을 확인하려면 가림을 해제하세요." : escapeHtml(group.body)}</p>
      <div class="duplicates-comparison-wrap"><table class="duplicates-comparison"><thead><tr><th>구분</th><th>저장 시각</th><th>본문</th><th>조치</th></tr></thead><tbody>${group.records.map(record => {
        const keeper = record.docId === group.keeperId;
        const archive = canApply && group.archiveIds.includes(record.docId);
        return `<tr><td><span class="duplicates-record-role${keeper ? " is-keeper" : ""}">${keeper ? "대표로 남길 기록" : archive ? "보관할 중복" : "보호된 기록"}</span></td><td>${escapeHtml(formatDate(record.savedAtMs))}</td><td>${hidden ? "원문 가림" : keeper ? escapeHtml(record.body) : "본문 동일 ✓"}${record.referenced ? '<small>학생기록 근거로 사용 중</small>' : ''}</td><td>${keeper ? "유지" : archive ? "보관 대상" : "보호"}</td></tr>`;
      }).join("")}</tbody></table></div>
      ${hidden ? '' : `<details class="duplicates-identities"><summary>원문·식별번호 확인</summary>${group.records.map(record => `<article><small>식별번호 ${escapeHtml(record.docId)}</small><p>${escapeHtml(record.body)}</p></article>`).join("")}</details>`}
      <p class="duplicates-differences">저장 시간과 식별번호가 달라도 학생·기록일·수업·본문·첨부·맥락이 같은 경우에만 정리합니다.</p>${group.blockedReasons.length ? '<p class="duplicates-blocked">보호된 근거 또는 변경된 기록은 자동으로 정리하지 않습니다.</p>' : ''}`;
  }
  function renderScan() {
    const groups = scan?.groups || [];
    const selection = cleanupSelection(scan, selected);
    const scroll = el("recordDuplicatesGroups").scrollTop;
    if (groups.length && !groups.some(group => group.groupId === activeGroupId)) activeGroupId = groups[0].groupId;
    el("recordDuplicatesSummaryText").textContent = scan
      ? groups.length ? `관찰 ${scan.scannedCount}건 · 중복 ${groups.length}묶음 · 선택 ${selection.groups.length}묶음 · 보관 대상 ${selection.count}건 · 보호 ${groups.filter(group => !group.canApply).length}묶음` : `관찰 ${scan.scannedCount}건 확인 · 정확히 같은 중복이 없습니다.`
      : "중복 자료를 검색해 주세요.";
    el("recordDuplicatesGroups").innerHTML = groups.map(renderGroup).join("");
    el("recordDuplicatesGroups").scrollTop = scroll;
    renderGroupDetail();
    if (scan && selection.count > scan.maxArchiveCount) status(`한 번에 ${scan.maxArchiveCount}건까지 정리할 수 있습니다. 일부 묶음의 선택을 해제해 주세요.`, "warning");
    updateControls();
  }
  function renderHistory() {
    el("recordDuplicatesHistoryList").innerHTML = history.length ? history.map(entry => `
      <article class="duplicates-history-entry"><header><div><strong>${escapeHtml(formatDate(entry.createdAtMs))}</strong><p>${entry.archivedCount}건 · ${entry.state === "restored" ? "되돌리기 완료" : entry.state === "changed" ? "정리 이후 자료 변경됨" : "보관됨"}</p></div><button type="button" data-duplicate-undo="${escapeHtml(entry.cleanupId)}"${entry.canUndo ? "" : " disabled"}>이 정리 되돌리기</button></header>
      <details><summary>정리한 기록 ${entry.records.length}건 보기</summary><ul>${entry.records.map(record => `<li><strong>${escapeHtml(studentLabel(record.studentName))} · ${escapeHtml(record.date)}</strong><p>${isStudentPrivacyEnabled() ? "원문 가림" : escapeHtml(record.body)}</p>${isStudentPrivacyEnabled() ? "" : `<small>식별번호 ${escapeHtml(record.docId)}</small>`}</li>`).join("")}</ul></details>
      ${entry.blockedReason ? `<p class="duplicates-blocked">정리 이후 자료 또는 근거가 바뀌어 자동으로 되돌릴 수 없습니다.</p>` : ""}</article>`).join("") : '<p class="duplicates-empty">아직 중복 정리 내역이 없습니다.</p>';
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
    if (busy || pending || isDeskRestoreBlocked() || !validTenant()) return;
    const token = epoch;
    busy = true; scan = null; selected.clear(); renderScan(); status("전체 기간의 관찰 기록에서 중복을 검색하는 중입니다.");
    try {
      const result = await command<DuplicateScan>("scan_record_duplicates", { tenantId, ...(scope.value === "selected" && selectedStudent ? { studentId: selectedStudent.studentId } : {}) });
      if (!current(token)) return;
      if (result.tenantId !== tenantId) throw new Error("duplicate_tenant_mismatch");
      scan = result; selected = new Set(result.groups.filter(group => group.canApply).map(group => group.groupId));
      renderScan(); status(result.groups.length ? "원문과 남길 기록을 확인한 뒤 보관할 묶음을 선택해 주세요." : "검색을 마쳤습니다. 변경된 자료는 없습니다.");
      if (cleanupSelection(scan, selected).count > scan.maxArchiveCount) status(`한 번에 ${scan.maxArchiveCount}건까지 정리할 수 있습니다. 일부 묶음의 선택을 해제해 주세요.`, "warning");
    } catch (error) { if (current(token)) { scan = null; renderScan(); status(duplicateErrorMessage(error), "error"); } }
    finally { if (current(token)) { busy = false; updateControls(); } }
  }
  async function apply(confirmed = false) {
    if (busy || isDeskRestoreBlocked() || isStudentPrivacyEnabled() || !validTenant()) return;
    const selection = cleanupSelection(scan, selected);
    if (!pending && !selection.valid) return;
    if (!pending && !confirmed) { el("recordDuplicatesConfirmTitle").textContent = `선택한 중복 ${selection.count}건을 보관할까요?`; el("recordDuplicatesConfirm").hidden = false; el("recordDuplicatesConfirm").scrollIntoView({ block: "nearest" }); el("recordDuplicatesConfirmApply").focus(); return; }
    el("recordDuplicatesConfirm").hidden = true;
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
  async function undo(cleanupId: string, confirmed = false) {
    if (busy || isDeskRestoreBlocked() || isStudentPrivacyEnabled() || !validTenant()) return;
    const entry = history.find(item => item.cleanupId === cleanupId);
    if (!entry?.canUndo) return;
    if (!confirmed) {
      dialog.querySelector(".duplicates-undo-confirm")?.remove();
      const confirmation = document.createElement("section");
      confirmation.className = "duplicates-confirm duplicates-undo-confirm";
      confirmation.innerHTML = `<div><strong>정리한 ${entry.archivedCount}건을 되돌릴까요?</strong><p>실행 직전에 현재 원문과 근거를 다시 검증합니다. 정리 이후 바뀐 자료는 덮어쓰지 않습니다.</p></div><button class="duplicates-primary" type="button" data-duplicate-undo-confirm="${escapeHtml(cleanupId)}">검증 후 되돌리기</button><button type="button" data-duplicate-undo-cancel>취소</button>`;
      el("recordDuplicatesHistory").append(confirmation);
      confirmation.scrollIntoView({ block: "nearest" });
      confirmation.querySelector<HTMLButtonElement>("[data-duplicate-undo-confirm]")?.focus();
      return;
    }
    dialog.querySelector(".duplicates-undo-confirm")?.remove();
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
    if (dialog.open || isDeskRestoreBlocked()) return;
    returnFocus = document.activeElement as HTMLElement | null;
    epoch += 1; tenantId = options.getTenantId().trim(); busy = false; pending = null; scan = null; selected.clear(); history = [];
    const student = studentOnly ? options.getSelectedStudent() : undefined;
    selectedStudent = student;
    scope.innerHTML = `<option value="">현재 학급 전체 학생</option>${student ? `<option value="selected">${escapeHtml(studentLabel(student.studentName))} 학생만</option>` : ""}`;
    scope.value = student ? "selected" : "";
    el("recordDuplicatesConfirm").hidden = true;
    el("recordDuplicatesTenant").textContent = document.getElementById("homeTenantLabel")?.textContent || "현재 학급";
    showTab("review"); renderScan(); renderHistory(); status(tenantId ? "중복 검색은 읽기 전용입니다. 전체 기간의 관찰을 확인합니다." : "설정에서 교사 로그인으로 학급을 먼저 연결해 주세요.", tenantId ? "" : "warning");
    dialog.showModal();
    tenantTimer = window.setInterval(() => { if (tenantId && options.getTenantId().trim() !== tenantId) invalidateTenant(); }, 300);
    tutorial.offer();
  }
  el("recordDuplicatesConfirmApply").addEventListener("click", () => void apply(true));
  el("recordDuplicatesConfirmCancel").addEventListener("click", () => { el("recordDuplicatesConfirm").hidden = true; applyButton.focus(); });
  el("recordDuplicatesGroups").addEventListener("click", event => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-duplicate-open]");
    if (button && !busy && !isDeskRestoreBlocked()) { activeGroupId = button.dataset.duplicateOpen || ""; renderScan(); }
  });
  window.addEventListener("desk:restore-lock-changed", updateControls);
  window.addEventListener("desk:student-privacy-changed", () => {
    dialog.querySelector(".duplicates-undo-confirm")?.remove();
    const value = scope.value; scope.innerHTML = `<option value="">현재 학급 전체 학생</option>${selectedStudent ? `<option value="selected">${escapeHtml(studentLabel(selectedStudent.studentName))} 학생만</option>` : ""}`; scope.value = value; el("recordDuplicatesConfirm").hidden = true; renderScan(); renderHistory();
  });
  el("recordDuplicatesScan").addEventListener("click", () => void search());
  el("recordDuplicatesApply").addEventListener("click", () => void apply());
  el("recordDuplicatesClose").addEventListener("click", () => { if (!busy && !isDeskRestoreBlocked()) dialog.close(); });
  el("recordDuplicatesReviewTab").addEventListener("click", () => showTab("review"));
  el("recordDuplicatesHistoryTab").addEventListener("click", () => { showTab("history"); void loadHistory(); });
  el("recordDuplicatesHistoryRefresh").addEventListener("click", () => void loadHistory());
  el("recordDuplicatesGroups").addEventListener("change", event => {
    const checkbox = (event.target as HTMLElement).closest<HTMLInputElement>("[data-duplicate-select]");
    if (!checkbox || busy || pending || isDeskRestoreBlocked() || isStudentPrivacyEnabled()) return;
    el("recordDuplicatesConfirm").hidden = true;
    const id = checkbox.dataset.duplicateSelect!;
    if (checkbox.checked) selected.add(id); else selected.delete(id);
    renderScan();
  });
  el("recordDuplicatesHistoryList").addEventListener("click", event => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-duplicate-undo]");
    if (button) void undo(button.dataset.duplicateUndo!);
  });
  el("recordDuplicatesHistory").addEventListener("click", event => {
    const target = event.target as HTMLElement;
    const confirmed = target.closest<HTMLButtonElement>("[data-duplicate-undo-confirm]");
    if (confirmed) void undo(confirmed.dataset.duplicateUndoConfirm || "", true);
    if (target.closest("[data-duplicate-undo-cancel]")) dialog.querySelector(".duplicates-undo-confirm")?.remove();
  });
  scope.addEventListener("change", () => { if (busy || isDeskRestoreBlocked()) return; epoch += 1; el("recordDuplicatesConfirm").hidden = true; scan = null; selected.clear(); renderScan(); status("검색 범위가 바뀌었습니다. 중복을 다시 검색해 주세요."); });
  dialog.addEventListener("cancel", event => { if (busy || isDeskRestoreBlocked()) event.preventDefault(); });
  dialog.addEventListener("close", () => { epoch += 1; tutorial.close(); clearInterval(tenantTimer); pending = null; scan = null; history = []; returnFocus?.focus(); });
  document.getElementById("dataExplorerDuplicates")?.addEventListener("click", () => void open());
  document.getElementById("studentTimelineDuplicates")?.addEventListener("click", () => void open(true));
  return { open };
}
