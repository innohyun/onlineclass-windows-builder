import type { BackupItem, BackupPreview } from './backup-types';
import { backupKindLabel } from './backup-storage';
import { formatBackupDateTime, backupCareCount, backupAttendanceCount, backupLearningCount, backupStudentRecordCount, backupBoardSnapshotCount, backupBoardMediaCount, backupArchiveCount, backupSourcePcName, backupSourceRelation, backupEnvironmentText, backupSourceListText, backupRowSummary } from './backup-display-utils';
import type { RestoreProgressState } from './backup-restore-progress';

const date = (value?: number) => value && Number.isFinite(value) ? new Intl.DateTimeFormat('ko-KR', { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }).format(new Date(value)) : '확인되지 않음';

export function initRestoreRenderer() {
  const panel = document.getElementById('backupRestorePanel');
  if (!panel || document.getElementById('backupRestoreProgress')) return;
  panel.insertAdjacentHTML('beforeend', '<section id="backupRestoreVerification" class="restore-verification" aria-label="선택 백업 검증"><h3>복원 전 확인</h3><p id="backupRestoreVerificationText">백업을 선택하면 검증 결과를 확인합니다.</p><p>현재 원본을 보호하는 pre_restore 백업을 검증한 뒤 자료와 첨부파일을 병합합니다. 온라인 원본과 로그인 정보는 이 보호 범위에 포함되지 않습니다.</p></section><section id="backupRestoreProgress" class="restore-progress" aria-live="polite" aria-atomic="true" hidden><header><span class="restore-progress-indicator" aria-hidden="true"></span><h3 id="backupRestoreProgressTitle"></h3></header><p id="backupRestoreProgressText"></p><ol id="backupRestoreProgressSteps"><li data-restore-step="protect">현재 상태 보호 백업 검증</li><li data-restore-step="verify">백업·첨부 검증과 준비</li><li data-restore-step="merge">자료·첨부 반영</li></ol><p id="backupRestoreProtectionFact"></p><p id="backupRestoreScopeFact">보호 범위: 이 학급의 로컬 자료와 백업 대상 첨부파일</p></section>');
}

export function renderRestoreVerification(input: { selected?: BackupItem | null; preview?: BackupPreview | null }) {
  initRestoreRenderer();
  const text = document.getElementById('backupRestoreVerificationText');
  if (!text) return;
  text.textContent = !input.selected ? '복원할 백업을 선택하세요.' : input.preview?.ok === true ? '선택 백업 미리보기 검증을 통과했습니다. 실행 직전에 다시 검증하며 원장 분기나 수업 연결 충돌이 있으면 복원을 중단합니다.' : '선택 백업의 검증 완료가 아직 확인되지 않았습니다. 미리보기를 확인하세요.';
}

export function renderRestoreProgress(state: RestoreProgressState) {
  initRestoreRenderer();
  const panel = document.getElementById('backupRestoreProgress');
  if (!panel) return;
  panel.hidden = state.status === 'idle';
  panel.dataset.state = state.status;
  panel.setAttribute('aria-busy', String(state.active));
  const set = (id: string, value: string) => { const node = document.getElementById(id); if (node) node.textContent = value; };
  const phase = state.lastConfirmedPhase;
  if (state.status === 'working') {
    set('backupRestoreProgressTitle', phase === 'merge_started' ? '자료·첨부파일을 반영하고 있습니다' : '복원할 자료를 확인하고 있습니다');
    set('backupRestoreProgressText', phase === 'merge_started' ? '자료 반영을 시작했습니다. 완료 결과를 확인할 때까지 앱을 종료하지 마세요.' : '검증과 준비 중입니다. 저장·동기화·추가 복원과 화면 이동은 잠시 중단됩니다.');
  } else if (state.status === 'success') {
    set('backupRestoreProgressTitle', '이 PC의 복원을 완료했습니다');
    set('backupRestoreProgressText', '복원 명령의 완료 결과를 확인했습니다. 기기 동기화 완료와는 별개이며 현재 자료와 연결 상태를 확인하세요.');
  } else if (state.status === 'failed') {
    set('backupRestoreProgressTitle', state.errorKind === 'conflict' || state.errorKind === 'validation_failed' ? '검증 문제로 복원이 차단되었습니다' : '복원을 완료하지 못했습니다');
    set('backupRestoreProgressText', phase === 'merge_started' ? '자료 반영 시작 이후 오류가 확인되었습니다. 현재 원본과 복구 상태를 확인해야 합니다. 보호·복구 파일을 삭제하거나 복원을 반복하지 마세요.' : phase ? '확인된 단계에서 복원이 중단되었습니다. 현재 저장 원본을 다시 확인한 뒤 진단에 따라 재시도하세요.' : '실패한 세부 단계가 확인되지 않았습니다. 현재 원본과 복구 상태를 확인하세요.');
  }
  set('backupRestoreProtectionFact', state.safetyBackupVerified ? `현재 상태 보호 백업 검증 완료${state.safetyCreatedAtMs ? ` · ${date(state.safetyCreatedAtMs)}` : ''}` : '현재 상태 보호 백업의 검증 완료가 아직 확인되지 않았습니다.');
  panel.querySelectorAll<HTMLElement>('[data-restore-step]').forEach((step) => {
    const key = step.dataset.restoreStep;
    const done = key === 'protect' ? state.safetyBackupVerified : key === 'verify' ? phase === 'merge_started' || state.status === 'success' : state.status === 'success';
    step.dataset.done = String(done);
  });
}

export type BackupRestorePanelState = { tenantId: string; configured: boolean; backups: BackupItem[]; selectedManifestPath: string; preview: BackupPreview | null; message: string; tone: 'ok' | 'warning' | 'error' | 'neutral'; busy?: boolean; currentPcName?: string };
const byId = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const escapeHtml = (value: unknown) => String(value ?? '').replace(/&/gu, '&amp;').replace(/</gu, '&lt;').replace(/>/gu, '&gt;').replace(/"/gu, '&quot;');
const numberText = (value: number) => new Intl.NumberFormat('ko-KR').format(Number(value) || 0);
const setText = (id: string, value: string) => { const node = byId(id); if (node) node.textContent = value; };
const setBadge = (id: string, value: string, tone: BackupRestorePanelState['tone']) => { const node = byId(id); if (node) { node.textContent = value; node.className = `status-badge badge-${tone}`; } };
export function renderBackupRestorePanel(input: BackupRestorePanelState) {
  initRestoreRenderer();
  const { tenantId, configured, backups: backupList, selectedManifestPath: selectedBackupManifestPath, message: rawMessage, tone: backupRestoreTone, currentPcName } = input;
  const backupRestoreMessage = input.tone === "error" && /[A-Za-z_]{3,}:|(?:[A-Z]:[\\/])|(?:\/Users\/)|(?:\/home\/)/u.test(rawMessage) ? "작업을 완료하지 못했습니다. 현재 원본과 복구 진단을 확인하세요." : rawMessage;
  const selectedRow = backupList.find(backup => backup.manifestPath === selectedBackupManifestPath);
  const backupPreview = selectedRow && input.preview?.ok === true && input.preview.manifestPath === selectedBackupManifestPath && input.preview.tenantId === tenantId ? input.preview : null;
  const listEl = byId<HTMLElement>("backupList");
  const selected = backupList.find((backup) => backup.manifestPath === selectedBackupManifestPath) || null;

  if (!tenantId) {
    setBadge("backupRestoreBadge", "학급 필요", "warning");
    setText("backupRestoreStatus", "학급 ID가 연결되면 백업 목록과 복원 미리보기를 확인할 수 있습니다.");
    listEl.innerHTML = `<p class="backup-list-empty">먼저 학급 ID를 입력하거나 다시 연결하기로 학급을 연결해 주세요.</p>`;
  } else if (!configured && !backupList.length) {
    setBadge("backupRestoreBadge", "폴더 필요", "warning");
    setText("backupRestoreStatus", "새 PC에서는 OneDrive 안의 백업 폴더를 선택하면 복원 후보를 찾습니다.");
    listEl.innerHTML = `<p class="backup-list-empty">백업 폴더가 아직 설정되지 않았습니다.</p>`;
  } else if (!backupList.length) {
    setBadge("backupRestoreBadge", "백업 없음", "warning");
    setText("backupRestoreStatus", "선택한 폴더에서 이 학급의 백업 manifest를 찾지 못했습니다.");
    listEl.innerHTML = `<p class="backup-list-empty">복원 가능한 백업이 없습니다.</p>`;
  } else {
    setBadge("backupRestoreBadge", backupRestoreTone === "ok" ? "완료" : backupPreview?.ok ? "미리보기" : "선택됨", backupRestoreTone);
    setText(
      "backupRestoreStatus",
      backupRestoreMessage
        || (backupPreview?.ok
          ? `${formatBackupDateTime(backupPreview.createdAtMs || selected?.createdAtMs)} 백업을 선택했습니다.`
          : "복원할 백업을 선택하면 미리보기를 불러옵니다."),
    );
    listEl.innerHTML = backupList.map((backup, index) => {
      const isSelected = backup.manifestPath === selectedBackupManifestPath;
      return `
        <button class="backup-list-row${isSelected ? " is-selected" : ""}" type="button" data-backup-index="${index}" aria-pressed="${isSelected}"${input.busy ? " disabled" : ""}>
          <span class="backup-list-row__radio" aria-hidden="true"></span>
          <span class="backup-list-row__device" aria-hidden="true"><i class="fa-solid fa-desktop"></i></span>
          <span class="backup-list-row__main">
            <strong class="backup-list-row__time">
              <span>${escapeHtml(formatBackupDateTime(backup.createdAtMs))}</span><span class="backup-list-row__badge backup-list-row__badge--kind">${escapeHtml(backupKindLabel(backup.kind))}</span>
              ${index === 0 ? `<span class="backup-list-row__badge">최신</span>` : ""}
            </strong>
            <span class="backup-list-row__meta">${escapeHtml(backupSourceListText(backup.source, currentPcName))}</span>
            <span class="backup-list-row__counts">${escapeHtml(backupRowSummary(backup))}</span>
          </span>
        </button>
      `;
    }).join("");
  }

  const counts = backupPreview?.counts || selected?.counts || {};
  const media = backupPreview?.media || selected?.media || {};
  const source = backupPreview?.source || selected?.source;
  setText("backupPreviewCare", backupList.length ? `${numberText(backupCareCount(counts))}건` : "-");
  setText("backupPreviewAttendance", backupList.length ? `${numberText(backupAttendanceCount(counts))}건` : "-");
  setText("backupPreviewLearning", backupList.length ? `${numberText(backupLearningCount(counts))}건` : "-");
  setText("backupPreviewStudentRecord", backupList.length ? `${numberText(backupStudentRecordCount(counts))}건` : "-");
  setText("backupPreviewBoard", backupList.length ? `${numberText(backupBoardSnapshotCount(counts))}건` : "-");
  setText("backupPreviewAttachments", backupList.length ? `${numberText(backupBoardMediaCount(counts, media))}개` : "-");
  setText("backupPreviewArchives", backupList.length ? `${numberText(backupArchiveCount(counts))}개` : "-");
  const detailsEl = byId<HTMLElement>("backupPreviewDetails");
  if (!backupList.length) {
    detailsEl.innerHTML = `<p class="restore-preview-empty">복원할 백업을 선택하면 PC 정보와 상세 건수가 표시됩니다.</p>`;
  } else {
    detailsEl.innerHTML = `
      <div class="restore-preview-source">
        <i class="fa-solid fa-desktop" aria-hidden="true"></i>
        <strong>${escapeHtml(`${backupSourcePcName(source)} / ${backupSourceRelation(source, currentPcName)} / ${backupEnvironmentText(source)}`)}</strong>
        <span>${escapeHtml(formatBackupDateTime(backupPreview?.createdAtMs || selected?.createdAtMs))} 백업</span>
      </div>
    `;
  }
  renderRestoreVerification({ selected, preview: backupPreview });
  const scroll = listEl.dataset.restoreScroll;
  if (scroll) listEl.scrollTop = Number(scroll);
  listEl.onscroll = () => { listEl.dataset.restoreScroll = String(listEl.scrollTop); };
  document.querySelectorAll<HTMLButtonElement>('[data-action="restore-backup"]').forEach(button => { button.disabled = input.busy === true || !backupPreview || !selected; });
}
