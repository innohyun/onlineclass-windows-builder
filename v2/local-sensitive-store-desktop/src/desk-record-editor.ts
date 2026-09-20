import { invoke } from '@tauri-apps/api/core';
import { type LocalDataRecord } from './data-explorer';

export type TeacherRecordKind = 'observation' | 'counseling';
export type TeacherRecordLocator = { tenantId: string; kind: TeacherRecordKind; recordId: string };
const TUTORIAL_KEY = 'localTeacherRecordEditorTutorial:v1';
type TeacherRecord = Record<string, unknown>;
type RecordResult = { ok?: boolean; record?: TeacherRecord; revision?: string | number; verified?: boolean; error?: string };
type Fields = { text: string; tags: string; status: string; followUpNote: string; correctionReason: string; studentCode: string };

export function teacherRecordLocator(tenantId: string, record?: LocalDataRecord): TeacherRecordLocator | null {
  if (!tenantId || !record) return null;
  if (record.sectionKey === 'observations' && typeof record.payload.docId === 'string' && record.payload.docId.trim()) return { tenantId, kind: 'observation', recordId: record.payload.docId };
  if (record.sectionKey === 'teacher-counseling-sessions' && typeof record.payload.sessionId === 'string' && record.payload.sessionId.trim()) return { tenantId, kind: 'counseling', recordId: record.payload.sessionId };
  return null;
}
export function teacherRecordPatch(kind: TeacherRecordKind, original: TeacherRecord, fields: Fields) {
  const tags = fields.tags.split(',').map(value => value.trim()).filter(Boolean);
  const textKey = kind === 'observation' ? 'note' : 'summary';
  const tagsKey = kind === 'observation' ? 'tags' : 'topics';
  const candidate: TeacherRecord = { [textKey]: fields.text.trim(), [tagsKey]: tags, status: fields.status };
  if (kind === 'counseling') candidate.followUpNote = fields.followUpNote;
  const patch: TeacherRecord = {};
  for (const [key, value] of Object.entries(candidate)) {
    const previous = original[key] ?? (Array.isArray(value) ? [] : '');
    if (JSON.stringify(value) !== JSON.stringify(previous)) patch[key] = value;
  }
  return patch;
}
export function verifyTeacherRecordPatch(patch: TeacherRecord, result: RecordResult) {
  return Boolean(result.ok && result.record && result.revision != null && Object.entries(patch).every(([key, value]) => JSON.stringify(result.record![key] ?? (Array.isArray(value) ? [] : '')) === JSON.stringify(value)));
}
export function updateTeacherRecordAction(container: HTMLElement, record: LocalDataRecord | undefined, tenantId: string) {
  container.querySelector('.desk-record-edit-entry')?.remove();
  const locator = teacherRecordLocator(tenantId, record);
  if (!locator) return;
  const button = document.createElement('button');
  button.type = 'button'; button.className = 'desk-outline desk-record-edit-entry';
  button.textContent = locator.kind === 'observation' ? '관찰 내용 정정' : '상담 기록 편집';
  button.addEventListener('click', () => window.dispatchEvent(new CustomEvent('desk:edit-record', { detail: locator })));
  container.prepend(button);
}

export function initDeskRecordEditor(options: { getTenantId: () => string; onChanged?: () => void }) {
  document.body.insertAdjacentHTML('beforeend', `<dialog class="desk-dialog desk-record-dialog" id="deskRecordDialog" aria-labelledby="deskRecordTitle"><header><div><h2 id="deskRecordTitle">기록 편집</h2><p id="deskRecordMeta">현재 저장본을 확인합니다.</p></div><div class="desk-actions"><button type="button" id="deskRecordHelp" aria-label="기록 작성 안내"><i class="fa-solid fa-circle-question" aria-hidden="true"></i></button><button type="button" id="deskRecordClose" aria-label="기록 편집 닫기"><i class="fa-solid fa-xmark" aria-hidden="true"></i></button></div></header><form id="deskRecordForm"><fieldset id="deskRecordFields"><label id="deskRecordStudentField"><span>학생</span><select id="deskRecordStudent" required><option value="">학급 명단 확인 중</option></select><small id="deskRecordRosterStatus"></small></label><label><span id="deskRecordTextLabel">기록 내용</span><textarea id="deskRecordText" rows="8" required maxlength="50000"></textarea></label><div class="desk-grid-two"><label><span>상태</span><select id="deskRecordState"></select></label><label><span id="deskRecordTagsLabel">태그</span><input id="deskRecordTags" type="text" maxlength="600" placeholder="쉼표로 구분" /></label></div><label id="deskRecordFollowUpField"><span>후속 지도 내용</span><textarea id="deskRecordFollowUp" rows="3" maxlength="20000"></textarea></label><label id="deskRecordReasonField"><span>정정 사유</span><input id="deskRecordReason" type="text" maxlength="500" placeholder="기존 기록을 정정하는 이유" /></label></fieldset><p class="desk-record-status" id="deskRecordStatus" role="status"></p><footer><span>이 PC 정본 · 저장 후 재조회</span><button class="desk-primary" type="submit" id="deskRecordSave">저장</button></footer></form><aside class="desk-record-guide" id="deskRecordGuide" hidden><strong id="deskRecordGuideTitle"></strong><p id="deskRecordGuideCopy"></p><button type="button" id="deskRecordGuideNext">다음</button></aside></dialog>`);
  const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
  const dialog = el<HTMLDialogElement>('deskRecordDialog');
  const form = el<HTMLFormElement>('deskRecordForm');
  const text = el<HTMLTextAreaElement>('deskRecordText');
  const tags = el<HTMLInputElement>('deskRecordTags');
  const state = el<HTMLSelectElement>('deskRecordState');
  const followUp = el<HTMLTextAreaElement>('deskRecordFollowUp');
  const reason = el<HTMLInputElement>('deskRecordReason');
  const student = el<HTMLSelectElement>('deskRecordStudent');
  const fieldset = el<HTMLFieldSetElement>('deskRecordFields');
  const save = el<HTMLButtonElement>('deskRecordSave');
  let locator: TeacherRecordLocator | null = null;
  let original: TeacherRecord = {};
  let revision: string | number = 0;
  let creating = false;
  let loading = false;
  let dirty = false;
  let generation = 0;
  let mutationId = crypto.randomUUID();
  let tutorialIndex = -1;
  let rosterIds = new Set<string>();
  const fields = (): Fields => ({ text: text.value, tags: tags.value, status: state.value, followUpNote: followUp.value, correctionReason: reason.value.trim(), studentCode: student.value });
  const message = (copy: string, failed = false) => { el('deskRecordStatus').textContent = copy; el('deskRecordStatus').classList.toggle('is-error', failed); };
  const setBusy = (busy: boolean) => { loading = busy; fieldset.disabled = busy; save.disabled = busy; el<HTMLButtonElement>('deskRecordClose').disabled = busy; };
  const errorMessage = (error: unknown) => {
    const code = String((error as Error)?.message || error || '');
    if (/revision|conflict/.test(code)) return '다른 화면에서 기록이 변경되었습니다. 입력한 내용은 그대로 두었습니다. 현재 창의 내용을 따로 보관한 뒤 최신 기록을 다시 열어 주세요.';
    if (/roster|student/.test(code)) return '학생 명단을 확인하지 못했습니다. 입력은 유지됩니다. 연결된 학급 명단을 확인한 뒤 다시 시도해 주세요.';
    return '기록 저장 또는 재조회를 확인하지 못했습니다. 입력은 유지됩니다. 연결·저장 상태를 확인한 뒤 다시 시도해 주세요.';
  };
  function fill(record: TeacherRecord) {
    text.value = String(record[locator?.kind === 'observation' ? 'note' : 'summary'] || '');
    const values = record[locator?.kind === 'observation' ? 'tags' : 'topics'];
    tags.value = Array.isArray(values) ? values.join(', ') : '';
    state.value = String(record.status || (locator?.kind === 'observation' ? 'none' : 'completed'));
    if (!state.value && record.status) { const option = new Option(String(record.status), String(record.status)); state.add(option); state.value = option.value; }
    followUp.value = String(record.followUpNote || ''); reason.value = '';
  }
  function closeGuide() {
    el('deskRecordGuide').hidden = true; tutorialIndex = -1;
    dialog.querySelectorAll('.desk-record-guide-target').forEach(node => node.classList.remove('desk-record-guide-target'));
  }
  function guide() {
    const steps = [
      { target: 'deskRecordMeta', title: '대상과 현재 저장본', copy: '정정은 조회한 기록의 식별자와 버전을 기준으로 합니다. 다른 화면에서 바뀐 기록은 덮어쓰지 않습니다.' },
      { target: 'deskRecordText', title: '필요한 내용만 편집', copy: '본문과 표시된 항목만 수정합니다. 학생 식별자, 비공개 필드와 기존 증빙은 유지합니다. 관찰 정정에는 사유가 필요합니다.' },
      { target: 'deskRecordSave', title: '저장과 실제 결과 확인', copy: '저장 버튼은 정식 기록 저장 경로를 사용합니다. 실제 저장본을 다시 읽어 확인한 뒤 완료를 표시합니다. 이 안내는 저장하지 않습니다.' },
    ];
    dialog.querySelectorAll('.desk-record-guide-target').forEach(node => node.classList.remove('desk-record-guide-target'));
    const step = steps[tutorialIndex]; if (!step) { try { localStorage.setItem(TUTORIAL_KEY, 'complete'); } catch { /* Guide state is optional. */ } closeGuide(); return; }
    el(step.target).classList.add('desk-record-guide-target'); el(step.target).scrollIntoView({ block: 'nearest' });
    el('deskRecordGuideTitle').textContent = `${tutorialIndex + 1} / 3 · ${step.title}`;
    el('deskRecordGuideCopy').textContent = step.copy;
    el('deskRecordGuideNext').textContent = tutorialIndex === 2 ? '완료' : '다음'; el('deskRecordGuide').hidden = false;
  }
  const canLeave = async () => {
    if (!dialog.open) return true;
    if (loading) return false;
    if (dirty && !window.confirm('저장하지 않은 입력이 있습니다. 이 창의 입력을 버리고 닫을까요?')) return false;
    closeGuide(); dialog.close(); return true;
  };
  const open = async (target?: TeacherRecordLocator) => {
    if (!await canLeave()) return;
    const tenantId = options.getTenantId();
    const requestGeneration = ++generation;
    creating = !target;
    locator = target || { tenantId, kind: 'counseling', recordId: `counseling-${crypto.randomUUID()}` };
    if (!tenantId || locator.tenantId !== tenantId) { locator = null; return; }
    const observation = locator.kind === 'observation';
    text.maxLength = observation ? 1000 : 5000;
    followUp.maxLength = 2000;
    tags.maxLength = observation ? 1219 : 487;
    el('deskRecordTitle').textContent = observation ? '관찰 내용 정정' : creating ? '새 상담 기록' : '상담 기록 편집';
    el('deskRecordTextLabel').textContent = observation ? '관찰 내용' : '상담 내용';
    el('deskRecordTagsLabel').textContent = observation ? '태그' : '상담 주제';
    el('deskRecordStudentField').hidden = !creating; student.required = creating;
    el('deskRecordReasonField').hidden = !observation; reason.required = observation;
    el('deskRecordFollowUpField').hidden = observation;
    state.replaceChildren(...(observation ? [['none', '일반'], ['good', '강점'], ['warning', '계속 관찰'], ['help', '도움 필요']] : [['completed', '상담 완료'], ['follow_up', '후속 지도']]).map(([value, label]) => new Option(label, value)));
    original = {}; revision = 0; dirty = false; mutationId = crypto.randomUUID(); fill({});
    el('deskRecordMeta').textContent = creating ? '현재 학급 명단을 확인해 학생을 선택하세요.' : '현재 저장본을 불러오는 중입니다.';
    message('현재 기록과 권한을 확인하고 있습니다.'); dialog.showModal(); setBusy(true);
    try {
      if (creating) {
        const result = await invoke<{ok?:boolean;tenantId?:string;roster?:{students:Array<{id:string;displayName:string;classNo?:number;status?:string}>;stale?:boolean; syncedAtMs?:number}}> ('get_quick_observation_context');
        if (requestGeneration !== generation) return;
        if (!result.ok || result.tenantId !== tenantId || !result.roster) throw new Error('roster_missing');
        const students = result.roster.students.filter(item => item.status !== 'archived');
        rosterIds = new Set(students.map(item => item.id));
        student.replaceChildren(new Option('학생 선택', ''), ...students.map(item => new Option(`${item.classNo ? `${item.classNo}번 ` : ''}${item.displayName}`, item.id)));
        el('deskRecordRosterStatus').textContent = result.roster.stale ? '보관된 명단입니다. 학급·학생이 맞는지 확인하세요.' : '이 PC에 보관된 현재 학급 명단';
        if (!students.length) throw new Error('roster_empty');
      } else {
        const result = await invoke<RecordResult>('get_local_teacher_record', locator);
        if (requestGeneration !== generation) return;
        if (!result.ok || !result.record || result.revision == null) throw new Error(result.error || 'record_missing');
        original = result.record; revision = result.revision; fill(original);
        el('deskRecordMeta').textContent = `${String(original.studentName || original.studentCode || '선택한 학생')} · ${observation ? '관찰 정정은 증빙 이력으로 보존합니다.' : '교사 상담 정본의 현재 내용입니다.'}`;
      }
      message(observation ? '정정 내용과 사유를 입력하세요. 최초 기록과 증빙은 보존합니다.' : '상담 내용과 후속 지도만 수정합니다. 비공개 교사 메모와 기존 연결은 유지합니다.');
      setBusy(false); text.focus();
      try { if (localStorage.getItem(TUTORIAL_KEY) !== 'complete') { tutorialIndex = 0; guide(); } } catch { /* Editing remains available without guide preferences. */ }
    } catch (error) { setBusy(false); save.disabled = true; message(errorMessage(error), true); }
  };
  form.addEventListener('input', () => { dirty = true; mutationId = crypto.randomUUID(); });
  form.addEventListener('change', () => { dirty = true; mutationId = crypto.randomUUID(); });
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    if (loading || !locator || locator.tenantId !== options.getTenantId() || !form.reportValidity()) return;
    if (creating && !rosterIds.has(student.value)) { message('현재 학급 명단에서 학생을 선택하세요.', true); return; }
    const current = fields();
    const tagValues = current.tags.split(',').map(value => value.trim()).filter(Boolean);
    const tagLimit = locator.kind === 'observation' ? 20 : 8;
    if (tagValues.length > tagLimit || tagValues.some(value => value.length > 60)) { message(`태그·주제는 ${tagLimit}개까지, 각 60자 이내로 입력하세요.`, true); return; }
    const patch = teacherRecordPatch(locator.kind, original, current);
    if (!Object.keys(patch).length) { message('변경한 내용이 없습니다.'); return; }
    setBusy(true); message('기록을 저장하고 실제 저장본을 다시 확인하고 있습니다.');
    try {
      const result = await invoke<RecordResult>('save_local_teacher_record', { input: { ...locator, expectedRevision: revision, mutationId, patch, ...(creating ? {studentCode: current.studentCode} : {}), ...(locator.kind === 'observation' ? {correctionReason: current.correctionReason} : {}) } });
      if (!result.ok || result.verified !== true || !result.record || result.revision == null) throw new Error(result.error || 'record_save_unverified');
      const readback = await invoke<RecordResult>('get_local_teacher_record', locator);
      if (!verifyTeacherRecordPatch(patch, readback) || readback.revision !== result.revision) throw new Error('record_readback_conflict');
      original = readback.record!; revision = readback.revision!; creating = false; dirty = false;
      mutationId = crypto.randomUUID(); el('deskRecordStudentField').hidden = true; student.required = false;
      message('이 PC에 저장했고 실제 기록에서 변경 내용을 확인했습니다.'); options.onChanged?.();
    } catch (error) { message(errorMessage(error), true); }
    finally { setBusy(false); }
  });
  el('deskRecordClose').addEventListener('click', () => void canLeave());
  dialog.addEventListener('cancel', (event) => { event.preventDefault(); void canLeave(); });
  el('deskRecordHelp').addEventListener('click', () => { tutorialIndex = 0; guide(); });
  el('deskRecordGuideNext').addEventListener('click', () => { tutorialIndex += 1; guide(); });
  window.addEventListener('beforeunload', event => { if (dirty && dialog.open) { event.preventDefault(); event.returnValue = ''; } });
  window.addEventListener('desk:edit-record', (event) => { const detail = (event as CustomEvent<TeacherRecordLocator>).detail; if (detail?.tenantId && ['observation','counseling'].includes(detail.kind) && detail.recordId) void open(detail); });
  window.addEventListener('desk:create-counseling', () => void open());
  return { canLeave };
}
