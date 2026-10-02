import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import ts from 'typescript';

function load() {
  const source = fs.readFileSync(new URL('./backup-restore-progress.ts', import.meta.url), 'utf8');
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 } }).outputText;
  const exported = {};
  let listener;
  let stopped = false;
  vm.runInNewContext(compiled, { exports: exported, require(name) {
    if (name.endsWith('/core')) return { isTauri: () => true };
    if (name.endsWith('/event')) return { listen: async (_name, receive) => { listener = receive; return () => { stopped = true; }; } };
    throw new Error(`unexpected import ${name}`);
  }});
  return { ...exported, emit: (payload) => listener?.({ payload }), stopped: () => stopped };
}

const state = () => ({ active: true, status: 'working', requestId: 'opaque-request', lastConfirmedPhase: null, safetyBackupVerified: false });
const event = (phase, extra = {}) => ({ requestId: 'opaque-request', phase, safetyBackupVerified: false, ...extra });

test('unstarted, malformed and stale operation signals cannot establish completion or protection', () => {
  const { reduceRestoreProgress: reduce } = load();
  const idle = { ...state(), active: false };
  assert.equal(reduce(idle, event('protected_backup_verified', { safetyBackupVerified: true })), idle);
  const active = reduce(state(), event('verify_stage'));
  const awaiting = { ...state(), requestId: 'new-request' };
  assert.equal(reduce(awaiting, event('verify_stage')), awaiting);
  assert.equal(reduce(awaiting, event('protected_backup_verified', { safetyBackupVerified: true })), awaiting);
  assert.equal(reduce(awaiting, event('failure')), awaiting);
  assert.equal(reduce(active, event('protected_backup_verified', { requestId: 'old-request', safetyBackupVerified: true })), active);
  assert.equal(reduce(active, event('invented')), active);
  assert.equal(active.status, 'working');
  assert.equal(active.safetyBackupVerified, false);
});

test('confirmed protection survives a staging failure and application never regresses to pre-apply', () => {
  const { reduceRestoreProgress: reduce } = load();
  let current = reduce(state(), event('verify_stage'));
  current = reduce(current, event('protected_backup_verified', { safetyBackupVerified: true, safetyCreatedAtMs: 1000 }));
  current = reduce(current, event('verify_stage', { safetyBackupVerified: true }));
  current = reduce(current, event('failure', { lastConfirmedPhase: 'verify_stage', safetyBackupVerified: true, errorKind: 'staging_failed' }));
  assert.equal(current.safetyBackupVerified, true);
  assert.equal(current.safetyCreatedAtMs, 1000);
  assert.equal(current.errorKind, 'staging_failed');
  assert.equal(current.status, 'working');
  current = reduce(current, event('merge_started', { safetyBackupVerified: true }));
  current = reduce(current, event('verify_stage', { safetyBackupVerified: true }));
  assert.equal(current.lastConfirmedPhase, 'merge_started');
});

test('the invoke return controls completion, including no-event success and no-event failure', async () => {
  const api = load();
  const snapshots = [];
  const controller = api.createBackupRestoreProgress({ onChange: (value) => snapshots.push(value) });
  await controller.prepare(); controller.begin('opaque-request');
  api.emit(event('verify_stage'));
  api.emit(event('result', { safetyBackupVerified: true, lastConfirmedPhase: 'merge_started' }));
  assert.equal(controller.snapshot().status, 'working');
  controller.finish({ ok: false, error: 'restore_recovery_required:private/path/body' });
  assert.equal(controller.snapshot().status, 'failed');
  assert.equal(controller.snapshot().errorKind, 'recovery_required');
  assert.equal(JSON.stringify(controller.snapshot()).includes('private'), false);
  controller.release(); assert.equal(api.stopped(), true);
  await controller.prepare(); controller.begin('new-request'); controller.finish({ ok: true });
  assert.equal(controller.snapshot().status, 'success');
  assert.equal(controller.snapshot().safetyBackupVerified, false);
  controller.release();
  assert.ok(snapshots.length >= 5);
});

test('failed or unknown event data cannot inject raw diagnostics into UI state', () => {
  const { reduceRestoreProgress: reduce } = load();
  const current = reduce(reduce(state(), event('verify_stage')), event('failure', { errorKind: 'student_body_or_absolute_path', safetyCreatedAtMs: 123 }));
  assert.equal(current.errorKind, 'other');
  assert.equal(current.safetyCreatedAtMs, undefined);
});

test('UI correlation IDs follow the bounded native wrapper contract', () => {
  const { createBackupRestoreProgress } = load();
  const controller = createBackupRestoreProgress({ onChange: () => {} });
  for (const invalid of ['', 'with_underscore', 'a'.repeat(81), '한글', 'private/path']) {
    assert.throws(() => controller.begin(invalid), /request_id_invalid/);
  }
  controller.begin('0a940380-2110-448e-807c-73023d379f99');
  assert.equal(controller.snapshot().requestId, '0a940380-2110-448e-807c-73023d379f99');
});
