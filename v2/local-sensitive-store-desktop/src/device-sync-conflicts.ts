import { invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';

type ConflictRow = { conflictId: string; tableName: string; recordKey: string; losingGeneration: number; winningGeneration: number; capturedAtMs: number; reviewedAtMs?: number | null };
type ConflictStats = { unreviewed: number; retained: number; lifetime: number };
type ConflictDetail = ConflictRow & { losingPayload?: unknown; currentPayload?: unknown };
type ConflictListResult = { ok?: boolean; records?: ConflictRow[]; stats?: ConflictStats; error?: string };
type ConflictDetailResult = { ok?: boolean; conflict?: ConflictDetail; error?: string };

const tableLabels: Record<string, string> = { work_note_pages: '업무 노트', work_note_attachments: '업무 노트 첨부', lesson_observations: '관찰 기록', counseling_teacher_notes: '상담 교사 메모', board_post_snapshots: '게시판 기록' };
let getTenantId: () => string = () => '';
let rows: ConflictRow[] = [];
let selected = new Set<string>();
let activeId = '';
const tutorialVersion = 'device-sync-conflicts-v1';
const tutorialSteps = [
  { target: 'deviceConflictList', title: '충돌 기록 선택', copy: '충돌은 동기화 오류가 아니라, 더 최신 값에 밀린 이전 값을 이 PC에 따로 보관한 기록입니다.' },
  { target: 'deviceConflictDetail', title: '이전 값과 현재 값 비교', copy: '기록을 열어 보관된 이전 값과 지금 적용된 값을 나란히 확인합니다. 이전 값을 자동 복원하지는 않습니다.' },
  { target: 'deviceConflictReview', title: '검토 뒤 필요한 처리', copy: '필요하면 JSON으로 내보내고 검토 완료로 표시하세요. 삭제는 검토한 선택 항목에만 허용됩니다.' },
];
let tutorialIndex = -1;

const el = <T extends HTMLElement>(id: string) => { const value = document.getElementById(id); if (!value) throw new Error(`missing element: ${id}`); return value as T; };
const dateText = (value: number) => value ? new Intl.DateTimeFormat('ko-KR', { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(value)) : '-';
const label = (tableName: string) => tableLabels[tableName] || tableName;
const fileName = () => `ClassAiMate_충돌기록_${new Date().toISOString().slice(0, 10)}.json`;

function setStatus(message: string) { el('deviceConflictStatus').textContent = message; }
function renderStats(stats: ConflictStats = { unreviewed: 0, retained: 0, lifetime: 0 }) {
  el('deviceConflictUnreviewed').textContent = `${Number(stats.unreviewed || 0)}건`;
  el('deviceConflictRetained').textContent = `${Number(stats.retained || 0)}건`;
  el('deviceConflictLifetime').textContent = `${Number(stats.lifetime || 0)}건`;
}

function renderList() {
  const list = el('deviceConflictList'); list.replaceChildren();
  if (!rows.length) { const empty = document.createElement('p'); empty.textContent = '보관 중인 충돌 기록이 없습니다.'; list.append(empty); return; }
  for (const row of rows) {
    const item = document.createElement('div'); item.className = `device-conflict-row${row.conflictId === activeId ? ' is-active' : ''}`; item.dataset.conflictId = row.conflictId;
    const checkbox = document.createElement('input'); checkbox.type = 'checkbox'; checkbox.checked = selected.has(row.conflictId); checkbox.dataset.conflictSelect = row.conflictId; checkbox.setAttribute('aria-label', `${label(row.tableName)} 충돌 선택`);
    const button = document.createElement('button'); button.type = 'button'; button.dataset.conflictOpen = row.conflictId;
    const title = document.createElement('strong'); title.textContent = label(row.tableName);
    const key = document.createElement('span'); key.textContent = row.recordKey;
    const meta = document.createElement('small'); meta.textContent = `${dateText(row.capturedAtMs)} · ${row.losingGeneration} → ${row.winningGeneration}세대`;
    const status = document.createElement('b'); status.className = row.reviewedAtMs ? 'is-reviewed' : 'is-unreviewed'; status.textContent = row.reviewedAtMs ? '검토 완료' : '미검토';
    button.append(title, key, meta, status); item.append(checkbox, button); list.append(item);
  }
  updateActions();
}

function updateActions() {
  const chosen = rows.filter((row) => selected.has(row.conflictId));
  el<HTMLButtonElement>('deviceConflictReview').disabled = !chosen.length || chosen.every((row) => row.reviewedAtMs);
  el<HTMLButtonElement>('deviceConflictExport').disabled = !chosen.length;
  el<HTMLButtonElement>('deviceConflictDelete').disabled = !chosen.length || chosen.some((row) => !row.reviewedAtMs);
}

function renderJsonPanel(title: string, value: unknown) {
  const section = document.createElement('section'); const heading = document.createElement('h3'); heading.textContent = title;
  const pre = document.createElement('pre'); pre.textContent = value == null ? '현재 DB에는 이 기록이 없습니다.' : JSON.stringify(value, null, 2); section.append(heading, pre); return section;
}

async function openDetail(conflictId: string) {
  activeId = conflictId; renderList(); el('deviceConflictDetail').textContent = '충돌 값을 불러오고 있습니다.';
  const result = await invoke<ConflictDetailResult>('get_device_sync_conflict', { tenantId: getTenantId(), conflictId });
  if (result?.ok === false || !result.conflict) throw new Error(result.error || 'device_sync_conflict_detail_failed');
  const detail = el('deviceConflictDetail'); detail.replaceChildren();
  const header = document.createElement('header'); const heading = document.createElement('h2'); heading.textContent = label(result.conflict.tableName);
  const copy = document.createElement('p'); copy.textContent = `${dateText(result.conflict.capturedAtMs)}에 ${result.conflict.losingGeneration}세대 값이 밀리고 ${result.conflict.winningGeneration}세대 값이 유지되었습니다.`;
  header.append(heading, copy); detail.append(header, renderJsonPanel('보관된 이전 값', result.conflict.losingPayload), renderJsonPanel('현재 적용된 값', result.conflict.currentPayload));
}

async function refresh() {
  const tenantId = getTenantId().trim(); if (!tenantId) throw new Error('연결된 학급이 없습니다.');
  setStatus('충돌 기록을 불러오고 있습니다.');
  const result = await invoke<ConflictListResult>('list_device_sync_conflicts', { tenantId });
  if (result?.ok === false) throw new Error(result.error || 'device_sync_conflict_list_failed');
  rows = result.records || []; selected = new Set([...selected].filter((id) => rows.some((row) => row.conflictId === id)));
  renderStats(result.stats); renderList(); setStatus('충돌 기록은 동기화를 막지 않으며 이 PC에만 보관됩니다.');
  if (activeId && rows.some((row) => row.conflictId === activeId)) await openDetail(activeId); else { activeId = ''; el('deviceConflictDetail').innerHTML = '<p>왼쪽에서 충돌 기록을 선택하세요.</p>'; }
}

async function reviewSelected() {
  const conflictIds = [...selected]; if (!conflictIds.length) return;
  const result = await invoke<{ ok?: boolean; error?: string }>('review_device_sync_conflicts', { tenantId: getTenantId(), conflictIds });
  if (result?.ok === false) throw new Error(result.error || 'device_sync_conflict_review_failed'); setStatus(`${conflictIds.length}건을 검토 완료로 표시했습니다.`); await refresh();
}

async function exportSelected() {
  const conflictIds = [...selected]; if (!conflictIds.length) return;
  const targetPath = await save({ defaultPath: fileName(), filters: [{ name: 'JSON', extensions: ['json'] }] }); if (!targetPath) return;
  const result = await invoke<{ ok?: boolean; count?: number; error?: string }>('export_device_sync_conflicts', { tenantId: getTenantId(), conflictIds, targetPath });
  if (result?.ok === false) throw new Error(result.error || 'device_sync_conflict_export_failed'); setStatus(`선택한 ${Number(result.count || conflictIds.length)}건을 JSON으로 내보냈습니다.`);
}

async function deleteSelected() {
  const conflictIds = [...selected]; if (!conflictIds.length) return;
  if (!window.confirm(`검토를 마친 충돌 기록 ${conflictIds.length}건을 이 PC에서 삭제할까요? 삭제한 보관 값은 복구할 수 없습니다.`)) return;
  const result = await invoke<{ ok?: boolean; error?: string }>('delete_device_sync_conflicts', { tenantId: getTenantId(), conflictIds });
  if (result?.ok === false) throw new Error(result.error || 'device_sync_conflict_delete_failed'); selected.clear(); activeId = ''; setStatus(`${conflictIds.length}건을 삭제했습니다.`); await refresh();
}

function run(action: () => Promise<void>) { void action().catch((error) => setStatus(`처리 실패: ${String((error as Error)?.message || error)}`)); }

function renderTutorial() {
  document.querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target'));
  const step = tutorialSteps[tutorialIndex]; if (!step) { closeTutorial(); return; }
  el(step.target).classList.add('local-reader-tutorial-target'); el('deviceConflictTutorialStep').textContent = `${tutorialIndex + 1} / ${tutorialSteps.length}`;
  el('deviceConflictTutorialTitle').textContent = step.title; el('deviceConflictTutorialCopy').textContent = step.copy;
  el('deviceConflictTutorialNext').textContent = tutorialIndex === tutorialSteps.length - 1 ? '완료' : '다음'; el('deviceConflictTutorial').hidden = false;
}
function openTutorial() { tutorialIndex = 0; renderTutorial(); }
function closeTutorial() { tutorialIndex = -1; document.querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target')); const panel = document.getElementById('deviceConflictTutorial'); if (panel) panel.hidden = true; }

export function initDeviceSyncConflicts(options: { getTenantId: () => string }) {
  getTenantId = options.getTenantId;
  el('deviceSyncConflictsOpen').addEventListener('click', () => { el('deviceConflictViewer').hidden = false; run(refresh); if (localStorage.getItem(tutorialVersion) !== 'done') openTutorial(); });
  el('deviceConflictClose').addEventListener('click', () => { el('deviceConflictViewer').hidden = true; closeTutorial(); });
  el('deviceConflictHelp').addEventListener('click', openTutorial);
  el('deviceConflictTutorialClose').addEventListener('click', closeTutorial);
  el('deviceConflictTutorialNext').addEventListener('click', () => { if (tutorialIndex >= tutorialSteps.length - 1) { localStorage.setItem(tutorialVersion, 'done'); closeTutorial(); } else { tutorialIndex += 1; renderTutorial(); } });
  el('deviceConflictList').addEventListener('change', (event) => { const input = (event.target as HTMLElement).closest<HTMLInputElement>('[data-conflict-select]'); if (!input?.dataset.conflictSelect) return; if (input.checked) selected.add(input.dataset.conflictSelect); else selected.delete(input.dataset.conflictSelect); updateActions(); });
  el('deviceConflictList').addEventListener('click', (event) => { const button = (event.target as HTMLElement).closest<HTMLButtonElement>('[data-conflict-open]'); if (button?.dataset.conflictOpen) run(() => openDetail(button.dataset.conflictOpen!)); });
  el('deviceConflictReview').addEventListener('click', () => run(reviewSelected));
  el('deviceConflictExport').addEventListener('click', () => run(exportSelected));
  el('deviceConflictDelete').addEventListener('click', () => run(deleteSelected));
}
