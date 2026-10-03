import { invoke } from '@tauri-apps/api/core';
import { type LocalDataRecord } from './data-explorer';
import { createStudentPrivacyToggle, isStudentPrivacyEnabled } from './desk-privacy';
import { isDeskRestoreBlocked } from './desk-restore-lock';

export type TeacherRecordKind = 'observation' | 'counseling';
export type TeacherRecordLocator = { tenantId: string; kind: TeacherRecordKind; recordId: string };
type TeacherRecord = Record<string, unknown>;
type RecordResult = { ok?: boolean; record?: TeacherRecord; revision?: string | number; verified?: boolean; error?: string };
export type TeacherRecordFields = { text: string; tags: string; status: string; followUpNote: string; correctionReason: string; studentCode: string; counselingDate?: string };
type RosterStudent = { id: string; displayName: string; classNo?: number | null; status?: string };
type RosterResult = { ok?: boolean; tenantId?: string; roster?: { students: RosterStudent[]; stale?: boolean; syncedAtMs?: number }; error?: string };
const TUTORIAL_KEY = 'localTeacherRecordEditorTutorial:v2';
const OBSERVATION_STATES = [['good', '강점'], ['warning', '계속 관찰'], ['help', '도움 필요'], ['none', '일반']];
const COUNSELING_STATES = [['completed', '상담 완료'], ['follow_up', '후속 지도']];
function canonicalRecordValue(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalRecordValue).join(',')}]`;
  if (value && typeof value === 'object') return `{${Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([key, item]) => `${JSON.stringify(key)}:${canonicalRecordValue(item)}`).join(',')}}`;
  return JSON.stringify(value);
}

/** A date-only input is midnight in Korea, independent of the PC's timezone. */
export function counselingDateToMs(value: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return null;
  const result = Date.parse(`${value}T00:00:00+09:00`);
  return Number.isFinite(result) && result > 0 && recordDateKst(result) === value ? result : null;
}
export function recordDateKst(value: unknown): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) return '';
  const date = new Date(value + 9 * 60 * 60 * 1000);
  return Number.isFinite(date.getTime()) ? date.toISOString().slice(0, 10) : '';
}
export function teacherRecordLocator(tenantId: string, record?: LocalDataRecord): TeacherRecordLocator | null {
  if (!tenantId || !record || (record.payload.tenantId && record.payload.tenantId !== tenantId)) return null;
  if (record.sectionKey === 'observations' && typeof record.payload.docId === 'string' && record.payload.docId.trim()) return { tenantId, kind: 'observation', recordId: record.payload.docId };
  if (record.sectionKey === 'teacher-counseling-sessions' && typeof record.payload.sessionId === 'string' && record.payload.sessionId.trim()) return { tenantId, kind: 'counseling', recordId: record.payload.sessionId };
  return null;
}
export function teacherRecordPatch(kind: TeacherRecordKind, original: TeacherRecord, fields: TeacherRecordFields, options: { creating?: boolean } = {}) {
  const candidate: TeacherRecord = {
    [kind === 'observation' ? 'note' : 'summary']: fields.text.trim(),
    [kind === 'observation' ? 'tags' : 'topics']: fields.tags.split(',').map(value => value.trim()).filter(Boolean),
    status: fields.status,
  };
  if (kind === 'counseling') {
    candidate.followUpNote = fields.followUpNote.trim();
    // Student, existing date, private notes, links and evidence never become editable patches.
    if (options.creating) {
      const date = counselingDateToMs(fields.counselingDate || '');
      if (date === null) throw new Error('counseling_date_required');
      candidate.counselingAtMs = date;
    }
  }
  const patch: TeacherRecord = {};
  for (const [key, value] of Object.entries(candidate)) {
    const previous = original[key] ?? (Array.isArray(value) ? [] : '');
    if (JSON.stringify(value) !== JSON.stringify(previous)) patch[key] = value;
  }
  return patch;
}
export function teacherRecordMatches(locator: TeacherRecordLocator, record: TeacherRecord, studentCode?: string): boolean {
  return record[locator.kind === 'observation' ? 'docId' : 'sessionId'] === locator.recordId
    && (!record.tenantId || record.tenantId === locator.tenantId)
    && (studentCode === undefined || record.studentCode === studentCode);
}
export function verifyTeacherRecordPatch(patch: TeacherRecord, result: RecordResult, locator?: TeacherRecordLocator, studentCode?: string) {
  if (!result.ok || !result.record || result.revision == null) return false;
  if (locator && (!teacherRecordMatches(locator, result.record, studentCode)
    || result.record[locator.kind === 'observation' ? 'revisionId' : 'updatedAtMs'] !== result.revision)) return false;
  return Object.entries(patch).every(([key, value]) => JSON.stringify(result.record![key] ?? (Array.isArray(value) ? [] : '')) === JSON.stringify(value));
}
export function validateTeacherRecordFields(kind: TeacherRecordKind, fields: TeacherRecordFields, options: { creating?: boolean; rosterIds?: ReadonlySet<string> } = {}): string[] {
  const errors: string[] = [];
  const limit = kind === 'observation' ? 1000 : 5000;
  if (!fields.text.trim()) errors.push(kind === 'observation' ? '관찰 내용을 입력하세요.' : '상담 내용을 입력하세요.');
  if (Array.from(fields.text.trim()).length > limit) errors.push(`본문은 ${limit}자 이내로 입력하세요.`);
  if (!(kind === 'observation' ? OBSERVATION_STATES : COUNSELING_STATES).some(([value]) => value === fields.status)) errors.push('상태를 선택하세요.');
  if (kind === 'observation' && !fields.correctionReason.trim()) errors.push('정정 사유를 입력하세요.');
  if (fields.correctionReason.length > 500) errors.push('정정 사유는 500자 이내로 입력하세요.');
  if (kind === 'counseling' && Array.from(fields.followUpNote).length > 2000) errors.push('후속 지도 내용은 2000자 이내로 입력하세요.');
  const tags = fields.tags.split(',').map(value => value.trim()).filter(Boolean);
  const tagLimit = kind === 'observation' ? 20 : 8;
  if (tags.length > tagLimit || tags.some(value => Array.from(value).length > 60)) errors.push(`${kind === 'observation' ? '태그' : '상담 주제'}는 ${tagLimit}개까지, 각 60자 이내로 입력하세요.`);
  if (options.creating) {
    if (!fields.studentCode || !options.rosterIds?.has(fields.studentCode)) errors.push('현재 학급 명단에서 학생을 선택하세요.');
    if (counselingDateToMs(fields.counselingDate || '') === null) errors.push('상담 날짜를 입력하세요.');
  }
  return errors;
}
export function updateTeacherRecordAction(container: HTMLElement, record: LocalDataRecord | undefined, tenantId: string) {
  container.querySelector('.desk-record-edit-entry')?.remove();
  const locator = teacherRecordLocator(tenantId, record);
  if (!locator) return;
  const button = document.createElement('button');
  button.type = 'button'; button.className = 'desk-outline desk-record-edit-entry';
  button.textContent = locator.kind === 'observation' ? '관찰 내용 정정' : '상담 기록 수정';
  button.addEventListener('click', () => window.dispatchEvent(new CustomEvent('desk:edit-record', { detail: locator })));
  container.prepend(button);
}

export function initDeskRecordEditor(options: { getTenantId: () => string; onChanged?: () => void }) {
  document.body.insertAdjacentHTML('beforeend', `<style>
    #deskRecordDialog{width:min(1120px,calc(100vw - 32px))}#deskRecordDialog>header{align-items:flex-start}
    #deskRecordDialog header .desk-actions{flex-wrap:wrap;justify-content:flex-end}#deskRecordDialog header .student-privacy-toggle{width:auto;padding:8px 12px;min-height:36px;color:#244563}
    #deskRecordLayout{display:grid;grid-template-columns:minmax(230px,1fr) minmax(0,1.6fr)}#deskRecordContext{padding:24px;background:#f5f8fc;border-right:1px solid #e1e7f0;min-width:0}
    #deskRecordDialog h3{margin:0 0 16px;font-size:16px}#deskRecordContext dl{display:grid;grid-template-columns:75px minmax(0,1fr);gap:12px;margin:0 0 22px;font-size:14px}#deskRecordContext dd{margin:0;overflow-wrap:anywhere}
    #deskRecordDialog .desk-record-original{white-space:pre-wrap;overflow-wrap:anywhere;font:inherit;line-height:1.8;max-height:240px;overflow:auto;padding:16px;border:1px solid #dce4ee;border-radius:8px;background:white}
    #deskRecordDialog label{font-size:14px}#deskRecordDialog label>small,#deskRecordDialog footer>span{font-size:12px}#deskRecordDialog textarea,#deskRecordDialog input,#deskRecordDialog select{font-size:14px}
    #deskRecordDialog .desk-record-field-error{color:#ad3d26}#deskRecordDialog .desk-record-field-error:empty{display:none}#deskRecordDialog [aria-invalid=true]{border-color:#c26447}
    #deskRecordPrivacyNotice,#deskRecordConflict{margin:0 24px 20px;padding:16px;border:1px solid #e6ca92;border-radius:8px;background:#fff8e8;font-size:14px;line-height:1.7}
    #deskRecordConflict .desk-grid-two{margin-top:12px}#deskRecordConflict label{flex-direction:row;align-items:center;margin:16px 0}#deskRecordConflict input[type=checkbox]{width:auto}
    #deskRecordDialog [hidden]{display:none!important}#deskRecordStatus{min-height:1.5em}#deskRecordFixedDate{display:block;padding:10px 0}
    @media(max-width:760px){#deskRecordLayout{grid-template-columns:1fr}#deskRecordContext{border-right:0;border-bottom:1px solid #e1e7f0}#deskRecordDialog>header{padding:18px}#deskRecordDialog .desk-grid-two{grid-template-columns:1fr}#deskRecordDialog footer{flex-wrap:wrap}#deskRecordDialog footer button{width:100%}}
  </style><dialog class="desk-dialog desk-record-dialog" id="deskRecordDialog" aria-labelledby="deskRecordTitle"><header><div><h2 id="deskRecordTitle">기록 작성</h2><p id="deskRecordMeta">이 PC의 학생 기록 · 저장 후 재조회</p></div><div class="desk-actions" id="deskRecordHeaderActions"><button type="button" id="deskRecordHelp" aria-label="현재 기록 화면 사용 안내"><i class="fa-solid fa-circle-question" aria-hidden="true"></i></button><button type="button" id="deskRecordClose" aria-label="기록 창 닫기"><i class="fa-solid fa-xmark" aria-hidden="true"></i></button></div></header>
    <p id="deskRecordPrivacyNotice" hidden>학생 기록 가림이 켜져 있습니다. 입력은 현재 창의 메모리에 보존됩니다. 가림을 해제하면 확인·입력을 이어갑니다.</p>
    <div id="deskRecordLayout"><section id="deskRecordContext" aria-labelledby="deskRecordContextTitle"><h3 id="deskRecordContextTitle">현재 원본</h3><dl><dt>학생</dt><dd id="deskRecordOriginalStudent"></dd><dt>기록 날짜</dt><dd id="deskRecordOriginalDate"></dd><dt>수업·상황</dt><dd id="deskRecordOriginalContext"></dd></dl><p id="deskRecordOriginalNote">기존 연결과 비공개 항목은 유지합니다.</p><div class="desk-record-original" id="deskRecordOriginalText"></div></section>
    <form id="deskRecordForm" novalidate><fieldset id="deskRecordFields"><label id="deskRecordStudentField"><span>학생 *</span><select id="deskRecordStudent" required aria-describedby="deskRecordRosterStatus"><option value="">학생 선택</option></select><small id="deskRecordRosterStatus"></small></label><label id="deskRecordDateField"><span>상담 날짜 *</span><input id="deskRecordDate" type="date" required aria-describedby="deskRecordDateHelp"><span id="deskRecordFixedDate" hidden></span><small id="deskRecordDateHelp">한국 시간(KST) 기준 · 실제 상담이 있었던 날짜</small></label><label><span id="deskRecordTextLabel">상담 내용 *</span><textarea id="deskRecordText" rows="7" required maxlength="5000" aria-describedby="deskRecordTextHelp"></textarea><small id="deskRecordTextHelp">5000자 이내</small></label><div class="desk-grid-two"><label><span>상태</span><select id="deskRecordState"></select></label><label><span id="deskRecordTagsLabel">상담 주제</span><input id="deskRecordTags" type="text" maxlength="487" placeholder="쉼표로 구분" /><small id="deskRecordTagsHelp"></small></label></div><label id="deskRecordFollowUpField"><span>후속 지도 내용</span><textarea id="deskRecordFollowUp" rows="3" maxlength="2000"></textarea><small>2000자 이내</small></label><label id="deskRecordReasonField" hidden><span>정정 사유 *</span><input id="deskRecordReason" type="text" maxlength="500" placeholder="정정하는 이유를 입력하세요" /><small>최초 기록과 정정 사유·증빙 이력을 보존합니다.</small></label></fieldset><p class="desk-record-status" id="deskRecordStatus" role="status" aria-live="polite"></p><footer><span>이 PC에 저장 · 실제 기록 재조회 후 확인<br>백업·다른 PC 반영 상태는 별도로 확인합니다.</span><button class="desk-primary" type="submit" id="deskRecordSave">상담 기록 저장</button></footer></form></div>
    <section id="deskRecordConflict" aria-labelledby="deskRecordConflictTitle" hidden><h3 id="deskRecordConflictTitle">최신 원본과 현재 입력을 비교하세요</h3><p>원본이 바뀌었습니다. 현재 입력은 이 창에 보존되어 있습니다. 최신 원본을 확인하기 전에는 다시 저장하지 않습니다.</p><div class="desk-grid-two"><section><h3>최신 저장본</h3><div id="deskRecordLatestText" class="desk-record-original"></div></section><section><h3>현재 입력</h3><div id="deskRecordDraftText" class="desk-record-original"></div></section></div><label><input id="deskRecordConflictReviewed" type="checkbox">최신 원본과 현재 입력을 비교했습니다.</label><div class="desk-actions"><button class="desk-outline" type="button" id="deskRecordReadLatest">최신 기록 다시 읽기</button><button class="desk-primary" type="button" id="deskRecordRetry">확인 후 다시 저장</button></div></section>
    <div class="desk-actions" style="padding:0 24px 20px" id="deskRecordRecoveryActions" hidden><button type="button" class="desk-outline" data-app-view-target="health">연결·저장 상태</button></div>
    <aside class="desk-record-guide" id="deskRecordGuide" hidden><strong id="deskRecordGuideTitle"></strong><p id="deskRecordGuideCopy"></p><div class="desk-actions"><button type="button" id="deskRecordGuidePrevious">이전</button><button type="button" id="deskRecordGuideNext">다음</button><button type="button" id="deskRecordGuideClose">안내 닫기</button></div></aside></dialog>`);
  const el = <T extends HTMLElement>(id: string) => {
    const node = document.getElementById(id);
    if (!node) throw new Error(`missing record editor element: ${id}`);
    return node as T;
  };
  const dialog = el<HTMLDialogElement>('deskRecordDialog');
  const form = el<HTMLFormElement>('deskRecordForm');
  const text = el<HTMLTextAreaElement>('deskRecordText');
  const tags = el<HTMLInputElement>('deskRecordTags');
  const state = el<HTMLSelectElement>('deskRecordState');
  const followUp = el<HTMLTextAreaElement>('deskRecordFollowUp');
  const reason = el<HTMLInputElement>('deskRecordReason');
  const student = el<HTMLSelectElement>('deskRecordStudent');
  const date = el<HTMLInputElement>('deskRecordDate');
  const fieldset = el<HTMLFieldSetElement>('deskRecordFields');
  const save = el<HTMLButtonElement>('deskRecordSave');
  const reviewed = el<HTMLInputElement>('deskRecordConflictReviewed');
  const creationGuard = document.createElement('p');
  creationGuard.id = 'deskRecordCreationGuard';
  creationGuard.textContent = '저장 요청 이후 학생과 상담 날짜는 고정됩니다. 같은 상담의 저장 여부를 확인한 뒤 새 기록을 작성하세요.';
  el('deskRecordDateField').append(creationGuard);
  for (const node of [student, date, text, state, reason]) {
    const error = document.createElement('small');
    error.id = `${node.id}Error`; error.className = 'desk-record-field-error';
    node.closest('label')!.append(error);
    node.setAttribute('aria-describedby', [node.getAttribute('aria-describedby'), error.id].filter(Boolean).join(' '));
  }
  const privacyToggle = createStudentPrivacyToggle();
  privacyToggle.id = 'deskRecordPrivacy';
  el('deskRecordHeaderActions').prepend(privacyToggle);
  const blank = (): TeacherRecordFields => ({ text: '', tags: '', status: 'completed', followUpNote: '', correctionReason: '', studentCode: '', counselingDate: recordDateKst(Date.now()) });
  let draft = blank();
  let locator: TeacherRecordLocator | null = null;
  let original: TeacherRecord = {};
  let revision: string | number = 0;
  let creating = false;
  let creationSubmitted = false;
  let busy = false;
  let ready = false;
  let dirty = false;
  let composing = false;
  let compositionEndedAt = 0;
  let generation = 0;
  let mutationId = crypto.randomUUID();
  let tutorialIndex = -1;
  let roster: RosterStudent[] = [];
  let rosterIds = new Set<string>();
  let rosterCopy = '';
  let renderedPrivacy = isStudentPrivacyEnabled();
  let renderedRoster = '';
  let saveFailed = false;
  let conflict: RecordResult | null = null;
  let statusCopy = '';
  let statusFailed = false;
  const capture = () => { draft = { text: text.value, tags: tags.value, status: state.value, followUpNote: followUp.value, correctionReason: reason.value, studentCode: creating ? (creationSubmitted ? draft.studentCode : student.value) : String(original.studentCode || ''), counselingDate: creating && !creationSubmitted ? date.value : draft.counselingDate }; };
  const kind = () => locator?.kind || 'counseling';
  const bodyText = (record: TeacherRecord) => String(record[kind() === 'observation' ? 'note' : 'summary'] || '');
  const dateText = (record: TeacherRecord) => kind() === 'counseling' ? recordDateKst(record.counselingAtMs) : (typeof record.date === 'string' ? record.date : '');
  const compareText = (record: TeacherRecord) => {
    const labels = kind() === 'observation' ? OBSERVATION_STATES : COUNSELING_STATES;
    const label = labels.find(([value]) => value === record.status)?.[1] || '상태 미확인';
    const values = record[kind() === 'observation' ? 'tags' : 'topics'];
    return `기록 날짜: ${dateText(record) || '미확인'}\n${label}\n${bodyText(record)}${Array.isArray(values) && values.length ? `\n\n${kind() === 'observation' ? '태그' : '상담 주제'}: ${values.join(', ')}` : ''}${kind() === 'counseling' && record.followUpNote ? `\n\n후속 지도: ${String(record.followUpNote)}` : ''}`;
  };
  function render() {
    const privacy = isStudentPrivacyEnabled();
    const restore = isDeskRestoreBlocked();
    const locked = privacy || restore || busy || !ready;
    fieldset.disabled = locked;
    student.disabled = locked || (creating && creationSubmitted);
    date.disabled = locked || (creating && creationSubmitted);
    creationGuard.hidden = !creating || !creationSubmitted;
    form.setAttribute('aria-busy', String(busy));
    const invalid = validateTeacherRecordFields(kind(), draft, { creating, rosterIds }).length > 0;
    const fieldErrors: Array<[HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement, string]> = [
      [student, creating && !rosterIds.has(draft.studentCode) ? '현재 학급 명단에서 학생을 선택하세요.' : ''],
      [date, creating && counselingDateToMs(draft.counselingDate || '') === null ? '실제 상담 날짜를 입력하세요.' : ''],
      [text, !draft.text.trim() ? `${kind() === 'observation' ? '관찰' : '상담'} 내용을 입력하세요.` : ''],
      [state, !(kind() === 'observation' ? OBSERVATION_STATES : COUNSELING_STATES).some(([value]) => value === draft.status) ? '상태를 선택하세요.' : ''],
      [reason, kind() === 'observation' && !draft.correctionReason.trim() ? '정정 사유를 입력하세요.' : ''],
    ];
    for (const [node, error] of fieldErrors) {
      const visibleError = !privacy && ready && dirty ? error : '';
      el(`${node.id}Error`).textContent = visibleError;
      node.setAttribute('aria-invalid', String(!!visibleError));
    }
    save.disabled = locked || composing || conflict !== null || invalid;
    el<HTMLButtonElement>('deskRecordClose').disabled = busy || composing || restore;
    el<HTMLButtonElement>('deskRecordHelp').disabled = busy;
    el<HTMLButtonElement>('deskRecordReadLatest').disabled = locked;
    reviewed.disabled = locked || composing;
    el<HTMLButtonElement>('deskRecordRetry').disabled = locked || composing || !conflict?.record || !reviewed.checked || invalid;
    el('deskRecordPrivacyNotice').hidden = !privacy;
    el('deskRecordConflict').hidden = !conflict;
    el('deskRecordRecoveryActions').hidden = !statusFailed || busy;
    el('deskRecordStudentField').hidden = !creating;
    el('deskRecordReasonField').hidden = kind() !== 'observation';
    el('deskRecordFollowUpField').hidden = kind() === 'observation';
    el('deskRecordDateField').hidden = kind() === 'observation';
    date.hidden = !creating; date.required = creating; date.readOnly = !creating;
    el('deskRecordFixedDate').hidden = creating;
    student.required = creating; reason.required = kind() === 'observation';
    const rosterKey = `${privacy}:${generation}:${roster.map(item => item.id).join(',')}`;
    if (rosterKey !== renderedRoster) {
      student.replaceChildren(new Option(privacy ? '가림 해제 후 학생 선택' : '학생 선택', ''), ...(privacy ? [] : roster.map(item => new Option(`${item.classNo ? `${item.classNo}번 ` : ''}${item.displayName}`, item.id))));
      renderedRoster = rosterKey;
    }
    for (const [node, value] of [[text, draft.text], [tags, draft.tags], [state, draft.status], [followUp, draft.followUpNote], [reason, draft.correctionReason], [student, draft.studentCode], [date, draft.counselingDate || '']] as const) {
      const next = privacy ? '' : value;
      if (node.value !== next) node.value = next;
    }
    const studentName = creating ? roster.find(item => item.id === draft.studentCode)?.displayName : String(original.studentName || original.studentCode || '학생 미확인');
    el('deskRecordOriginalStudent').textContent = privacy ? '학생 정보 가림' : studentName || '학생을 선택하세요';
    el('deskRecordOriginalDate').textContent = privacy ? '가림 해제 후 확인' : (creating ? draft.counselingDate : dateText(original)) || '기록 날짜 미확인';
    const context = kind() === 'observation' ? [original.contextLabel || original.contextType, original.period ? `${original.period}교시` : '', original.subject].filter(Boolean).join(' · ') : '교사 상담 기록';
    el('deskRecordOriginalContext').textContent = privacy ? '가림 해제 후 확인' : context || '상황 미확인';
    el('deskRecordOriginalText').textContent = privacy ? '기록 원문이 가려져 있습니다.' : creating ? '선택한 학생과 실제 상담 날짜를 확인한 뒤 작성하세요.' : compareText(original);
    el('deskRecordFixedDate').textContent = privacy ? '가림 해제 후 확인' : dateText(original) || '기록 날짜 미확인';
    el('deskRecordLatestText').textContent = privacy ? '기록 원문이 가려져 있습니다.' : conflict?.record ? compareText(conflict.record) : '최신 기록을 다시 읽어 주세요.';
    const draftRecord: TeacherRecord = { date: original.date, counselingAtMs: creating ? counselingDateToMs(draft.counselingDate || '') : original.counselingAtMs, [kind() === 'observation' ? 'note' : 'summary']: draft.text, status: draft.status, [kind() === 'observation' ? 'tags' : 'topics']: draft.tags.split(',').map(value => value.trim()).filter(Boolean), followUpNote: draft.followUpNote };
    el('deskRecordDraftText').textContent = privacy ? '입력 내용이 가려져 있습니다.' : compareText(draftRecord);
    el('deskRecordContextTitle').textContent = creating ? '상담 대상' : '현재 원본 · 읽기 전용';
    el('deskRecordOriginalNote').textContent = kind() === 'observation' ? '최초 기록·학생·발생 일자·수업 연결과 증빙은 유지합니다.' : creating ? '현재 학급 명단에서 실제 학생을 선택합니다.' : '학생·기존 상담 날짜·비공개 교사 메모와 기존 연결은 유지합니다.';
    el('deskRecordRosterStatus').textContent = rosterCopy;
    el('deskRecordMeta').textContent = '이 PC의 학생 기록 · 저장 후 재조회';
    el('deskRecordStatus').textContent = restore ? '백업 복원 중에는 기록을 저장할 수 없습니다. 현재 입력은 유지됩니다.' : statusCopy;
    el('deskRecordStatus').classList.toggle('is-error', restore || statusFailed);
    save.textContent = busy ? '저장·재조회 확인 중…' : saveFailed ? '다시 저장' : kind() === 'observation' ? '정정 저장' : creating ? '상담 기록 저장' : '수정 저장';
    renderedPrivacy = privacy;
  }
  const message = (copy: string, failed = false) => { statusCopy = copy; statusFailed = failed; render(); };
  const setBusy = (value: boolean) => { busy = value; render(); };
  const errorMessage = (error: unknown) => {
    const code = String((error as Error)?.message || error || '');
    if (/counseling_date_unverified/.test(code)) return '현재 원본의 상담 날짜를 확인하지 못해 수정할 수 없습니다. 날짜를 추정하지 않습니다. 원본을 확인한 뒤 다시 열어 주세요.';
    if (/roster|student/.test(code)) return '현재 학급 명단·학생을 확인하지 못했습니다. 입력은 이 창에 유지됩니다. 온라인 교사 홈에서 학급 명단을 확인하세요.';
    return '기록 저장 여부를 확인하지 못했습니다. 현재 입력은 이 창의 메모리에 보존됩니다. 연결·저장 상태를 확인한 뒤 다시 시도하세요.';
  };
  async function readCanonical(target: TeacherRecordLocator, expectedStudent?: string): Promise<RecordResult> {
    const result = await invoke<RecordResult>('get_local_teacher_record', target);
    if (!verifyTeacherRecordPatch({}, result, target, expectedStudent)) throw new Error(result.error || 'record_readback_unverified');
    if (target.kind === 'counseling' && (typeof result.record!.counselingAtMs !== 'number' || result.record!.counselingAtMs <= 0 || !recordDateKst(result.record!.counselingAtMs))) throw new Error('counseling_date_unverified');
    return result;
  }
  function fill(record: TeacherRecord) {
    const values = record[kind() === 'observation' ? 'tags' : 'topics'];
    draft = { text: bodyText(record), tags: Array.isArray(values) ? values.join(', ') : '', status: String(record.status || (kind() === 'observation' ? 'none' : 'completed')), followUpNote: String(record.followUpNote || ''), correctionReason: '', studentCode: String(record.studentCode || ''), counselingDate: dateText(record) };
  }
  function closeGuide() {
    el('deskRecordGuide').hidden = true; tutorialIndex = -1;
    dialog.querySelectorAll('.desk-record-guide-target').forEach(node => node.classList.remove('desk-record-guide-target'));
  }
  function guide() {
    const steps = [
      { target: 'deskRecordPrivacy', title: '이름과 원문 가림', copy: '가림이 켜져 있으면 입력과 원문을 숨기고 잠급니다. 가림을 해제해도 이 창에 보관된 입력은 그대로 이어집니다.' },
      { target: 'deskRecordContext', title: '대상과 현재 원본', copy: '현재 학급의 학생과 실제 기록 날짜를 확인하세요. 기존 학생·날짜·수업 연결과 증빙은 읽기 전용으로 유지합니다.' },
      { target: 'deskRecordFields', title: '상담 수정과 관찰 정정', copy: '상담 본문과 후속 지도는 수정합니다. 관찰 정정에는 사유가 필요합니다. 새 상담 날짜는 한국 시간 기준입니다.' },
      { target: 'deskRecordSave', title: '저장 결과와 원본 충돌', copy: '실제 저장본을 다시 읽어 확인한 뒤 완료를 표시합니다. 원본이 바뀌면 최신본과 현재 입력을 비교하고 확인한 뒤 재시도하세요. 이 안내는 저장하지 않습니다.' },
    ];
    dialog.querySelectorAll('.desk-record-guide-target').forEach(node => node.classList.remove('desk-record-guide-target'));
    const step = steps[tutorialIndex];
    if (!step) { try { localStorage.setItem(TUTORIAL_KEY, 'complete'); } catch { /* Optional guide preference. */ } closeGuide(); return; }
    el(step.target).classList.add('desk-record-guide-target'); el(step.target).scrollIntoView({ block: 'nearest' });
    el('deskRecordGuideTitle').textContent = `${tutorialIndex + 1} / ${steps.length} · ${step.title}`;
    el('deskRecordGuideCopy').textContent = step.copy;
    el<HTMLButtonElement>('deskRecordGuidePrevious').disabled = tutorialIndex === 0;
    el('deskRecordGuideNext').textContent = tutorialIndex === steps.length - 1 ? '완료' : '다음'; el('deskRecordGuide').hidden = false;
  }
  const canLeave = async () => {
    if (!dialog.open) return true;
    if (busy || composing || isDeskRestoreBlocked()) return false;
    if (dirty && !window.confirm('저장 결과가 확인되지 않은 입력이 있습니다. 이 창의 입력을 버리고 닫을까요?')) return false;
    closeGuide(); dialog.close(); dirty = false; return true;
  };
  const open = async (target?: TeacherRecordLocator, selectedStudent?: string) => {
    if (!await canLeave()) return;
    // Two queued entry events can both observe a closed dialog before this await resumes.
    if (dialog.open || busy || composing) return;
    const tenantId = options.getTenantId();
    if (!tenantId || (target && target.tenantId !== tenantId) || isDeskRestoreBlocked()) return;
    const requestGeneration = ++generation;
    creating = !target;
    locator = target || { tenantId, kind: 'counseling', recordId: `counseling-${crypto.randomUUID()}` };
    original = {}; revision = 0; dirty = false; ready = false; conflict = null; reviewed.checked = false; saveFailed = false; composing = false; creationSubmitted = false;
    mutationId = crypto.randomUUID(); roster = []; rosterIds = new Set(); rosterCopy = ''; draft = blank();
    const observation = kind() === 'observation';
    text.maxLength = observation ? 1000 : 5000; tags.maxLength = observation ? 1219 : 487;
    el('deskRecordTitle').textContent = observation ? '관찰 내용 정정' : creating ? '새 상담 기록' : '상담 기록 수정';
    el('deskRecordTextLabel').textContent = observation ? '관찰 내용 *' : '상담 내용 *';
    el('deskRecordTextHelp').textContent = `${observation ? 1000 : 5000}자 이내`;
    el('deskRecordTagsLabel').textContent = observation ? '태그' : '상담 주제';
    el('deskRecordTagsHelp').textContent = `${observation ? 20 : 8}개까지 · 각 60자 이내 · 쉼표로 구분`;
    state.replaceChildren(...(observation ? OBSERVATION_STATES : COUNSELING_STATES).map(([value, label]) => new Option(label, value)));
    statusCopy = '현재 기록과 학급 명단을 확인하고 있습니다.'; statusFailed = false; dialog.showModal(); setBusy(true);
    try {
      if (creating) {
        const result = await invoke<RosterResult>('get_quick_observation_context', { tenantId });
        if (requestGeneration !== generation || options.getTenantId() !== tenantId) return;
        if (!result.ok || result.tenantId !== tenantId || !Array.isArray(result.roster?.students)) throw new Error(result.error || 'roster_missing');
        roster = result.roster.students.filter(item => item.status !== 'archived' && typeof item.id === 'string' && !!item.id);
        rosterIds = new Set(roster.map(item => item.id));
        rosterCopy = result.roster.stale ? '보관된 오래된 명단입니다. 학급·학생이 맞는지 확인하세요.' : '이 PC에 보관된 현재 학급 명단';
        if (!roster.length) throw new Error('roster_empty');
        if (selectedStudent && rosterIds.has(selectedStudent)) draft.studentCode = selectedStudent;
      } else {
        const result = await readCanonical(locator);
        if (requestGeneration !== generation || options.getTenantId() !== tenantId) return;
        original = result.record!; revision = result.revision!; fill(original);
      }
      ready = true;
      message(observation ? '정정 내용과 사유를 입력하세요. 최초 기록과 증빙 이력은 보존합니다.' : creating ? '현재 학급의 학생과 실제 상담 날짜를 선택해 작성하세요.' : '본문·상태·상담 주제·후속 지도만 수정합니다. 학생·상담 날짜와 비공개 항목은 유지합니다.');
      if (!isStudentPrivacyEnabled()) text.focus();
      try { if (localStorage.getItem(TUTORIAL_KEY) !== 'complete') { tutorialIndex = 0; guide(); } } catch { /* Editing does not depend on localStorage. */ }
    } catch (error) { message(errorMessage(error), true); }
    finally { if (requestGeneration === generation) setBusy(false); }
  };
  const presentConflict = (result?: RecordResult) => {
    conflict = result || {}; reviewed.checked = false;
    message('원본이 변경되었습니다. 현재 입력은 이 창에 보존됩니다. 최신 원본과 입력을 비교하고 확인한 뒤 다시 저장하세요.', true);
  };
  const refreshLatest = async () => {
    if (!locator || busy || composing || isStudentPrivacyEnabled() || isDeskRestoreBlocked() || locator.tenantId !== options.getTenantId()) return;
    setBusy(true);
    try { presentConflict(await readCanonical(locator, creating ? draft.studentCode : String(original.studentCode || ''))); }
    catch { presentConflict(); message('최신 원본을 읽지 못했습니다. 입력은 유지됩니다. 연결 상태를 확인한 뒤 다시 읽어 주세요.', true); }
    finally { setBusy(false); }
  };
  async function saveRecord(retryReviewed = false) {
    if (busy || composing || Date.now() - compositionEndedAt < 150 || !ready || !locator || locator.tenantId !== options.getTenantId() || isStudentPrivacyEnabled() || isDeskRestoreBlocked()) return;
    if (conflict && (!retryReviewed || !reviewed.checked || !conflict.record)) return;
    const current = { ...draft };
    const errors = validateTeacherRecordFields(locator.kind, current, { creating, rosterIds });
    if (errors.length) { message(errors.join(' '), true); return; }
    const target = { ...locator };
    const expectedStudent = creating ? current.studentCode : String(original.studentCode || '');
    const comparedRevision = conflict?.revision;
    const comparedRecord = conflict?.record;
    setBusy(true); message('기록을 저장하고 실제 저장본을 다시 확인하고 있습니다.');
    try {
      if (!creating || retryReviewed) {
        const fresh = await readCanonical(target, expectedStudent);
        if (retryReviewed) {
          if (fresh.revision !== comparedRevision || canonicalRecordValue(fresh.record) !== canonicalRecordValue(comparedRecord)) { presentConflict(fresh); message('비교한 뒤 원본이 다시 변경되었습니다. 최신 원본을 다시 비교하고 확인하세요. 입력은 유지됩니다.', true); return; }
          if (creating && fresh.record!.counselingAtMs !== counselingDateToMs(current.counselingDate || '')) { presentConflict(fresh); message('발견된 상담의 날짜가 현재 입력과 다릅니다. 학생·날짜를 추정해 바꾸지 않습니다. 현재 입력을 보관하고 원본을 확인하세요.', true); return; }
          // Only this explicit review action may adopt a new canonical CAS baseline.
          original = fresh.record!; revision = fresh.revision!; creating = false; conflict = null; reviewed.checked = false; mutationId = crypto.randomUUID();
        } else if (fresh.revision !== revision || canonicalRecordValue(fresh.record) !== canonicalRecordValue(original)) { presentConflict(fresh); return; }
      }
      const patch = teacherRecordPatch(target.kind, original, current, { creating });
      if (!Object.keys(patch).length) { dirty = false; saveFailed = false; message('입력과 현재 저장본이 일치합니다. 변경한 내용이 없습니다.'); return; }
      if (isDeskRestoreBlocked() || target.tenantId !== options.getTenantId()) throw new Error('record_context_changed');
      if (creating) { creationSubmitted = true; render(); }
      const result = await invoke<RecordResult>('save_local_teacher_record', { input: { ...target, expectedRevision: revision, mutationId, patch, ...(creating ? { studentCode: current.studentCode } : {}), ...(target.kind === 'observation' ? { correctionReason: current.correctionReason.trim() } : {}) } });
      if (!result.ok || result.verified !== true || !verifyTeacherRecordPatch(patch, result, target, expectedStudent)) throw new Error(result.error || 'record_save_unverified');
      const readback = await readCanonical(target, expectedStudent);
      if (!verifyTeacherRecordPatch(patch, readback, target, expectedStudent) || readback.revision !== result.revision) throw new Error('record_readback_conflict');
      if (target.tenantId !== options.getTenantId()) throw new Error('record_context_changed');
      original = readback.record!; revision = readback.revision!; creating = false; dirty = false; saveFailed = false; conflict = null; reviewed.checked = false;
      fill(original); mutationId = crypto.randomUUID();
      message('이 PC에 저장했고 실제 기록에서 변경 내용을 확인했습니다.'); options.onChanged?.();
    } catch (error) {
      saveFailed = true;
      if (/revision|conflict/.test(String((error as Error)?.message || error))) {
        try { presentConflict(await readCanonical(target, expectedStudent)); }
        catch { presentConflict(); message('최신 원본을 읽지 못했습니다. 현재 입력은 보존됩니다. 최신 기록을 다시 읽고 비교해 주세요.', true); }
      } else message(errorMessage(error), true);
    } finally { setBusy(false); }
  }
  const change = () => {
    if (busy || !ready || isStudentPrivacyEnabled() || isDeskRestoreBlocked()) return;
    capture(); dirty = true; mutationId = crypto.randomUUID(); reviewed.checked = false; render();
  };
  form.addEventListener('input', change); form.addEventListener('change', change);
  form.addEventListener('compositionstart', () => { composing = true; render(); });
  form.addEventListener('compositionend', () => { composing = false; compositionEndedAt = Date.now(); change(); render(); });
  form.addEventListener('keydown', event => { if (event.key === 'Enter' && (event.isComposing || composing || event.keyCode === 229)) event.preventDefault(); });
  form.addEventListener('submit', event => { event.preventDefault(); void saveRecord(); });
  reviewed.addEventListener('change', render);
  el('deskRecordReadLatest').addEventListener('click', () => void refreshLatest());
  el('deskRecordRetry').addEventListener('click', () => void saveRecord(true));
  el('deskRecordClose').addEventListener('click', () => void canLeave());
  dialog.addEventListener('cancel', event => { event.preventDefault(); void canLeave(); });
  el('deskRecordHelp').addEventListener('click', () => { tutorialIndex = 0; guide(); });
  el('deskRecordGuideNext').addEventListener('click', () => { tutorialIndex += 1; guide(); });
  el('deskRecordGuidePrevious').addEventListener('click', () => { tutorialIndex = Math.max(0, tutorialIndex - 1); guide(); });
  el('deskRecordGuideClose').addEventListener('click', closeGuide);
  window.addEventListener('desk:student-privacy-changed', () => {
    if (!renderedPrivacy && isStudentPrivacyEnabled() && dialog.open && !busy) capture();
    if (isStudentPrivacyEnabled()) composing = false;
    reviewed.checked = false; render();
  });
  window.addEventListener('desk:restore-lock-changed', () => { reviewed.checked = false; render(); });
  window.addEventListener('beforeunload', event => { if (dialog.open && (dirty || busy || composing)) { event.preventDefault(); event.returnValue = ''; } });
  window.addEventListener('desk:edit-record', event => { const detail = (event as CustomEvent<TeacherRecordLocator>).detail; if (detail?.tenantId && ['observation', 'counseling'].includes(detail.kind) && detail.recordId) void open(detail); });
  window.addEventListener('desk:create-counseling', event => { const detail = (event as CustomEvent<{ studentId?: unknown }>).detail; void open(undefined, typeof detail?.studentId === 'string' ? detail.studentId : undefined); });
  return { canLeave };
}
