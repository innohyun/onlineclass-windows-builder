import { invoke } from '@tauri-apps/api/core';
import type { BackupStorageOverview } from './backup-types';

type Options = { getTenantId: () => string; isConfigured: () => boolean };
const TUTORIAL_KEY = 'localBackupStorageTutorial:v3';
const CACHE_MS = 30_000;
const DESIGN_PREVIEW = new URLSearchParams(window.location.search).get('designPreview') === 'backup';
const PREVIEW_STORAGE: BackupStorageOverview = {
  ok: true,
  scanComplete: true,
  scannedAtMs: Date.now(),
  totalLogicalBytes: 9_510_000_000,
  storageBreakdown: {
    v5DatabaseBytes: 320_000_000,
    v5MetadataBytes: 8_000_000,
    legacySnapshotBytes: 3_760_000_000,
    objectBytes: 2_010_000_000,
    objectQuarantineBytes: 62_000_000,
    legacyQuarantineBytes: 2_940_000_000,
    archiveBundleBytes: 400_000_000,
    stagingBytes: 9_000_000,
    otherBytes: 1_000_000,
  },
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
  let snapshotTenantId = '';
  let revision = 0;
  let cache: { tenantId: string; storedAtMs: number; storage: BackupStorageOverview } | null = null;
  const pending = new Map<string, { revision: number; promise: Promise<void> }>();
  let undoBusy = false;
  let tutorialIndex = -1;

  const currentRequest = (tenantId: string, requestRevision: number) => tenantId === options.getTenantId()
    && requestRevision === revision && options.isConfigured();
  const invalidate = () => { revision += 1; cache = null; };
  const updateActions = () => {
    const busy = pending.has(options.getTenantId());
    const refreshButton = required<HTMLButtonElement>('backupStorageRefresh');
    refreshButton.disabled = !options.isConfigured() || busy || undoBusy;
    refreshButton.textContent = busy ? '용량 확인 중…' : '용량 새로고침';
    required('backupStoragePanel').setAttribute('aria-busy', String(busy || undoBusy));
    required<HTMLButtonElement>('backupLegacyUndo').disabled = busy || undoBusy
      || snapshotTenantId !== options.getTenantId() || !snapshot?.ok
      || !numeric(snapshot.legacyQuarantineCount) || Boolean(snapshot.legacyQuarantineError);
  };

  const render = (storage: BackupStorageOverview | null, error = '') => {
    snapshot = storage;
    snapshotTenantId = options.getTenantId();
    const configured = options.isConfigured();
    if (!configured || !storage?.ok) {
      const failed = Boolean(storage || error);
      badge(configured ? failed ? '확인 필요' : '확인 중' : '폴더 필요', configured && failed ? 'warning' : 'neutral');
      text('backupStorageStatus', configured
        ? failed ? `용량을 읽지 못했습니다: ${error || storage?.error || '폴더 접근 상태를 확인해 주세요.'}` : '백업 폴더 용량을 확인하고 있습니다.'
        : error ? `백업 폴더를 확인하지 못했습니다: ${error}` : '백업 폴더를 설정하면 중복 제거와 보관 용량을 확인할 수 있습니다.');
      ['backupStorageTotal', 'backupStorageOriginal', 'backupStorageObjects', 'backupStorageDatabase', 'backupStorageMetadata', 'backupStorageLegacy', 'backupStorageReclaimable', 'backupStorageObjectQuarantine', 'backupStorageArchives', 'backupStorageStaging', 'backupStorageOther'].forEach((id) => text(id, failed ? '확인 필요' : '-'));
      text('backupStorageScannedAt', '전체 용량을 아직 확인하지 못했습니다.');
      text('backupStorageQuarantineNote', '격리 파일도 정리되기 전까지 백업 폴더 용량에 포함됩니다.');
      required('backupStorageLargeFiles').innerHTML = '<p>100MB 이상 첨부파일이 있으면 여기에 표시합니다. 원본은 자동으로 줄이거나 삭제하지 않습니다.</p>';
      updateActions(); return;
    }
    const breakdown = storage.storageBreakdown;
    const complete = storage.scanComplete === true && Boolean(breakdown) && Number.isFinite(storage.totalLogicalBytes);
    const storageBytes = (value?: number) => {
      if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return '확인 필요';
      if (!complete) return value > 0 ? `${byteText(value)} 이상` : '확인 필요';
      return byteText(value);
    };
    const reviewCount = numeric(storage.legacyQuarantineReviewCount);
    const quarantineError = String(storage.legacyQuarantineError || '');
    const healthy = complete && storage.snapshotVersion === 5 && !reviewCount && !quarantineError;
    badge(healthy ? 'v5 자동 관리' : complete ? '확인 필요' : '용량 일부 미확인', healthy ? 'ok' : 'warning');
    text('backupStorageStatus', !complete
      ? '일부 파일을 읽지 못해 전체 용량을 확정할 수 없습니다. 폴더 접근과 OneDrive 상태를 확인한 뒤 새로고침해 주세요.'
      : quarantineError
        ? `자동 격리 기록을 확인해야 합니다: ${quarantineError}`
        : reviewCount
          ? `파일 상태가 달라 자동 삭제하지 않은 이전 백업 ${numberText(reviewCount)}개가 있습니다.`
          : '정상 v5 백업을 확인한 뒤 안전한 이전 백업만 30일 동안 자동 격리합니다.');
    text('backupStorageTotal', storageBytes(storage.totalLogicalBytes));
    text('backupStorageScannedAt', storage.scannedAtMs
      ? `마지막 확인 ${new Intl.DateTimeFormat('ko-KR', { dateStyle: 'short', timeStyle: 'short' }).format(new Date(storage.scannedAtMs))}${complete ? '' : ' · 일부 미확인'}`
      : '전체 용량 확인 시각이 없습니다. 새로고침해 주세요.');
    text('backupStorageOriginal', `${numberText(storage.currentOriginalCount)}개 · ${storageBytes(storage.currentOriginalBytes)}`);
    text('backupStorageObjects', `${numberText(storage.uniqueObjectCount)}개 · ${storageBytes(breakdown?.objectBytes)}`);
    text('backupStorageDatabase', storageBytes(breakdown?.v5DatabaseBytes));
    text('backupStorageMetadata', storageBytes(breakdown?.v5MetadataBytes));
    text('backupStorageLegacy', `${numberText(storage.legacySnapshotCount)}개 · ${storageBytes(breakdown?.legacySnapshotBytes)}`);
    text('backupStorageObjectQuarantine', storageBytes(breakdown?.objectQuarantineBytes));
    text('backupStorageArchives', storageBytes(breakdown?.archiveBundleBytes));
    text('backupStorageStaging', storageBytes(breakdown?.stagingBytes));
    text('backupStorageOther', storageBytes(breakdown?.otherBytes));
    const quarantineCount = numeric(storage.legacyQuarantineCount);
    const purgeDate = dateText(storage.legacyQuarantinePurgeAfterMs);
    text('backupStorageReclaimable', `${numberText(quarantineCount)}개 · ${storageBytes(breakdown?.legacyQuarantineBytes)}`);
    text('backupStorageQuarantineNote', purgeDate
      ? `격리된 이전 백업은 ${purgeDate} 이후 앱 실행 중 안전 조건을 확인한 뒤 정리합니다. 정리 전까지 이 용량은 계속 포함됩니다.`
      : '격리 파일도 정리되기 전까지 백업 폴더 용량에 포함됩니다. 만료 후 앱 실행 중 안전 조건을 확인한 뒤 정리합니다.');
    const largest = Array.isArray(storage.largestFiles) ? storage.largestFiles : [];
    required('backupStorageLargeFiles').innerHTML = largest.length
      ? `<strong>100MB 이상 큰 원본</strong><ul>${largest.map((file) => `<li><span>${escapeHtml(file.kind || '첨부')} · ${escapeHtml(file.name || file.localPath || '이름 없음')}</span><b>${byteText(file.bytes)}</b></li>`).join('')}</ul><p>큰 파일도 원본 그대로 보관합니다. 필요 여부를 교사가 확인해 원본 자료에서 직접 정리하세요.</p>`
      : complete
        ? '<p>100MB 이상 큰 첨부파일이 없습니다. 원본은 자동으로 줄이거나 삭제하지 않습니다.</p>'
        : '<p>확인된 큰 첨부파일이 없습니다. 일부 파일을 읽지 못했으므로 새로고침 후 다시 확인해 주세요. 원본은 자동으로 줄이거나 삭제하지 않습니다.</p>';
    updateActions();
  };

  const refresh = async (force = false): Promise<void> => {
    const tenantId = options.getTenantId();
    if (DESIGN_PREVIEW) {
      const state = new URLSearchParams(window.location.search).get('storageState');
      render(state === 'error' ? { ok: false, error: '백업 폴더 접근 실패' }
        : state === 'incomplete' ? { ...PREVIEW_STORAGE, scanComplete: false, scanErrors: ['fixture_unreadable_file'] }
          : PREVIEW_STORAGE);
      return;
    }
    if (!tenantId || !options.isConfigured()) { invalidate(); render(null); return; }
    if (force) cache = null;
    if (!force && cache?.tenantId === tenantId && Date.now() - cache.storedAtMs < CACHE_MS) {
      render(cache.storage); return;
    }
    const running = pending.get(tenantId);
    if (running) {
      const sameRevision = running.revision === revision;
      await running.promise;
      if (!sameRevision && tenantId === options.getTenantId() && options.isConfigured()) await refresh(force);
      return;
    }
    const request = { revision, promise: Promise.resolve() };
    pending.set(tenantId, request);
    if (snapshotTenantId !== tenantId) render(null);
    updateActions();
    request.promise = (async () => {
      try {
        const storage = await invoke<BackupStorageOverview>('get_backup_storage_overview', { tenantId, forceRefresh: force });
        if (!currentRequest(tenantId, request.revision)) return;
        if (storage.ok && storage.scanComplete) cache = { tenantId, storedAtMs: Date.now(), storage };
        render(storage);
      } catch (error) {
        if (currentRequest(tenantId, request.revision)) render({ ok: false }, String((error as Error)?.message || error));
      } finally {
        if (pending.get(tenantId) === request) pending.delete(tenantId);
        updateActions();
      }
    })();
    return request.promise;
  };

  const undoCleanup = async () => {
    const tenantId = options.getTenantId();
    const quarantineCount = numeric(snapshot?.legacyQuarantineCount);
    if (!tenantId || !quarantineCount || undoBusy || snapshotTenantId !== tenantId) return;
    undoBusy = true; updateActions();
    const dialog = required<HTMLDialogElement>('backupCleanupConfirmDialog');
    dialog.returnValue = '';
    text('backupCleanupConfirmSummary', `30일 보관 중인 이전 방식 백업 ${numberText(quarantineCount)}개(${byteText(snapshot?.legacyQuarantineBytes)})를 원래 위치로 복원합니다.`);
    try {
      const confirmed = await new Promise<boolean>((resolve) => { dialog.addEventListener('close', () => resolve(dialog.returnValue === 'confirm'), { once: true }); dialog.showModal(); });
      if (!confirmed || tenantId !== options.getTenantId()) return;
      const result = await invoke<{ ok?: boolean; restored?: number; restoredBytes?: number; reviewCount?: number; error?: string }>('undo_legacy_backup_cleanup', { tenantId });
      if (!result?.ok) throw new Error(result?.error || 'backup_cleanup_undo_failed');
      invalidate(); await refresh(true);
      if (tenantId !== options.getTenantId()) return;
      const outcome = result.reviewCount
        ? `${numberText(result.restored)}개를 되돌렸고, 상태가 달라진 ${numberText(result.reviewCount)}개는 건드리지 않았습니다.`
        : `이전 방식 백업 ${numberText(result.restored)}개(${byteText(result.restoredBytes)})를 원래 위치로 되돌렸습니다.`;
      text('backupStorageStatus', `${outcome} ${required('backupStorageStatus').textContent || ''}`);
    } catch (error) {
      invalidate(); await refresh(true);
      if (tenantId === options.getTenantId()) text('backupStorageStatus', `격리 되돌리기 실패: ${String((error as Error)?.message || error)}. 파일은 자동 삭제하지 않았습니다. ${required('backupStorageStatus').textContent || ''}`);
    } finally {
      undoBusy = false; updateActions();
    }
  };

  const policy = required<HTMLDetailsElement>('backupStoragePolicy');
  let policyWasOpen = false;
  const tutorialSteps = [
    { target: required('backupStorageTotalSummary'), title: '중복 없이 보는 백업 용량', copy: 'DB, 첨부 객체, 이전 백업, 격리 파일을 한 번씩 합산한 논리 크기입니다. 일부 파일을 읽지 못하면 전체 용량을 확정하지 않고 확인 필요로 표시합니다.' },
    { target: required('backupStorageOriginalReference'), title: '현재 원본은 참고값', copy: '현재 첨부 원본은 백업 폴더 합계에 더하지 않습니다. 같은 내용은 객체 하나만 보관합니다. 아래 큰 원본 목록도 자동 압축·삭제하지 않습니다.' },
    { target: policy, title: '격리 중에도 용량은 포함', copy: '격리 파일은 30일이 지나고 앱이 실행 중일 때 안전 조건을 다시 확인한 뒤 정리합니다. 읽지 못하거나 상태가 바뀐 파일은 보류하며, 수동 백업과 동기화에 필요한 세대는 유지합니다.' },
    { target: required('backupLegacyUndo'), title: '30일 안에는 되돌리기', copy: '자동 격리한 이전 백업이 있으면 이 버튼으로 원래 위치에 되돌릴 수 있습니다. 안내는 버튼을 대신 누르거나 파일을 변경하지 않습니다.' },
  ];
  const clearTutorialTarget = () => required('backupStoragePanel').querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target'));
  const positionTutorial = () => {
    const step = tutorialSteps[tutorialIndex];
    if (!step) return;
    const tutorial = required('backupStorageTutorial');
    const target = step.target.getBoundingClientRect();
    const panel = tutorial.getBoundingClientRect();
    const margin = 16; const gap = 14;
    const top = target.bottom + gap + panel.height <= window.innerHeight - margin
      ? target.bottom + gap : Math.max(margin, target.top - panel.height - gap);
    tutorial.style.left = `${Math.max(margin, Math.min(target.right - panel.width, window.innerWidth - panel.width - margin))}px`;
    tutorial.style.top = `${Math.min(top, Math.max(margin, window.innerHeight - panel.height - margin))}px`;
  };
  const renderTutorial = () => {
    clearTutorialTarget();
    const step = tutorialSteps[tutorialIndex]; const tutorial = required('backupStorageTutorial');
    if (!step) { tutorial.hidden = true; tutorialIndex = -1; return; }
    if (step.target === policy) policy.open = true;
    step.target.classList.add('local-reader-tutorial-target'); text('backupStorageTutorialStep', `${tutorialIndex + 1} / ${tutorialSteps.length}`);
    text('backupStorageTutorialTitle', step.title); text('backupStorageTutorialCopy', step.copy);
    required<HTMLButtonElement>('backupStorageTutorialNext').textContent = tutorialIndex === tutorialSteps.length - 1 ? '완료' : '다음'; tutorial.hidden = false;
    step.target.scrollIntoView({ block: 'center', behavior: 'instant' });
    positionTutorial();
    window.requestAnimationFrame(positionTutorial);
  };
  const openTutorial = () => {
    if (required('backupStoragePanel').closest<HTMLElement>('[data-app-view]')?.hidden) return;
    if (tutorialIndex < 0) policyWasOpen = policy.open;
    tutorialIndex = 0; renderTutorial();
  };
  const closeTutorial = (complete = false) => {
    clearTutorialTarget();
    required('backupStorageTutorial').hidden = true;
    if (tutorialIndex >= 0) policy.open = policyWasOpen;
    tutorialIndex = -1; if (complete) localStorage.setItem(TUTORIAL_KEY, 'complete');
  };

  required('backupStorageRefresh').addEventListener('click', () => void refresh(true));
  required('backupLegacyUndo').addEventListener('click', () => void undoCleanup());
  required('backupStorageHelp').addEventListener('click', openTutorial);
  required('backupStorageTutorialClose').addEventListener('click', () => closeTutorial(false));
  required('backupStorageTutorialNext').addEventListener('click', () => { if (tutorialIndex >= tutorialSteps.length - 1) closeTutorial(true); else { tutorialIndex += 1; renderTutorial(); } });
  document.querySelector<HTMLElement>('[data-app-view-target="backup"]')?.addEventListener('click', () => { if (localStorage.getItem(TUTORIAL_KEY) !== 'complete') window.setTimeout(openTutorial, 0); });
  document.addEventListener('click', (event) => {
    const view = (event.target as HTMLElement | null)?.closest<HTMLElement>('[data-app-view-target]')?.dataset.appViewTarget;
    if (view && view !== 'backup') closeTutorial();
  });
  document.addEventListener('keydown', (event) => { if (event.key === 'Escape' && tutorialIndex >= 0) closeTutorial(); });
  window.addEventListener('resize', positionTutorial);
  window.addEventListener('scroll', positionTutorial, true);
  return {
    refresh, invalidate,
    clear: () => { invalidate(); render(null); },
    error: (value: unknown) => { invalidate(); render({ ok: false }, String((value as Error)?.message || value)); },
  };
}
