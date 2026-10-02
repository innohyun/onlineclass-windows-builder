import { createStudentPrivacyToggle, isStudentPrivacyEnabled } from './desk-privacy';
import { type initQuickObservation } from './quick-observation';
import { type initStudentTimeline } from './student-timeline';

type QuickController = ReturnType<typeof initQuickObservation>;
type TimelineController = ReturnType<typeof initStudentTimeline>;

/** This panel borrows the one existing composer. It never clones controls or writes records. */
export function initDeskStudentPanel(options: {
  quickObservation: QuickController;
  studentTimeline: TimelineController;
  navigate: (view: 'quick-observation') => Promise<boolean>;
}) {
  const form = document.getElementById('quickObservationForm') as HTMLFormElement;
  const homeParent = form.parentElement!;
  const homeNext = form.nextSibling;
  const column = document.querySelector<HTMLElement>('.student-detail-column')!;
  const slot = document.createElement('aside');
  slot.id = 'studentTimelineQuickObservationPanel';
  slot.className = 'student-quick-panel';
  slot.hidden = true;
  const header = document.createElement('header');
  const title = document.createElement('h2'); title.textContent = '빠른 관찰';
  const close = document.createElement('button'); close.type = 'button'; close.className = 'desk-outline'; close.textContent = '접기';
  header.append(title, close);
  const selected = document.createElement('p'); selected.className = 'student-quick-target';
  const multi = document.createElement('button'); multi.type = 'button'; multi.className = 'desk-outline'; multi.textContent = '여러 학생 선택';
  const boundary = document.createElement('p'); boundary.className = 'student-roster-notice'; boundary.textContent = '선택한 학생을 이어받습니다. 입력은 현재 창에 유지하며 이 PC의 정본을 확인한 뒤 저장 완료를 표시합니다.';
  slot.append(header, selected, createStudentPrivacyToggle(), multi, boundary);
  column.append(slot);
  let opened = false;

  const renderTarget = () => {
    const student = options.studentTimeline.getSelectedStudent();
    selected.textContent = student ? isStudentPrivacyEnabled() ? `학생 ${student.classNo ? String(student.classNo).padStart(2, '0') : '선택됨'} · 가림을 해제하면 입력할 수 있습니다.` : `${student.classNo ? `${student.classNo}번 · ` : ''}${student.studentName}` : '학생을 먼저 선택하세요.';
  };
  function restore() {
    if (form.parentElement === slot) homeParent.insertBefore(form, homeNext?.parentNode === homeParent ? homeNext : null);
    slot.hidden = true; opened = false; column.classList.remove('has-quick-panel');
  }
  async function open(studentIds: string[]) {
    if (options.quickObservation.getBusy()) return;
    if (!studentIds.length) { if (await options.navigate('quick-observation')) await options.quickObservation.open({focus:true}); return; }
    await options.quickObservation.open({studentIds});
    if (options.quickObservation.getSelectedStudentIds().length !== studentIds.length || studentIds.some(id => !options.quickObservation.getSelectedStudentIds().includes(id))) return;
    opened = true; slot.hidden = false; column.hidden = false; column.classList.add('has-quick-panel');
    document.querySelector('.student-timeline-workbench')?.classList.remove('is-detail-collapsed');
    slot.append(form); renderTarget();
  }
  close.addEventListener('click', async () => { if (await options.quickObservation.canLeave()) restore(); });
  multi.addEventListener('click', async () => {
    const ids = options.quickObservation.getSelectedStudentIds();
    if (await options.navigate('quick-observation')) { restore(); await options.quickObservation.open({studentIds:ids,focus:true}); }
  });
  window.addEventListener('desk:open-student-quick', event => void open((event as CustomEvent<{studentIds:string[]}>).detail?.studentIds || []));
  window.addEventListener('desk:student-selected', event => {
    renderTarget();
    if (opened) void options.quickObservation.open({studentIds:[(event as CustomEvent<{studentId:string}>).detail.studentId]});
  });
  window.addEventListener('desk:student-privacy-changed', renderTarget);
  return { onViewChange: (view: string) => { if (view !== 'students') restore(); }, open };
}
