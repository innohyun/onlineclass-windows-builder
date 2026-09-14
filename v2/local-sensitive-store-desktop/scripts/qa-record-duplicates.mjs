import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { createRequire } from 'node:module';

// Serve only the built desktop fixture at the canonical origin. No helper or real DB is contacted.
const { chromium } = createRequire(path.join(process.env.DUPLICATES_QA_RUNTIME || process.cwd(), 'package.json'))('playwright');
const desktop = path.resolve(import.meta.dirname, '..');
const dist = path.join(desktop, 'dist');
const output = path.resolve(process.env.DUPLICATES_QA_OUTPUT || path.join(desktop, '../artifacts/local-record-duplicates'));
const origin = 'http://127.0.0.1:8794';
await mkdir(output, { recursive: true });
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const evidence = { origin, fixtureOnly: true, captures: [], scenarios: [], errors: [] };

function fixture() {
  localStorage.setItem('localRecordDuplicatesTutorial:v1', 'complete');
  const body = '글쓰기 활동에서 여행 경험을 시간 순서에 따라 구체적으로 서술함. 피드백을 반영하여 당시의 느낌이 잘 드러나도록 글을 완성함. <img src=x onerror="window.fixtureXss=true">';
  const records = [
    { docId: 'fixture-keep', revisionId: 'r1', savedAtMs: 1789111320000, body, referenced: false },
    { docId: 'fixture-archive', revisionId: 'r2', savedAtMs: 1789112640000, body, referenced: false },
  ];
  const group = { groupId: 'exact-fixture', snapshotHash: 'review-1', sectionKey: 'observations', studentId: '3', studentName: '이서윤', date: '2026-09-11', body, matchReason: '같은 학생·날짜·본문·기록 맥락입니다. 저장 시간과 식별번호만 다릅니다.', keeperId: records[0].docId, records, archiveIds: [records[1].docId], canApply: true, blockedReasons: [] };
  const protectedGroup = { ...group, groupId: 'protected-fixture', studentName: '정민준', keeperId: 'protected-1', records: records.map((record, index) => ({ ...record, docId: `protected-${index + 1}`, referenced: true })), archiveIds: ['protected-2'], canApply: false, blockedReasons: ['학생기록 근거로 연결된 기록이 여러 건이어서 자동 정리할 수 없습니다.'] };
  const state = { calls: [], mode: 'normal', failures: 0, release: null };
  window.__duplicateFixture = state;
  const history = () => JSON.parse(sessionStorage.getItem('duplicate-fixture-history') || '[]');
  window.__TAURI_INTERNALS__ = { invoke: async (name, args) => {
    const input = args?.input || {};
    state.calls.push({ name, input: structuredClone(input) });
    if (name === 'scan_record_duplicates') {
      if (state.mode === 'delayed') await new Promise(resolve => { state.release = resolve; });
      if (state.mode === 'error') return { ok: false, error: 'fixture_read_failed' };
      return { ok: true, tenantId: input.tenantId, scannedCount: 12, groups: state.mode === 'empty' ? [] : [group, protectedGroup], maxArchiveCount: 200 };
    }
    if (name === 'list_record_duplicate_history') {
      if (state.failures > 0) { state.failures -= 1; throw new Error('fixture_response_lost'); }
      return { ok: true, entries: history() };
    }
    if (name === 'apply_record_duplicates') {
      await new Promise(resolve => setTimeout(resolve, 60));
      if (state.mode === 'stale') return { ok: false, error: 'duplicate_cleanup_scan_stale' };
      const entries = history();
      const replayed = entries.some(entry => entry.cleanupId === input.cleanupId);
      if (!replayed) entries.unshift({ cleanupId: input.cleanupId, createdAtMs: Date.now(), archivedCount: 1, canUndo: true, state: 'archived', records: [{ docId: records[1].docId, studentName: '이서윤', date: group.date, body }], blockedReason: '' });
      sessionStorage.setItem('duplicate-fixture-history', JSON.stringify(entries));
      if (state.mode === 'lost-response') throw new Error('fixture_response_lost');
      return { ok: true, cleanupId: input.cleanupId, archivedCount: 1, replayed };
    }
    if (name === 'undo_record_duplicate_cleanup') {
      const entries = history();
      const entry = entries.find(entry => entry.cleanupId === input.cleanupId);
      if (entry) { entry.state = 'restored'; entry.canUndo = false; }
      sessionStorage.setItem('duplicate-fixture-history', JSON.stringify(entries));
      if (state.mode === 'lost-undo-response') throw new Error('fixture_response_lost');
      return { ok: true, cleanupId: input.cleanupId, restoredCount: 1 };
    }
    if (name === 'search_local_data_records') return { ok: true, total: 0, records: [] };
    if (name === 'get_local_data_overview') return { ok: true, groups: [], counts: {}, total: 0 };
    if (name === 'list_local_students') return { ok: true, total: 0, students: [] };
    return { ok: true };
  } };
}
async function prepare(viewport) {
  const page = await browser.newPage({ viewport });
  page.on('pageerror', error => evidence.errors.push(`page:${error.message}`));
  page.on('console', message => { if (message.type() === 'error') evidence.errors.push(`console:${message.text()}`); });
  page.on('requestfailed', request => evidence.errors.push(`request:${request.url()}:${request.failure()?.errorText}`));
  page.on('response', response => { if (response.status() >= 400) evidence.errors.push(`http:${response.status()}:${response.url()}`); });
  await page.addInitScript(fixture);
  await page.route(`${origin}/**`, async route => {
    const pathname = decodeURIComponent(new URL(route.request().url()).pathname);
    const file = path.resolve(dist, `.${pathname === '/' ? '/index.html' : pathname}`);
    if (!file.startsWith(`${dist}${path.sep}`)) return route.abort();
    try {
      const contentType = ({ '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.svg': 'image/svg+xml', '.png': 'image/png' })[path.extname(file)] || 'application/octet-stream';
      await route.fulfill({ status: 200, contentType, body: await readFile(file) });
    } catch { await route.fulfill({ status: 404, body: 'not found' }); }
  });
  await page.goto(`${origin}/?designPreview=students`, { waitUntil: 'networkidle' });
  await page.evaluate(() => { document.querySelector('#backupTenantInput').value = 'fixture-tenant'; });
  await page.locator('#studentTimelineDuplicates').click();
  assert.equal(await page.locator('#recordDuplicatesScope').inputValue(), '3');
  return page;
}
async function scan(page) {
  await page.locator('#recordDuplicatesScan').click();
  await page.locator('.duplicates-group').first().waitFor();
  await page.waitForFunction(() => !document.querySelector('#recordDuplicatesScan').disabled);
}
try {
  for (const viewport of [{ width: 1366, height: 900 }, { width: 640, height: 520 }, { width: 390, height: 844 }]) {
    const page = await prepare(viewport);
    await page.locator('#recordDuplicatesHelp').click();
    const tutorial = [];
    for (let step = 1; step <= 5; step++) {
      await page.locator('[data-tutorial-step]').getByText(`${step} / 5`, { exact: true }).waitFor();
      const geometry = await page.evaluate(() => {
        const target = document.querySelector('.duplicates-tutorial-target')?.getBoundingClientRect();
        const panel = document.querySelector('#recordDuplicatesTutorial')?.getBoundingClientRect();
        return { target: target?.toJSON(), panel: panel?.toJSON(), width: innerWidth, height: innerHeight };
      });
      const { target, panel, width, height } = geometry;
      assert.ok(target && target.top >= 0 && target.bottom <= height && target.left >= 0 && target.right <= width, `tutorial target ${step} visible at ${viewport.width}`);
      assert.ok(panel && panel.top >= 0 && panel.bottom <= height && panel.left >= 0 && panel.right <= width, `tutorial panel ${step} visible at ${viewport.width}`);
      assert.ok(Math.min(target.right, panel.right) <= Math.max(target.left, panel.left) || Math.min(target.bottom, panel.bottom) <= Math.max(target.top, panel.top), `tutorial ${step} not overlapping at ${viewport.width}`);
      tutorial.push({ step, ...geometry });
      if (step === 4) await page.screenshot({ path: path.join(output, `tutorial-${viewport.width}.png`) });
      await page.locator('[data-tutorial-next]').click();
    }
    assert.equal(await page.evaluate(() => window.__duplicateFixture.calls.filter(call => /record_duplicate/.test(call.name)).length), 0, 'tutorial is read-only and does not run searches');
    await scan(page);
    assert.equal(await page.locator('.duplicates-group').count(), 2);
    assert.equal(await page.locator('[data-duplicate-select]').count(), 1, 'protected records cannot be selected');
    assert.equal(await page.locator('#recordDuplicatesGroups img').count(), 0, 'body is escaped');
    assert.equal(await page.evaluate(() => Boolean(window.fixtureXss)), false);
    await page.locator('[data-duplicate-select]').uncheck();
    assert.equal(await page.locator('#recordDuplicatesApply').isDisabled(), true);
    await page.locator('[data-duplicate-select]').check();
    await page.locator('#recordDuplicatesSummary').evaluate(element => element.scrollIntoView({ block: 'start' }));
    const screenshot = path.join(output, `review-${viewport.width}.png`);
    await page.screenshot({ path: screenshot });
    const geometry = await page.locator('#recordDuplicatesDialog').evaluate(element => ({ width: element.clientWidth, scrollWidth: element.scrollWidth, rect: element.getBoundingClientRect().toJSON() }));
    assert.ok(geometry.scrollWidth <= geometry.width, 'dialog has no horizontal overflow');
    await page.locator('#recordDuplicatesApply').evaluate(button => { button.click(); button.click(); });
    await page.locator('#recordDuplicatesStatus').getByText(/로컬 DB 재조회/).waitFor();
    assert.equal(await page.evaluate(() => window.__duplicateFixture.calls.filter(call => call.name === 'apply_record_duplicates').length), 1, 'double click is one mutation');
    await page.locator('[data-duplicate-undo]').click();
    await page.locator('#recordDuplicatesStatus').getByText(/되돌리기를 로컬 DB 재조회/).waitFor();
    await page.locator('#recordDuplicatesClose').click();
    await page.reload({ waitUntil: 'networkidle' });
    await page.evaluate(() => { document.querySelector('#backupTenantInput').value = 'fixture-tenant'; });
    await page.locator('#studentTimelineDuplicates').click();
    await page.locator('#recordDuplicatesHistoryTab').click();
    await page.locator('.duplicates-history-entry').getByText(/되돌리기 완료/).waitFor();
    await page.screenshot({ path: path.join(output, `history-${viewport.width}.png`) });
    evidence.captures.push({ viewport, screenshot, geometry, tutorial });
    await page.close();
  }
  const page = await prepare({ width: 1040, height: 720 });
  await page.evaluate(() => { window.__duplicateFixture.mode = 'empty'; });
  await page.locator('#recordDuplicatesScan').click();
  await page.locator('#recordDuplicatesSummaryText').getByText(/중복이 없습니다/).waitFor();
  evidence.scenarios.push('empty');
  await page.evaluate(() => { window.__duplicateFixture.mode = 'error'; });
  await page.locator('#recordDuplicatesScan').click();
  await page.locator('#recordDuplicatesStatus').getByText(/확인하지 못했습니다/).waitFor();
  evidence.scenarios.push('scan-error');
  await page.evaluate(() => { window.__duplicateFixture.mode = 'stale'; });
  await scan(page);
  await page.locator('#recordDuplicatesApply').click();
  await page.locator('#recordDuplicatesStatus').getByText(/검토 이후 바뀌었습니다/).waitFor();
  assert.equal(await page.locator('.duplicates-group').count(), 0);
  assert.equal(await page.locator('#recordDuplicatesApply').isDisabled(), true);
  evidence.scenarios.push('stale-review-rejected');
  await page.evaluate(() => { window.__duplicateFixture.mode = 'normal'; window.__duplicateFixture.failures = 2; });
  await scan(page);
  await page.locator('#recordDuplicatesApply').click();
  await page.locator('#recordDuplicatesStatus').getByText(/같은 정리 결과 다시 확인/).waitFor();
  const firstId = await page.evaluate(() => window.__duplicateFixture.calls.filter(call => call.name === 'apply_record_duplicates').at(-1).input.cleanupId);
  await page.locator('#recordDuplicatesApply').click();
  await page.locator('#recordDuplicatesStatus').getByText(/로컬 DB 재조회/).waitFor();
  const retryId = await page.evaluate(() => window.__duplicateFixture.calls.filter(call => call.name === 'apply_record_duplicates').at(-1).input.cleanupId);
  assert.equal(retryId, firstId);
  evidence.scenarios.push('unknown-outcome-same-id-retry');
  await page.locator('#recordDuplicatesReviewTab').click();
  await page.evaluate(() => { window.__duplicateFixture.mode = 'lost-response'; });
  await scan(page);
  await page.locator('#recordDuplicatesApply').click();
  await page.locator('#recordDuplicatesStatus').getByText(/보관 결과를 로컬 DB에서 확인/).waitFor();
  evidence.scenarios.push('lost-mutation-response-readback');
  await page.evaluate(() => { window.__duplicateFixture.mode = 'lost-undo-response'; });
  await page.locator('[data-duplicate-undo]').first().click();
  await page.locator('#recordDuplicatesStatus').getByText(/되돌리기를 로컬 DB 재조회/).waitFor();
  evidence.scenarios.push('lost-undo-response-readback');
  await page.locator('#recordDuplicatesReviewTab').click();
  await page.evaluate(() => { window.__duplicateFixture.mode = 'delayed'; });
  await page.locator('#recordDuplicatesScan').click();
  await page.waitForFunction(() => Boolean(window.__duplicateFixture.release));
  assert.equal(await page.locator('#recordDuplicatesClose').isDisabled(), true);
  await page.evaluate(() => { document.querySelector('#backupTenantInput').value = 'another-tenant'; window.__duplicateFixture.release(); });
  await page.locator('#recordDuplicatesStatus').getByText(/학급 연결이 바뀌었습니다/).waitFor();
  assert.equal(await page.locator('.duplicates-group').count(), 0);
  assert.equal(await page.locator('#recordDuplicatesApply').isDisabled(), true);
  evidence.scenarios.push('tenant-switch-drops-pending-response');
  await page.close();
} catch (error) { evidence.errors.push(error.stack || String(error)); }
finally { await browser.close(); }
evidence.passed = evidence.errors.length === 0;
await writeFile(path.join(output, 'evidence.json'), `${JSON.stringify(evidence, null, 2)}\n`);
console.log(JSON.stringify(evidence));
if (!evidence.passed) process.exitCode = 1;
