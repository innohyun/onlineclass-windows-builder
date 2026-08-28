import { invoke } from '@tauri-apps/api/core';
import type { BackupStorageOverview } from './backup-types';

type Options = { getTenantId: () => string; isConfigured: () => boolean };
const TUTORIAL_KEY = 'localBackupStorageTutorial:v2';
const DESIGN_PREVIEW = new URLSearchParams(window.location.search).get('designPreview') === 'backup';
const PREVIEW_STORAGE: BackupStorageOverview = {
  ok: true,
  snapshotVersion: 5,
  currentOriginalCount: 248,
  currentOriginalBytes: 2_840_000_000,
  uniqueObjectCount: 183,
  uniqueObjectBytes: 2_010_000_000,
  databaseHistoryBytes: 486_000_000,
  legacySnapshotCount: 12,
  legacySnapshotBytes: 3_760_000_000,
  legacyCleanupCandidateCount: 9,
  legacyReclaimableBytes: 2_940_000_000,
  legacyQuarantineCount: 9,
  legacyQuarantineBytes: 2_940_000_000,
  legacyQuarantinePurgeAfterMs: Date.now() + 12 * 86_400_000,
  legacyQuarantineReviewCount: 0,
  largestFiles: [
    { kind: '수업자료', name: '과학_지층관찰_원본영상.mp4', localPath: '', bytes: 684_000_000 },
    { kind: '업무자료', name: '현장체험학습_안전교육.mp4', localPath: '', bytes: 238_000_000 },
  ],
};

function required<T extends HTMLElement>(id: string) {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing backup storage element: ${id}`);
  return node as T;
}

function text(id: string, value: string) { required(id).textContent = value || '-'; }
function numeric(value?: number) { return Number(value || 0) || 0; }
function numberText(value?: number) { return String(numeric(value)); }
function byteText(value?: number) {
  const bytes = Math.max(0, numeric(value));
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}
function dateText(value?: number) {
  const timestamp = numeric(value);
  if (!timestamp) return '';
  return new Intl.DateTimeFormat('ko-KR', { month: 'long', day: 'numeric' }).format(new Date(timestamp));
}
function escapeHtml(value: string) {
  return String(value || '').replace(/[&<>'"]/g, (character) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', "'": '&#39;', '"': '&quot;' })[character] || character);
}
function badge(label: string, tone: 'ok' | 'warning' | 'neutral') {
  const node = required('backupStorageBadge'); node.textContent = label; node.className = `status-badge badge-${tone}`;
}

export function initBackupStorage(options: Options) {
  let snapshot: BackupStorageOverview | null = null;
  let tutorialIndex = -1;

  const render = (storage: BackupStorageOverview | null, error = '') => {
    snapshot = storage;
    const configured = options.isConfigured();
    const undo = required<HTMLButtonElement>('backupLegacyUndo');
    const refresh = required<HTMLButtonElement>('backupStorageRefresh');
    if (!configured || !storage?.ok) {
      badge(configured ? '확인 필요' : '폴더 필요', configured ? 'warning' : 'neutral');
      text('backupStorageStatus', configured ? `용량을 읽지 못했습니다: ${error || storage?.error || 'unknown'}` : '백업 폴더를 설정하면 중복 제거와 보관 용량을 확인할 수 있습니다.');
      ['backupStorageOriginal', 'backupStorageObjects', 'backupStorageDatabase', 'backupStorageLegacy', 'backupStorageReclaimable'].forEach((id) => text(id, '-'));
      required('backupStorageLargeFiles').innerHTML = '<p>100MB 이상 첨부파일이 있으면 여기에 표시합니다. 원본은 자동으로 줄이거나 삭제하지 않습니다.</p>';
      refresh.disabled = !configured; undo.disabled = true; return;
    }
    const reviewCount = numeric(storage.legacyQuarantineReviewCount);
    const quarantineError = String(storage.legacyQuarantineError || '');
    const healthy = storage.snapshotVersion === 5 && !reviewCount && !quarantineError;
    badge(healthy ? 'v5 자동 관리' : '확인 필요', healthy ? 'ok' : 'warning');
    text('backupStorageStatus', quarantineError
      ? `자동 격리 기록을 확인해야 합니다: ${quarantineError}`
      : reviewCount
        ? `파일 상태가 달라 자동 삭제하지 않은 이전 백업 ${numberText(reviewCount)}개가 있습니다.`
        : '정상 v5 백업을 확인한 뒤 안전한 이전 백업만 30일 동안 자동 격리합니다.');
    text('backupStorageOriginal', `${numberText(storage.currentOriginalCount)}개 · ${byteText(storage.currentOriginalBytes)}`);
    text('backupStorageObjects', `${numberText(storage.uniqueObjectCount)}개 · ${byteText(storage.uniqueObjectBytes)}`);
    text('backupStorageDatabase', byteText(storage.databaseHistoryBytes));
    text('backupStorageLegacy', `${numberText(storage.legacySnapshotCount)}개 · ${byteText(storage.legacySnapshotBytes)}`);
    const quarantineCount = numeric(storage.legacyQuarantineCount);
    const purgeDate = dateText(storage.legacyQuarantinePurgeAfterMs);
    text('backupStorageReclaimable', `${numberText(quarantineCount)}개 · ${byteText(storage.legacyQuarantineBytes)}${purgeDate ? ` · ${purgeDate}까지` : ''}`);
    const largest = Array.isArray(storage.largestFiles) ? storage.largestFiles : [];
    required('backupStorageLargeFiles').innerHTML = largest.length
      ? `<strong>100MB 이상 큰 원본</strong><ul>${largest.map((file) => `<li><span>${escapeHtml(file.kind || '첨부')} · ${escapeHtml(file.name || file.localPath || '이름 없음')}</span><b>${byteText(file.bytes)}</b></li>`).join('')}</ul><p>큰 파일도 원본 그대로 보관합니다. 필요 여부를 교사가 확인해 원본 자료에서 직접 정리하세요.</p>`
      : '<p>100MB 이상 큰 첨부파일이 없습니다. 원본은 자동으로 줄이거나 삭제하지 않습니다.</p>';
    refresh.disabled = false;
    undo.disabled = quarantineCount === 0 || Boolean(quarantineError);
  };

  const refresh = async () => {
    const tenantId = options.getTenantId();
    if (DESIGN_PREVIEW) { render(PREVIEW_STORAGE); return; }
    if (!tenantId || !options.isConfigured()) { render(null); return; }
    try { render(await invoke<BackupStorageOverview>('get_backup_storage_overview', { tenantId })); }
    catch (error) { render({ ok: false }, String((error as Error)?.message || error)); }
  };

  const undoCleanup = async () => {
    const tenantId = options.getTenantId();
    const quarantineCount = numeric(snapshot?.legacyQuarantineCount);
    if (!tenantId || !quarantineCount) return;
    const dialog = required<HTMLDialogElement>('backupCleanupConfirmDialog');
    text('backupCleanupConfirmSummary', `30일 보관 중인 이전 방식 백업 ${numberText(quarantineCount)}개(${byteText(snapshot?.legacyQuarantineBytes)})를 원래 위치로 복원합니다.`);
    const confirmed = await new Promise<boolean>((resolve) => { dialog.addEventListener('close', () => resolve(dialog.returnValue === 'confirm'), { once: true }); dialog.showModal(); });
    if (!confirmed) return;
    required<HTMLButtonElement>('backupLegacyUndo').disabled = true;
    try {
      const result = await invoke<{ ok?: boolean; restored?: number; restoredBytes?: number; reviewCount?: number; error?: string }>('undo_legacy_backup_cleanup', { tenantId });
      if (!result?.ok) throw new Error(result?.error || 'backup_cleanup_undo_failed');
      await refresh();
      text('backupStorageStatus', result.reviewCount
        ? `${numberText(result.restored)}개를 되돌렸고, 상태가 달라진 ${numberText(result.reviewCount)}개는 건드리지 않았습니다.`
        : `이전 방식 백업 ${numberText(result.restored)}개(${byteText(result.restoredBytes)})를 원래 위치로 되돌렸습니다.`);
    } catch (error) {
      text('backupStorageStatus', `격리 되돌리기 실패: ${String((error as Error)?.message || error)}. 파일은 자동 삭제하지 않았습니다.`); await refresh();
    }
  };

  const tutorialSteps = [
    { target: required('backupStoragePanel'), title: '백업 하나, 첨부 원본 하나', copy: '수업자료와 업무자료는 같은 백업을 사용합니다. 같은 내용의 첨부는 SHA-256 객체 한 개만 보관합니다.' },
    { target: required('backupStorageLargeFiles'), title: '큰 원본 확인', copy: '100MB 이상 원본을 보여 주지만 자동 압축·삭제하지 않습니다. 필요 여부는 교사가 원본 자료에서 판단합니다.' },
    { target: required('backupLegacyUndo'), title: '30일 안에는 되돌리기', copy: '검증된 v5 백업보다 오래되고 안전한 이전 백업만 자동 격리합니다. 30일 안에는 이 버튼으로 원래 위치에 되돌릴 수 있습니다.' },
  ];
  const renderTutorial = () => {
    document.querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target'));
    const step = tutorialSteps[tutorialIndex]; const tutorial = required('backupStorageTutorial');
    if (!step) { tutorial.hidden = true; tutorialIndex = -1; return; }
    step.target.classList.add('local-reader-tutorial-target'); text('backupStorageTutorialStep', `${tutorialIndex + 1} / ${tutorialSteps.length}`);
    text('backupStorageTutorialTitle', step.title); text('backupStorageTutorialCopy', step.copy);
    required<HTMLButtonElement>('backupStorageTutorialNext').textContent = tutorialIndex === tutorialSteps.length - 1 ? '완료' : '다음'; tutorial.hidden = false;
  };
  const openTutorial = () => { tutorialIndex = 0; renderTutorial(); };
  const closeTutorial = (complete = false) => {
    document.querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target'));
    required('backupStorageTutorial').hidden = true; tutorialIndex = -1; if (complete) localStorage.setItem(TUTORIAL_KEY, 'complete');
  };

  required('backupStorageRefresh').addEventListener('click', () => void refresh());
  required('backupLegacyUndo').addEventListener('click', () => void undoCleanup());
  required('backupStorageHelp').addEventListener('click', openTutorial);
  required('backupStorageTutorialClose').addEventListener('click', () => closeTutorial(false));
  required('backupStorageTutorialNext').addEventListener('click', () => { if (tutorialIndex >= tutorialSteps.length - 1) closeTutorial(true); else { tutorialIndex += 1; renderTutorial(); } });
  document.querySelector<HTMLElement>('[data-app-view-target="backup"]')?.addEventListener('click', () => { if (localStorage.getItem(TUTORIAL_KEY) !== 'complete') window.setTimeout(openTutorial, 0); });
  return { refresh, clear: () => render(null), error: (value: unknown) => render({ ok: false }, String((value as Error)?.message || value)) };
}
