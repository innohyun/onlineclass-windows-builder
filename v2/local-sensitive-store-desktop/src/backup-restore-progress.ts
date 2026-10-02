import { isTauri } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export type RestorePhase = 'verify_stage' | 'protected_backup_verified' | 'merge_started' | 'failure' | 'result';
export type RestoreErrorKind = 'backup_unavailable' | 'recovery_required' | 'safety_backup_failed' | 'download_pending' | 'validation_failed' | 'staging_failed' | 'conflict' | 'merge_failed' | 'other';
export type RestoreProgressEvent = {
  requestId: string;
  phase: RestorePhase;
  lastConfirmedPhase?: RestorePhase | null;
  safetyBackupVerified: boolean;
  safetyCreatedAtMs?: number;
  errorKind?: RestoreErrorKind;
};
export type RestoreProgressState = {
  active: boolean;
  status: 'idle' | 'working' | 'success' | 'failed';
  requestId: string;
  lastConfirmedPhase: RestorePhase | null;
  safetyBackupVerified: boolean;
  safetyCreatedAtMs?: number;
  errorKind?: RestoreErrorKind;
};
export type RestoreFinalResult = { ok?: boolean; error?: string; safetyBackup?: unknown };

const phases = new Set<RestorePhase>(['verify_stage', 'protected_backup_verified', 'merge_started', 'failure', 'result']);
const errorKinds = new Set<RestoreErrorKind>(['backup_unavailable', 'recovery_required', 'safety_backup_failed', 'download_pending', 'validation_failed', 'staging_failed', 'conflict', 'merge_failed', 'other']);
const initial = (): RestoreProgressState => ({ active: false, status: 'idle', requestId: '', lastConfirmedPhase: null, safetyBackupVerified: false });

/** This reducer retains confirmed facts only. The invoke result remains the completion authority. */
export function reduceRestoreProgress(state: RestoreProgressState, event: RestoreProgressEvent): RestoreProgressState {
  if (!state.active || !event || !phases.has(event.phase) || typeof event.requestId !== 'string' || !/^[A-Za-z0-9-]{1,80}$/u.test(event.requestId)) return state;
  if (!state.requestId || state.requestId !== event.requestId) return state;
  if (typeof event.safetyBackupVerified !== 'boolean') return state;
  const checkpoint = event.phase === 'failure' || event.phase === 'result' ? event.lastConfirmedPhase : event.phase;
  const validCheckpoint = checkpoint && phases.has(checkpoint) && checkpoint !== 'failure' && checkpoint !== 'result' ? checkpoint : null;
  // Once application begins, a delayed verification event cannot move the UI back before it.
  const lastConfirmedPhase = state.lastConfirmedPhase === 'merge_started' ? 'merge_started' : validCheckpoint || state.lastConfirmedPhase;
  const safetyCreatedAtMs = event.safetyBackupVerified && Number.isFinite(event.safetyCreatedAtMs) && Number(event.safetyCreatedAtMs) > 0 ? Number(event.safetyCreatedAtMs) : state.safetyCreatedAtMs;
  return {
    ...state, requestId: event.requestId, lastConfirmedPhase,
    safetyBackupVerified: state.safetyBackupVerified || event.safetyBackupVerified,
    ...(safetyCreatedAtMs ? { safetyCreatedAtMs } : {}),
    ...(event.phase === 'failure' ? { errorKind: event.errorKind && errorKinds.has(event.errorKind) ? event.errorKind : 'other' } : {}),
  };
}

export function createBackupRestoreProgress(options: { onChange: (state: RestoreProgressState) => void }) {
  let state = initial();
  let unlisten: UnlistenFn | undefined;
  let preparing: Promise<void> | undefined;
  const publish = () => options.onChange({ ...state });
  const accept = (event: RestoreProgressEvent) => {
    const next = reduceRestoreProgress(state, event);
    if (next !== state) { state = next; publish(); }
  };
  return {
    async prepare() {
      if (!isTauri() || unlisten) return;
      if (!preparing) preparing = listen<RestoreProgressEvent>('desktop-backup-restore-progress', (event) => accept(event.payload))
        .then((stop) => { unlisten = stop; }).finally(() => { preparing = undefined; });
      await preparing;
    },
    begin(expectedRequestId: string) {
      if (state.active) throw new Error('backup_restore_already_running');
      if (!/^[A-Za-z0-9-]{1,80}$/u.test(expectedRequestId)) throw new Error('backup_restore_request_id_invalid');
      state = { ...initial(), active: true, status: 'working', requestId: expectedRequestId }; publish();
    },
    accept,
    finish(result?: RestoreFinalResult | null, error?: unknown) {
      if (!state.active) return;
      // Success can be asserted only after the existing restore command returns ok:true.
      state = { ...state, active: false, status: result?.ok === true ? 'success' : 'failed' };
      if (result?.ok !== true && !state.errorKind) {
        const code = String(result?.error || (error instanceof Error ? error.message : error) || '');
        state.errorKind = code.startsWith('pre_restore_backup_failed:') ? 'safety_backup_failed' : /integrity|verify|digest/iu.test(code) ? 'validation_failed' : /observation|binding|revision_conflict/iu.test(code) ? 'conflict' : /restore_recovery_required/iu.test(code) ? 'recovery_required' : 'other';
      }
      publish();
    },
    release() { unlisten?.(); unlisten = undefined; },
    snapshot() { return { ...state }; },
  };
}
