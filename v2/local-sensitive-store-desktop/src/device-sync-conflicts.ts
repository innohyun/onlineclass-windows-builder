import { invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';

type ConflictPreview = {
  title?: string;
  emoji?: string;
  studentName?: string;
  studentCode?: string;
  classNo?: number;
  dateKey?: string;
  subject?: string;
  fileName?: string;
  status?: string;
  summary?: string;
};
type ConflictRow = {
  conflictId: string;
  tableName: string;
  recordKey: string;
  losingGeneration: number;
  winningGeneration: number;
  capturedAtMs: number;
  reviewedAtMs?: number | null;
  preview?: ConflictPreview;
};
type ConflictStats = { unreviewed: number; retained: number; lifetime: number };
type ConflictDetail = ConflictRow & { losingPayload?: unknown; currentPayload?: unknown };
type ConflictListResult = { ok?: boolean; records?: ConflictRow[]; stats?: ConflictStats; error?: string };
type ConflictDetailResult = { ok?: boolean; conflict?: ConflictDetail; error?: string };
type JsonRecord = Record<string, unknown>;
type ReadableField = { id: string; label: string; value: string };

const tableLabels: Record<string, string> = {
  lesson_observations: '관찰 기록',
  teacher_counseling_sessions: '교사 상담 기록',
  student_private_details: '학생 민감정보',
  student_private_photos: '학생 사진',
  math_daily_attempts: '매일수학 풀이 기록',
  math_daily_student_profiles: '매일수학 학습 상태',
  math_daily_review_sessions: '매일수학 복습 기록',
  math_daily_assignments: '매일수학 과제',
  math_daily_assignment_results: '매일수학 과제 결과',
  math_daily_cache_runs: '매일수학 자료 갱신',
  board_post_snapshots: '게시판 기록',
  board_media_files: '게시판 첨부파일',
  attendance_records: '출결 기록',
  attendance_nais_checks: '출결 NEIS 확인',
  attendance_document_requests: '출결 증빙 요청',
  counseling_records: '상담 요청',
  counseling_teacher_notes: '상담 교사 메모',
  eval_assignments: '평가 운영',
  eval_results: '평가 결과',
  student_record_draft_sets: '학생부 초안 묶음',
  student_record_drafts: '학생부 초안',
  local_import_runs: '자료 가져오기 기록',
  work_note_pages: '업무 노트',
  work_note_attachments: '업무 노트 첨부파일',
};
const statusLabels: Record<string, string> = {
  active: '사용 중', archived: '보관됨', approved: '승인', closed: '종료', completed: '완료',
  draft: '초안', follow_up: '추후 확인', in_progress: '진행 중', pending: '대기', recorded: '기록 완료',
  rejected: '반려', reviewed: '검토 완료', unread: '미확인', read: '확인', absent: '결석', late: '지각',
};
const fieldLabels: Record<string, string> = {
  title: '제목', pageTitle: '문서 제목', assignmentTitle: '과제 이름', emoji: '아이콘',
  studentName: '학생 이름', displayName: '이름', classNo: '번호', class_no: '번호', studentCode: '학생', student_code: '학생', studentId: '학생', student_id: '학생',
  dateKey: '기록 날짜', date_key: '기록 날짜', date: '날짜', scheduledDate: '예정 날짜', scheduled_date: '예정 날짜', fromDate: '시작 날짜', from_date: '시작 날짜', toDate: '종료 날짜', to_date: '종료 날짜',
  period: '교시', subject: '교과', curriculum: '학습 과정', status: '상태', kind: '종류', action: '작업',
  summary: '요약', description: '설명', observation: '관찰 내용', memo: '메모', note: '메모', reason: '사유', content: '내용', draftText: '초안 내용', text: '내용', behaviorComment: '행동특성 및 종합의견', subjectComments: '교과별 의견', creativeComments: '창의적 체험활동 의견', customResultText: '평가 내용', feedback: '피드백', result: '결과', markdown: '본문',
  fileName: '파일 이름', file_name: '파일 이름', contentType: '파일 종류', content_type: '파일 종류', byteSize: '파일 크기', byte_size: '파일 크기', size: '파일 크기',
  dueDate: '마감일', tags: '태그', year: '학년도', updatedAtMs: '마지막 수정', updated_at_ms: '마지막 수정', counselingAtMs: '상담 일시', counseling_at_ms: '상담 일시', archivedAtMs: '보관 일시', archived_at_ms: '보관 일시',
  topic: '주제', category: '분류', level: '수준', score: '점수', answer: '답', attachments: '첨부파일', files: '첨부파일', properties: '문서 속성',
};
const hiddenKeys = new Set([
  'tenantId', 'tenant_id', 'id', 'docId', 'doc_id', 'pageId', 'page_id', 'parentId', 'parent_id',
  'recordId', 'record_id', 'requestId', 'request_id', 'draftId', 'draft_id', 'draftSetId', 'draft_set_id',
  'assignmentId', 'assignment_id', 'resultId', 'result_id', 'runId', 'run_id', 'sessionId', 'session_id',
  'postId', 'post_id', 'boardId', 'board_id', 'mediaId', 'media_id', 'attachmentId', 'attachment_id',
  'blockId', 'block_id', 'sha256', 'storagePath', 'storage_path', 'localPath', 'local_path',
  'createdAtMs', 'created_at_ms', 'payload_json', 'properties_json', 'document_json', 'position',
]);
const contentKeys = [
  'behaviorComment', 'subjectComments', 'creativeComments', 'customResultText', 'observation', 'summary',
  'description', 'reason', 'feedback', 'result', 'memo', 'note', 'content', 'draftText', 'text', 'markdown',
];

let getTenantId: () => string = () => '';
let rows: ConflictRow[] = [];
let selected = new Set<string>();
let activeId = '';
const tutorialVersion = 'device-sync-conflicts-v2';
const tutorialSteps = [
  { target: 'deviceConflictList', title: '어떤 기록인지 먼저 확인', copy: '내부 테이블명과 긴 식별자 대신 자료 종류, 학생·제목·날짜를 보여 줍니다.' },
  { target: 'deviceConflictDetail', title: '바뀐 내용만 비교', copy: '두 PC에서 달랐던 항목을 이전 값과 현재 적용 값으로 나누어 읽기 쉽게 보여 줍니다. 원문은 고급 정보에서 확인할 수 있습니다.' },
  { target: 'deviceConflictReview', title: '확인한 기록 정리', copy: '필요하면 원문 파일을 내보내고 검토 완료로 표시하세요. 삭제는 검토한 선택 기록에만 허용됩니다.' },
];
let tutorialIndex = -1;

const el = <T extends HTMLElement>(id: string) => {
  const value = document.getElementById(id);
  if (!value) throw new Error(`missing element: ${id}`);
  return value as T;
};
const dateText = (value: number) => value
  ? new Intl.DateTimeFormat('ko-KR', { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(value))
  : '-';
const label = (tableName: string) => tableLabels[tableName] || '기타 로컬 기록';
const fileName = () => `ClassAiMate_충돌기록_${new Date().toISOString().slice(0, 10)}.json`;

function objectValue(value: unknown): JsonRecord {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonRecord : {};
}

function parseJson(value: unknown): unknown {
  if (typeof value !== 'string') return value;
  const text = value.trim();
  if (!text || (!text.startsWith('{') && !text.startsWith('['))) return value;
  try { return JSON.parse(text); } catch { return value; }
}

function firstValue(record: JsonRecord, keys: string[]) {
  for (const key of keys) {
    const value = record[key];
    if (value !== undefined && value !== null && value !== '') return value;
  }
  return undefined;
}

function firstText(record: JsonRecord, keys: string[]) {
  const value = firstValue(record, keys);
  return typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
}

function formatBytes(value: number) {
  if (value >= 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)}MB`;
  if (value >= 1024) return `${Math.round(value / 1024)}KB`;
  return `${value}바이트`;
}

function formatStructured(value: unknown, key = ''): string {
  const parsed = parseJson(value);
  if (parsed == null || parsed === '') return '없음';
  if (typeof parsed === 'boolean') return parsed ? '예' : '아니요';
  if (typeof parsed === 'number') {
    if (/(?:AtMs|_at_ms)$/u.test(key) && parsed > 100000000000) return dateText(parsed);
    if (/(?:size|bytes|byteSize|byte_size)$/iu.test(key) && parsed >= 0) return formatBytes(parsed);
    return Number.isFinite(parsed) ? parsed.toLocaleString('ko-KR') : String(parsed);
  }
  if (typeof parsed === 'string') {
    const status = statusLabels[parsed.toLowerCase()];
    return status || parsed.trim() || '없음';
  }
  if (Array.isArray(parsed)) {
    if (!parsed.length) return '없음';
    return parsed.map((item) => `• ${formatStructured(item, key)}`).join('\n');
  }
  const entries = Object.entries(objectValue(parsed)).filter(([childKey, childValue]) => !hiddenKeys.has(childKey) && childValue !== '' && childValue != null);
  if (!entries.length) return '내용 없음';
  return entries.map(([childKey, childValue]) => {
    const childLabel = fieldLabels[childKey];
    const formatted = formatStructured(childValue, childKey);
    return childLabel ? `${childLabel}: ${formatted}` : formatted;
  }).join('\n');
}

function expandedPayload(value: unknown) {
  const outer = objectValue(value);
  const nested = objectValue(parseJson(outer.payload_json));
  const properties = objectValue(parseJson(outer.properties_json));
  return { outer, nested, properties, combined: { ...outer, ...nested } };
}

function addField(fields: ReadableField[], id: string, labelText: string, value: unknown) {
  if (value === undefined || value === null || value === '') return;
  const formatted = formatStructured(value, id);
  if (!formatted || formatted === '없음' || fields.some((field) => field.id === id)) return;
  fields.push({ id, label: labelText, value: formatted });
}

function readableFields(tableName: string, value: unknown) {
  const { outer, nested, properties, combined } = expandedPayload(value);
  const fields: ReadableField[] = [];
  const studentName = firstText(combined, ['studentName', 'displayName']);
  const studentCode = firstText(combined, ['studentCode', 'student_code', 'studentId', 'student_id']);
  const classNo = firstText(combined, ['classNo', 'class_no', 'number']);
  const student = [classNo ? `${classNo}번` : '', studentName || (studentCode ? `학생 ${studentCode}` : '')].filter(Boolean).join(' · ');
  addField(fields, 'student', '대상 학생', student);

  addField(fields, 'title', tableName === 'work_note_pages' ? '문서 제목' : '기록 이름', firstValue(combined, ['title', 'pageTitle', 'assignmentTitle', 'name']));
  addField(fields, 'emoji', '아이콘', firstValue(combined, ['emoji']));
  addField(fields, 'fileName', '파일 이름', firstValue(combined, ['fileName', 'file_name']));
  addField(fields, 'date', '기록 날짜', firstValue(combined, ['dateKey', 'date_key', 'date', 'scheduledDate', 'scheduled_date']));
  addField(fields, 'period', '교시', firstValue(combined, ['period']));
  addField(fields, 'subject', '교과·영역', firstValue(combined, ['subject', 'curriculum', 'topic', 'category']));
  addField(fields, 'status', '상태', firstValue(combined, ['status', 'kind', 'action']));

  if (tableName === 'student_record_draft_sets') {
    const from = firstText(combined, ['fromDate', 'from_date']);
    const to = firstText(combined, ['toDate', 'to_date']);
    addField(fields, 'range', '기록 범위', [from, to].filter(Boolean).join(' ~ '));
  }
  for (const key of contentKeys) {
    if (tableName === 'work_note_pages' && key !== 'markdown') continue;
    const valueForKey = nested[key] ?? outer[key];
    addField(fields, key, fieldLabels[key] || '주요 내용', valueForKey);
  }
  if (tableName === 'work_note_pages' && !fields.some((field) => field.id === 'markdown')) {
    addField(fields, 'document', '본문', parseJson(outer.document_json));
  }
  addField(fields, 'dueDate', '마감일', firstValue(properties, ['dueDate']));
  addField(fields, 'tags', '태그', firstValue(properties, ['tags']));
  addField(fields, 'year', '학년도', firstValue(properties, ['year']));
  addField(fields, 'attachments', '첨부파일', firstValue(combined, ['attachments', 'files']));
  addField(fields, 'size', '파일 크기', firstValue(combined, ['byteSize', 'byte_size', 'size']));
  addField(fields, 'updatedAtMs', '마지막 수정', firstValue(combined, ['updatedAtMs', 'updated_at_ms']));
  return fields;
}

function previewText(row: ConflictRow) {
  const preview = row.preview || {};
  const student = [preview.classNo ? `${preview.classNo}번` : '', preview.studentName || (preview.studentCode ? `학생 ${preview.studentCode}` : '')].filter(Boolean).join(' · ');
  const title = [preview.emoji, preview.title].filter(Boolean).join(' ').trim();
  const identity = [title, student].filter(Boolean).join(' · ');
  return identity || preview.fileName || [preview.dateKey, preview.subject].filter(Boolean).join(' · ') || preview.summary || '저장된 기록';
}

function setStatus(message: string) { el('deviceConflictStatus').textContent = message; }

function renderStats(stats: ConflictStats = { unreviewed: 0, retained: 0, lifetime: 0 }) {
  el('deviceConflictUnreviewed').textContent = `${Number(stats.unreviewed || 0)}건`;
  el('deviceConflictRetained').textContent = `${Number(stats.retained || 0)}건`;
  el('deviceConflictLifetime').textContent = `${Number(stats.lifetime || 0)}건`;
}

function renderList() {
  const list = el('deviceConflictList');
  list.replaceChildren();
  if (!rows.length) {
    const empty = document.createElement('p');
    empty.className = 'device-conflict-empty';
    empty.textContent = '보관 중인 충돌 기록이 없습니다.';
    list.append(empty);
    return;
  }
  for (const row of rows) {
    const item = document.createElement('div');
    item.className = `device-conflict-row${row.conflictId === activeId ? ' is-active' : ''}`;
    item.dataset.conflictId = row.conflictId;
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    checkbox.checked = selected.has(row.conflictId);
    checkbox.dataset.conflictSelect = row.conflictId;
    checkbox.setAttribute('aria-label', `${label(row.tableName)} 충돌 선택`);
    const button = document.createElement('button');
    button.type = 'button';
    button.dataset.conflictOpen = row.conflictId;
    const title = document.createElement('strong');
    title.textContent = label(row.tableName);
    const summary = document.createElement('span');
    summary.className = 'device-conflict-row-summary';
    summary.textContent = previewText(row);
    const meta = document.createElement('small');
    meta.textContent = `${dateText(row.capturedAtMs)} · 최신 동기화본 적용`;
    const status = document.createElement('b');
    status.className = row.reviewedAtMs ? 'is-reviewed' : 'is-unreviewed';
    status.textContent = row.reviewedAtMs ? '검토 완료' : '확인 필요';
    button.append(title, summary, meta, status);
    item.append(checkbox, button);
    list.append(item);
  }
  updateActions();
}

function updateActions() {
  const chosen = rows.filter((row) => selected.has(row.conflictId));
  el<HTMLButtonElement>('deviceConflictReview').disabled = !chosen.length || chosen.every((row) => row.reviewedAtMs);
  el<HTMLButtonElement>('deviceConflictExport').disabled = !chosen.length;
  el<HTMLButtonElement>('deviceConflictDelete').disabled = !chosen.length || chosen.some((row) => !row.reviewedAtMs);
}

function rawJsonPanel(title: string, value: unknown) {
  const section = document.createElement('section');
  const heading = document.createElement('h4');
  heading.textContent = title;
  const pre = document.createElement('pre');
  pre.textContent = value == null ? '현재 DB에는 이 기록이 없습니다.' : JSON.stringify(value, null, 2);
  section.append(heading, pre);
  return section;
}

function renderReadableComparison(tableName: string, previous: unknown, current: unknown) {
  const previousMap = new Map(readableFields(tableName, previous).map((field) => [field.id, field]));
  const currentMap = new Map(readableFields(tableName, current).map((field) => [field.id, field]));
  const ids = [...new Set([...previousMap.keys(), ...currentMap.keys()])];
  const changed = ids.filter((id) => previousMap.get(id)?.value !== currentMap.get(id)?.value);
  const section = document.createElement('section');
  section.className = 'device-conflict-comparison';
  const heading = document.createElement('h3');
  heading.textContent = `달라진 내용 ${changed.length}개`;
  section.append(heading);
  if (!changed.length) {
    const same = document.createElement('p');
    same.className = 'device-conflict-same';
    same.textContent = '사람이 읽는 주요 내용은 같습니다. 저장 시각이나 내부 동기화 정보만 달라졌을 수 있습니다.';
    section.append(same);
    return section;
  }
  const list = document.createElement('div');
  list.className = 'device-conflict-change-list';
  for (const id of changed) {
    const before = previousMap.get(id);
    const after = currentMap.get(id);
    const card = document.createElement('article');
    card.className = 'device-conflict-change';
    const title = document.createElement('h4');
    title.textContent = after?.label || before?.label || '변경 내용';
    const values = document.createElement('div');
    values.className = 'device-conflict-change-values';
    for (const [kind, caption, field] of [['before', '보관된 이전 값', before], ['after', '현재 적용된 값', after]] as const) {
      const value = document.createElement('div');
      value.className = `is-${kind}`;
      const labelNode = document.createElement('span');
      labelNode.textContent = caption;
      const textNode = document.createElement('p');
      textNode.textContent = field?.value || '없음';
      value.append(labelNode, textNode);
      values.append(value);
    }
    card.append(title, values);
    list.append(card);
  }
  section.append(list);
  return section;
}

async function openDetail(conflictId: string) {
  activeId = conflictId;
  renderList();
  el('deviceConflictDetail').textContent = '충돌 기록을 읽기 쉽게 정리하고 있습니다.';
  const result = await invoke<ConflictDetailResult>('get_device_sync_conflict', { tenantId: getTenantId(), conflictId });
  if (result?.ok === false || !result.conflict) throw new Error(result.error || 'device_sync_conflict_detail_failed');
  const detail = el('deviceConflictDetail');
  detail.replaceChildren();
  const activeRow = rows.find((row) => row.conflictId === conflictId);
  const header = document.createElement('header');
  const heading = document.createElement('h2');
  heading.textContent = `${label(result.conflict.tableName)} · ${activeRow ? previewText(activeRow) : '저장된 기록'}`;
  const copy = document.createElement('p');
  copy.textContent = `${dateText(result.conflict.capturedAtMs)}에 같은 기록의 내용이 달라 최신 동기화본을 적용했습니다. 이전 값은 확인용으로만 보관됩니다.`;
  header.append(heading, copy);
  const notice = document.createElement('div');
  notice.className = 'device-conflict-notice';
  notice.innerHTML = '<strong>현재 적용된 값이 앱에서 사용됩니다.</strong><span>보관된 이전 값은 자동으로 복원되거나 현재 기록을 덮어쓰지 않습니다.</span>';
  const advanced = document.createElement('details');
  advanced.className = 'device-conflict-advanced';
  const advancedTitle = document.createElement('summary');
  advancedTitle.textContent = '고급 정보(JSON) 보기';
  const generation = document.createElement('p');
  generation.textContent = `동기화 세대 ${result.conflict.losingGeneration} → ${result.conflict.winningGeneration}`;
  const raw = document.createElement('div');
  raw.className = 'device-conflict-raw-grid';
  raw.append(rawJsonPanel('보관된 원문', result.conflict.losingPayload), rawJsonPanel('현재 원문', result.conflict.currentPayload));
  advanced.append(advancedTitle, generation, raw);
  detail.append(header, notice, renderReadableComparison(result.conflict.tableName, result.conflict.losingPayload, result.conflict.currentPayload), advanced);
}

async function refresh() {
  const tenantId = getTenantId().trim();
  if (!tenantId) throw new Error('연결된 학급이 없습니다.');
  setStatus('충돌 기록을 불러오고 있습니다.');
  const result = await invoke<ConflictListResult>('list_device_sync_conflicts', { tenantId });
  if (result?.ok === false) throw new Error(result.error || 'device_sync_conflict_list_failed');
  rows = result.records || [];
  selected = new Set([...selected].filter((id) => rows.some((row) => row.conflictId === id)));
  renderStats(result.stats);
  renderList();
  setStatus('확인한 기록만 검토 완료로 표시하세요. 이 보관함의 기록은 현재 동기화를 막지 않습니다.');
  if (activeId && rows.some((row) => row.conflictId === activeId)) await openDetail(activeId);
  else if (rows[0]) await openDetail(rows[0].conflictId);
  else {
    activeId = '';
    el('deviceConflictDetail').innerHTML = '<p class="device-conflict-empty">보관 중인 충돌 기록이 없습니다.</p>';
  }
}

async function reviewSelected() {
  const conflictIds = [...selected];
  if (!conflictIds.length) return;
  const result = await invoke<{ ok?: boolean; error?: string }>('review_device_sync_conflicts', { tenantId: getTenantId(), conflictIds });
  if (result?.ok === false) throw new Error(result.error || 'device_sync_conflict_review_failed');
  setStatus(`${conflictIds.length}건을 검토 완료로 표시했습니다.`);
  await refresh();
}

async function exportSelected() {
  const conflictIds = [...selected];
  if (!conflictIds.length) return;
  const targetPath = await save({ defaultPath: fileName(), filters: [{ name: 'JSON', extensions: ['json'] }] });
  if (!targetPath) return;
  const result = await invoke<{ ok?: boolean; count?: number; error?: string }>('export_device_sync_conflicts', { tenantId: getTenantId(), conflictIds, targetPath });
  if (result?.ok === false) throw new Error(result.error || 'device_sync_conflict_export_failed');
  setStatus(`선택한 ${Number(result.count || conflictIds.length)}건의 원문 파일을 내보냈습니다.`);
}

async function deleteSelected() {
  const conflictIds = [...selected];
  if (!conflictIds.length) return;
  if (!window.confirm(`검토를 마친 충돌 기록 ${conflictIds.length}건을 이 PC에서 삭제할까요? 삭제한 보관 값은 복구할 수 없습니다.`)) return;
  const result = await invoke<{ ok?: boolean; error?: string }>('delete_device_sync_conflicts', { tenantId: getTenantId(), conflictIds });
  if (result?.ok === false) throw new Error(result.error || 'device_sync_conflict_delete_failed');
  selected.clear();
  activeId = '';
  setStatus(`${conflictIds.length}건을 삭제했습니다.`);
  await refresh();
}

function run(action: () => Promise<void>) {
  void action().catch((error) => setStatus(`처리 실패: ${String((error as Error)?.message || error)}`));
}

function renderTutorial() {
  document.querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target'));
  const step = tutorialSteps[tutorialIndex];
  if (!step) { closeTutorial(); return; }
  el(step.target).classList.add('local-reader-tutorial-target');
  el('deviceConflictTutorialStep').textContent = `${tutorialIndex + 1} / ${tutorialSteps.length}`;
  el('deviceConflictTutorialTitle').textContent = step.title;
  el('deviceConflictTutorialCopy').textContent = step.copy;
  el('deviceConflictTutorialNext').textContent = tutorialIndex === tutorialSteps.length - 1 ? '완료' : '다음';
  el('deviceConflictTutorial').hidden = false;
}

function openTutorial() { tutorialIndex = 0; renderTutorial(); }
function closeTutorial() {
  tutorialIndex = -1;
  document.querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target'));
  const panel = document.getElementById('deviceConflictTutorial');
  if (panel) panel.hidden = true;
}

export function initDeviceSyncConflicts(options: { getTenantId: () => string }) {
  getTenantId = options.getTenantId;
  el('deviceSyncConflictsOpen').addEventListener('click', () => {
    el('deviceConflictViewer').hidden = false;
    run(refresh);
    if (localStorage.getItem(tutorialVersion) !== 'done') openTutorial();
  });
  el('deviceConflictClose').addEventListener('click', () => { el('deviceConflictViewer').hidden = true; closeTutorial(); });
  el('deviceConflictHelp').addEventListener('click', openTutorial);
  el('deviceConflictTutorialClose').addEventListener('click', closeTutorial);
  el('deviceConflictTutorialNext').addEventListener('click', () => {
    if (tutorialIndex >= tutorialSteps.length - 1) { localStorage.setItem(tutorialVersion, 'done'); closeTutorial(); }
    else { tutorialIndex += 1; renderTutorial(); }
  });
  el('deviceConflictList').addEventListener('change', (event) => {
    const input = (event.target as HTMLElement).closest<HTMLInputElement>('[data-conflict-select]');
    if (!input?.dataset.conflictSelect) return;
    if (input.checked) selected.add(input.dataset.conflictSelect);
    else selected.delete(input.dataset.conflictSelect);
    updateActions();
  });
  el('deviceConflictList').addEventListener('click', (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>('[data-conflict-open]');
    if (button?.dataset.conflictOpen) run(() => openDetail(button.dataset.conflictOpen!));
  });
  el('deviceConflictReview').addEventListener('click', () => run(reviewSelected));
  el('deviceConflictExport').addEventListener('click', () => run(exportSelected));
  el('deviceConflictDelete').addEventListener('click', () => run(deleteSelected));
}
