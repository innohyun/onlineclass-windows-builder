/** Privacy starts enabled on each app launch. Sensitive drafts stay in their owning controller. */
let studentPrivacyEnabled = true;

export function isStudentPrivacyEnabled(): boolean {
  return studentPrivacyEnabled;
}

export function setStudentPrivacyEnabled(enabled: boolean): void {
  if (studentPrivacyEnabled === Boolean(enabled)) return;
  studentPrivacyEnabled = Boolean(enabled);
  window.dispatchEvent(new CustomEvent('desk:student-privacy-changed', { detail: { enabled: studentPrivacyEnabled } }));
}

export function createStudentPrivacyToggle(): HTMLButtonElement {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'desk-outline student-privacy-toggle';
  button.dataset.studentPrivacyToggle = '';
  button.setAttribute('role', 'switch');
  const render = () => {
    button.setAttribute('aria-checked', String(isStudentPrivacyEnabled()));
    button.textContent = `학생 기록 가림 ${isStudentPrivacyEnabled() ? 'ON' : 'OFF'}`;
    button.title = isStudentPrivacyEnabled() ? '이름·원문·첨부파일을 가리고 입력을 잠급니다. 해제하면 입력을 이어갑니다.' : '학생 이름과 기록 원문이 표시됩니다.';
  };
  button.addEventListener('click', () => setStudentPrivacyEnabled(!isStudentPrivacyEnabled()));
  window.addEventListener('desk:student-privacy-changed', render);
  render();
  return button;
}
