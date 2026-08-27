import { invoke } from '@tauri-apps/api/core';
import type { BackupStorageOverview, LegacyCleanupPreview } from './backup-types';

type Options = { getTenantId: () => string; isConfigured: () => boolean };
const TUTORIAL_KEY = 'localBackupStorageTutorial:v1';
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
function escapeHtml(value: string) {
  return String(value || '').replace(/[&<>'"]/g, (character) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', "'": '&#39;', '"': '&quot;' })[character] || character);
}
function badge(label: string, tone: 'ok' | 'warning' | 'neutral') {
  const node = required('backupStorageBadge'); node.textContent = label; node.className = `status-badge badge-${tone}`;
}

export function initBackupStorage(options: Options) {
  let snapshot: BackupStorageOverview | null = null;
  let cleanupPreview: LegacyCleanupPreview | null = null;
  let tutorialIndex = -1;

  const render = (storage: BackupStorageOverview | null, error = '') => {
    snapshot = storage;
    const configured = options.isConfigured();
    const apply = required<HTMLButtonElement>('backupLegacyApply');
    const preview = required<HTMLButtonElement>('backupLegacyPreview');
    const refresh = required<HTMLButtonElement>('backupStorageRefresh');
    if (!configured || !storage?.ok) {
      badge(configured ? '확인 필요' : '폴더 필요', configured ? 'warning' : 'neutral');
      text('backupStorageStatus', configured ? `용량을 읽지 못했습니다: ${error || storage?.error || 'unknown'}` : '백업 폴더를 설정하면 중복 제거와 보관 용량을 확인할 수 있습니다.');
      ['backupStorageOriginal', 'backupStorageObjects', 'backupStorageDatabase', 'backupStorageLegacy', 'backupStorageReclaimable'].forEach((id) => text(id, '-'));
      required('backupStorageLargeFiles').innerHTML = '<p>100MB 이상 첨부파일이 있으면 여기에 표시합니다. 원본은 자동으로 줄이거나 삭제하지 않습니다.</p>';
      preview.disabled = true; refresh.disabled = !configured; apply.disabled = true; return;
    }
    badge(storage.snapshotVersion === 5 ? 'v5 중복 제거' : '확인 필요', storage.snapshotVersion === 5 ? 'ok' : 'warning');
    text('backupStorageStatus', '같은 첨부파일은 SHA-256 객체 한 개만 저장하며 백업 이력에서는 그 객체를 참조합니다.');
    text('backupStorageOriginal', `${numberText(storage.currentOriginalCount)}개 · ${byteText(storage.currentOriginalBytes)}`);
    text('backupStorageObjects', `${numberText(storage.uniqueObjectCount)}개 · ${byteText(storage.uniqueObjectBytes)}`);
    text('backupStorageDatabase', byteText(storage.databaseHistoryBytes));
    text('backupStorageLegacy', `${numberText(storage.legacySnapshotCount)}개 · ${byteText(storage.legacySnapshotBytes)}`);
    text('backupStorageReclaimable', `${numberText(storage.legacyCleanupCandidateCount)}개 · ${byteText(storage.legacyReclaimableBytes)}`);
    const largest = Array.isArray(storage.largestFiles) ? storage.largestFiles : [];
    required('backupStorageLargeFiles').innerHTML = largest.length
      ? `<strong>100MB 이상 큰 원본</strong><ul>${largest.map((file) => `<li><span>${escapeHtml(file.kind || '첨부')} · ${escapeHtml(file.name || file.localPath || '이름 없음')}</span><b>${byteText(file.bytes)}</b></li>`).join('')}</ul><p>큰 파일도 원본 그대로 보관합니다. 필요 여부를 교사가 확인해 원본 자료에서 직접 정리하세요.</p>`
      : '<p>100MB 이상 큰 첨부파일이 없습니다. 원본은 자동으로 줄이거나 삭제하지 않습니다.</p>';
    preview.disabled = numeric(storage.legacySnapshotCount) === 0;
    refresh.disabled = false;
    apply.disabled = !cleanupPreview?.previewToken || numeric(cleanupPreview.candidateCount) === 0;
  };

  const refresh = async () => {
    const tenantId = options.getTenantId();
    if (DESIGN_PREVIEW) { render(PREVIEW_STORAGE); return; }
    if (!tenantId || !options.isConfigured()) { render(null); return; }
    try { render(await invoke<BackupStorageOverview>('get_backup_storage_overview', { tenantId })); }
    catch (error) { render({ ok: false }, String((error as Error)?.message || error)); }
  };

  const previewCleanup = async () => {
    const tenantId = options.getTenantId(); if (!tenantId) return;
    required<HTMLButtonElement>('backupLegacyPreview').disabled = true;
    let message = '';
    try {
      cleanupPreview = await invoke<LegacyCleanupPreview>('preview_legacy_backup_cleanup', { tenantId });
      if (!cleanupPreview?.ok) throw new Error(cleanupPreview?.error || 'backup_cleanup_preview_failed');
      message = numeric(cleanupPreview.candidateCount) > 0
        ? `이전 방식 백업 ${numberText(cleanupPreview.candidateCount)}개, ${byteText(cleanupPreview.reclaimableBytes)}를 정리할 수 있습니다. 목록이 바뀌면 실행은 자동 취소됩니다.`
        : '자동 동기화에 필요한 세대와 수동 백업을 제외하면 정리할 이전 백업이 없습니다.';
    } catch (error) { cleanupPreview = null; message = `정리 미리보기 실패: ${String((error as Error)?.message || error)}`; }
    render(snapshot); text('backupStorageStatus', message);
  };

  const applyCleanup = async () => {
    const tenantId = options.getTenantId(); const preview = cleanupPreview;
    if (!tenantId || !preview?.previewToken || !numeric(preview.candidateCount)) return;
    const dialog = required<HTMLDialogElement>('backupCleanupConfirmDialog');
    text('backupCleanupConfirmSummary', `미리보기에서 확인한 이전 방식 백업 ${numberText(preview.candidateCount)}개(${byteText(preview.reclaimableBytes)})만 삭제합니다.`);
    const confirmed = await new Promise<boolean>((resolve) => { dialog.addEventListener('close', () => resolve(dialog.returnValue === 'confirm'), { once: true }); dialog.showModal(); });
    if (!confirmed) return;
    required<HTMLButtonElement>('backupLegacyApply').disabled = true;
    try {
      const result = await invoke<{ ok?: boolean; deleted?: number; reclaimedBytes?: number; error?: string }>('apply_legacy_backup_cleanup', { tenantId, previewToken: preview.previewToken });
      if (!result?.ok) throw new Error(result?.error || 'backup_cleanup_failed');
      cleanupPreview = null; await refresh();
      text('backupStorageStatus', `이전 방식 백업 ${numberText(result.deleted)}개를 정리해 ${byteText(result.reclaimedBytes)}를 회수했습니다.`);
    } catch (error) {
      cleanupPreview = null; text('backupStorageStatus', `정리 취소 또는 실패: ${String((error as Error)?.message || error)}. 다시 미리보기한 뒤 실행하세요.`); await refresh();
    }
  };

  const tutorialSteps = [
    { target: required('backupStoragePanel'), title: '백업 하나, 첨부 원본 하나', copy: '수업자료와 업무자료는 같은 백업을 사용합니다. 같은 내용의 첨부는 SHA-256 객체 한 개만 보관합니다.' },
    { target: required('backupStorageLargeFiles'), title: '큰 원본 확인', copy: '100MB 이상 원본을 보여 주지만 자동 압축·삭제하지 않습니다. 필요 여부는 교사가 원본 자료에서 판단합니다.' },
    { target: required('backupLegacyPreview'), title: '먼저 미리보기', copy: '이전 방식의 중복 백업은 미리보기와 두 번째 확인 뒤에만 정리합니다. 수동 백업과 동기화에 필요한 세대는 제외합니다.' },
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
  required('backupLegacyPreview').addEventListener('click', () => void previewCleanup());
  required('backupLegacyApply').addEventListener('click', () => void applyCleanup());
  required('backupStorageHelp').addEventListener('click', openTutorial);
  required('backupStorageTutorialClose').addEventListener('click', () => closeTutorial(false));
  required('backupStorageTutorialNext').addEventListener('click', () => { if (tutorialIndex >= tutorialSteps.length - 1) closeTutorial(true); else { tutorialIndex += 1; renderTutorial(); } });
  document.querySelector<HTMLElement>('[data-app-view-target="backup"]')?.addEventListener('click', () => { if (localStorage.getItem(TUTORIAL_KEY) !== 'complete') window.setTimeout(openTutorial, 0); });
  return { refresh, clear: () => render(null), error: (value: unknown) => render({ ok: false }, String((value as Error)?.message || value)) };
}
