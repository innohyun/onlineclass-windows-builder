import { launchQaChromium } from '../../tools/v3-web/qa-browser-options.mjs';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { createRequire } from 'node:module';

// Serve only the built desktop fixture at the canonical origin. No helper or real DB is contacted.
const require = createRequire(path.join(process.env.DUPLICATES_QA_RUNTIME || process.cwd(), 'package.json'));
const { chromium } = require('playwright');
const { build } = require('esbuild');
const desktop = path.resolve(process.cwd(), 'local-sensitive-store-desktop');
const sourceCommit=execFileSync('git',['rev-parse','HEAD'],{cwd:process.cwd(),encoding:'utf8'}).trim();
const sourceDirtyPaths=execFileSync('git',['status','--porcelain','--untracked-files=no'],{cwd:process.cwd(),encoding:'utf8'}).trim().split('\n').filter(Boolean);
const dist = path.join(desktop, 'dist');
const output = path.resolve(process.env.DUPLICATES_QA_OUTPUT || path.join(desktop, '../artifacts/local-record-duplicates'));
const origin = 'http://127.0.0.1:8794';
assert.equal((await fetch(`${origin}/api/v3/health`)).status, 200, 'canonical preview must be healthy');
await mkdir(output, { recursive: true });
const { browser, browserMetadata } = await launchQaChromium(chromium);
const evidence = { sourceCommit, sourceDirtyPaths, browser: browserMetadata, origin, fixtureOnly: true, scope: 'Actual duplicate-review source and index DOM/CSS with strict synthetic IPC. No real SQLite, records or Windows installer are opened.', captures: [], scenarios: [], errors: [] };

function fixture() {
  localStorage.setItem('localRecordDuplicatesTutorial:v2', 'complete');
  const body = '글쓰기 활동에서 여행 경험을 시간 순서에 따라 구체적으로 서술함. 피드백을 반영하여 당시의 느낌이 잘 드러나도록 글을 완성함. <img src=x onerror="window.fixtureXss=true">';
  const records = [
    { docId: 'fixture-keep', revisionId: 'r1', savedAtMs: Date.UTC(2026,9,2,10,2), body, referenced: false },
    { docId: 'fixture-archive', revisionId: 'r2', savedAtMs: Date.UTC(2026,9,2,10,24), body, referenced: false },
  ];
  const group = { groupId: 'exact-fixture', snapshotHash: 'review-1', sectionKey: 'observations', studentId: '3', studentName: '이서윤', date: '2026-10-02', body, matchReason: '같은 학생·날짜·본문·기록 맥락입니다. 저장 시간과 식별번호만 다릅니다.', keeperId: records[0].docId, records, archiveIds: [records[1].docId], canApply: true, blockedReasons: [] };
  const protectedGroup = { ...group, groupId: 'protected-fixture', studentName: '정민준', keeperId: 'protected-1', records: records.map((record, index) => ({ ...record, docId: `protected-${index + 1}`, referenced: true })), archiveIds: ['protected-2'], canApply: false, blockedReasons: ['학생기록 근거로 연결된 기록이 여러 건이어서 자동 정리할 수 없습니다.'] };
  const state = { calls: [], mode: 'normal', failures: 0, release: null };
  window.__duplicateFixture = state;
  const history = () => JSON.parse(sessionStorage.getItem('duplicate-fixture-history') || '[]');
  window.__duplicateInvoke = async (name, args) => {
    const input = args?.input || {};
    state.calls.push({ name, input: structuredClone(input) });
    if (!input.tenantId) throw new Error('synthetic tenant authority missing');
    if (name === 'scan_record_duplicates') {
      if (input.studentId && input.studentId !== '3') throw new Error('synthetic selected-student authority mismatch');
      if (state.mode === 'delayed') await new Promise(resolve => { state.release = resolve; });
      if (state.mode === 'error') return { ok: false, error: 'fixture_read_failed' };
      return { ok: true, tenantId: input.tenantId, scannedCount: 12, groups: state.mode === 'empty' ? [] : [group, protectedGroup], maxArchiveCount: 200 };
    }
    if (name === 'list_record_duplicate_history') {
      if (state.failures > 0) { state.failures -= 1; throw new Error('fixture_response_lost'); }
      return { ok: true, entries: history() };
    }
    if (name === 'apply_record_duplicates') {
      if (!input.cleanupId || input.groups.length !== 1 || input.groups[0].groupId !== group.groupId || input.groups[0].snapshotHash !== group.snapshotHash) throw new Error('synthetic exact review authority mismatch');
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
    throw new Error(`unexpected synthetic duplicate command: ${name}`);
  };
}
const bundle = await build({stdin:{resolveDir:desktop,sourcefile:'duplicate-review-qa.ts',contents:`
import {initRecordDuplicates} from './src/record-duplicates.ts';
import {beginDeskRestore} from './src/desk-restore-lock.ts';
window.qaBeginRestore = beginDeskRestore;
document.querySelectorAll('[data-app-view]').forEach(panel=>{panel.hidden=panel.dataset.appView!=='students';panel.classList.toggle('is-active',!panel.hidden);});
document.body.dataset.appView='students'; document.body.classList.add('is-desktop-shell');
window.qaDuplicates = initRecordDuplicates({getTenantId:()=>document.querySelector('#backupTenantInput').value,getSelectedStudent:()=>({studentId:'3',studentName:'이서윤'}),onChanged:async()=>{window.__duplicateFixture.changed=(window.__duplicateFixture.changed||0)+1;}});
document.querySelector('#studentTimelineDuplicates').addEventListener('click',()=>window.qaDuplicates.open(true));
`},bundle:true,write:false,outdir:output,format:'esm',platform:'browser',target:'chrome120',plugins:[{name:'strict-native-duplicate',setup(builder){
  builder.onResolve({filter:/^@tauri-apps\/api\/core$/},()=>({path:'core',namespace:'qa-native'}));
  builder.onLoad({filter:/.*/,namespace:'qa-native'},()=>({loader:'js',contents:'export const invoke=(name,args)=>window.__duplicateInvoke(name,args); export const isTauri=()=>true;'}));
}}]});
const javascript = bundle.outputFiles.find(file=>file.path.endsWith('.js')).text;
const css = bundle.outputFiles.filter(file=>file.path.endsWith('.css')).map(file=>file.text).join('\n');
const sourceHtml = (await readFile(path.join(dist,'index.html'),'utf8')).replace(/<script\b[^>]*>[\s\S]*?<\/script>/g,'').replace('</head>',`<style>${css}</style></head>`).replace('</body>','<script type="module" src="/__duplicates-source-qa.js"></script></body>');
async function prepare(viewport) {
  const page = await browser.newPage({ viewport });
  page.on('pageerror', error => evidence.errors.push(`page:${error.message}`));
  page.on('console', message => { if (message.type() === 'error') evidence.errors.push(`console:${message.text()}`); });
  page.on('requestfailed', request => evidence.errors.push(`request:${request.url()}:${request.failure()?.errorText}`));
  page.on('response', response => { if (response.status() >= 400) evidence.errors.push(`http:${response.status()}:${response.url()}`); });
  await page.addInitScript(fixture);
  await page.route(`${origin}/**`, async route => {
    const pathname = decodeURIComponent(new URL(route.request().url()).pathname);
    if (pathname === '/__duplicates-source-qa.js') return route.fulfill({contentType:'text/javascript',body:javascript});
    if (pathname === '/') return route.fulfill({contentType:'text/html',body:sourceHtml});
    const file = path.resolve(dist, `.${pathname === '/' ? '/index.html' : pathname}`);
    if (!file.startsWith(`${dist}${path.sep}`)) return route.abort();
    try {
      const contentType = ({ '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.svg': 'image/svg+xml', '.png': 'image/png' })[path.extname(file)] || 'application/octet-stream';
      await route.fulfill({ status: 200, contentType, body: await readFile(file) });
    } catch { await route.fulfill({ status: 404, body: 'not found' }); }
  });
  await page.goto(`${origin}/`, { waitUntil: 'networkidle' });
  await page.evaluate(() => { document.querySelector('#backupTenantInput').value = 'fixture-tenant'; });
  await page.locator('#studentTimelineDuplicates').click();
  assert.equal(await page.locator('#recordDuplicatesScope').inputValue(), 'selected');
  const toggle=page.locator('#recordDuplicatesDialog [data-student-privacy-toggle]');
  assert.equal(await toggle.getAttribute('aria-checked'),'true','privacy defaults ON');
  assert.doesNotMatch(await page.locator('#recordDuplicatesDialog').innerText(), /이서윤|정민준|글쓰기 활동/);
  await toggle.click(); assert.equal(await toggle.getAttribute('aria-checked'),'false');
  return page;
}
async function scan(page) {
  await page.locator('#recordDuplicatesScan').click();
  await page.locator('.duplicates-group').first().waitFor();
  await page.waitForFunction(() => !document.querySelector('#recordDuplicatesScan').disabled);
}
async function apply(page, doubleClick = false) {
  const before = await page.evaluate(()=>window.__duplicateFixture.calls.filter(call=>call.name==='apply_record_duplicates').length);
  const pending = (await page.locator('#recordDuplicatesApply').innerText()).includes('같은 정리');
  await page.locator('#recordDuplicatesApply').click();
  if (!pending) {
    await page.locator('#recordDuplicatesConfirm').waitFor({state:'visible'});
    assert.equal(await page.evaluate(()=>window.__duplicateFixture.calls.filter(call=>call.name==='apply_record_duplicates').length),before,'review CTA alone never writes');
    if (doubleClick) await page.locator('#recordDuplicatesConfirmApply').evaluate(button=>{button.click();button.click();});
    else await page.locator('#recordDuplicatesConfirmApply').click();
  }
}
async function undo(page) {
  const before=await page.evaluate(()=>window.__duplicateFixture.calls.filter(call=>call.name==='undo_record_duplicate_cleanup').length);
  await page.locator('[data-duplicate-undo]').first().click();
  await page.locator('[data-duplicate-undo-confirm]').waitFor();
  assert.equal(await page.evaluate(()=>window.__duplicateFixture.calls.filter(call=>call.name==='undo_record_duplicate_cleanup').length),before,'undo CTA requires current-state confirmation');
  await page.locator('[data-duplicate-undo-confirm]').click();
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
    assert.equal(await page.locator('#recordDuplicatesGroupDetail img').count(), 0, 'comparison body is escaped');
    assert.equal(await page.evaluate(()=>window.__duplicateFixture.calls.findLast(call=>call.name==='scan_record_duplicates').input.studentId),'3','opaque selected option resolves exact actual student ID');
    await page.locator('[data-duplicate-open=protected-fixture]').click(); assert.match(await page.locator('#recordDuplicatesGroupDetail').innerText(),/보호된 기록/);
    await page.locator('[data-duplicate-open=exact-fixture]').click(); assert.match(await page.locator('#recordDuplicatesGroupDetail').innerText(),/글쓰기 활동/);
    const toggle=page.locator('#recordDuplicatesDialog [data-student-privacy-toggle]'); await toggle.click();
    assert.doesNotMatch(await page.locator('#recordDuplicatesDialog').innerText(),/이서윤|정민준|글쓰기 활동|fixture-keep|fixture-archive/);
    assert.equal(await page.locator('#recordDuplicatesApply').isDisabled(),true); await toggle.click();
    assert.equal(await page.evaluate(() => Boolean(window.fixtureXss)), false);
    await page.locator('[data-duplicate-select]').uncheck();
    assert.equal(await page.locator('#recordDuplicatesApply').isDisabled(), true);
    await page.locator('[data-duplicate-select]').check();
    await page.locator('#recordDuplicatesSummary').evaluate(element => element.scrollIntoView({ block: 'start' }));
    const screenshot = path.join(output, `review-${viewport.width}.png`);
    await page.screenshot({ path: screenshot });
    const geometry = await page.locator('#recordDuplicatesDialog').evaluate(element => ({ width: element.clientWidth, scrollWidth: element.scrollWidth, rect: element.getBoundingClientRect().toJSON() }));
    assert.ok(geometry.scrollWidth <= geometry.width, 'dialog has no horizontal overflow');
    await page.locator('#recordDuplicatesApply').click(); await page.locator('#recordDuplicatesConfirmCancel').click();
    assert.equal(await page.evaluate(()=>window.__duplicateFixture.calls.filter(call=>call.name==='apply_record_duplicates').length),0);
    await page.evaluate(()=>{window.qaReleaseRestore=window.qaBeginRestore();});
    await page.locator('#recordDuplicatesApply').dispatchEvent('click');
    assert.equal(await page.evaluate(()=>window.__duplicateFixture.calls.filter(call=>call.name==='apply_record_duplicates').length),0,'restore guard blocks direct dispatch');
    await page.evaluate(()=>window.qaReleaseRestore());
    await apply(page,true);
    await page.locator('#recordDuplicatesStatus').getByText(/로컬 DB 재조회/).waitFor();
    assert.equal(await page.evaluate(() => window.__duplicateFixture.calls.filter(call => call.name === 'apply_record_duplicates').length), 1, 'double click is one mutation');
    await undo(page);
    await page.locator('#recordDuplicatesStatus').getByText(/되돌리기를 로컬 DB 재조회/).waitFor();
    await page.locator('#recordDuplicatesClose').click();
    await page.reload({ waitUntil: 'networkidle' });
    await page.evaluate(() => { document.querySelector('#backupTenantInput').value = 'fixture-tenant'; });
    await page.locator('#studentTimelineDuplicates').click();
    await page.locator('#recordDuplicatesDialog [data-student-privacy-toggle]').click();
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
  await apply(page);
  await page.locator('#recordDuplicatesStatus').getByText(/검토 이후 바뀌었습니다/).waitFor();
  assert.equal(await page.locator('.duplicates-group').count(), 0);
  assert.equal(await page.locator('#recordDuplicatesApply').isDisabled(), true);
  evidence.scenarios.push('stale-review-rejected');
  await page.evaluate(() => { window.__duplicateFixture.mode = 'normal'; window.__duplicateFixture.failures = 2; });
  await scan(page);
  await apply(page);
  await page.locator('#recordDuplicatesStatus').getByText(/같은 정리 결과 다시 확인/).waitFor();
  const firstId = await page.evaluate(() => window.__duplicateFixture.calls.filter(call => call.name === 'apply_record_duplicates').at(-1).input.cleanupId);
  await apply(page);
  await page.locator('#recordDuplicatesStatus').getByText(/로컬 DB 재조회/).waitFor();
  const retryId = await page.evaluate(() => window.__duplicateFixture.calls.filter(call => call.name === 'apply_record_duplicates').at(-1).input.cleanupId);
  assert.equal(retryId, firstId);
  evidence.scenarios.push('unknown-outcome-same-id-retry');
  await page.locator('#recordDuplicatesReviewTab').click();
  await page.evaluate(() => { window.__duplicateFixture.mode = 'lost-response'; });
  await scan(page);
  await apply(page);
  await page.locator('#recordDuplicatesStatus').getByText(/보관 결과를 로컬 DB에서 확인/).waitFor();
  evidence.scenarios.push('lost-mutation-response-readback');
  await page.evaluate(() => { window.__duplicateFixture.mode = 'lost-undo-response'; });
  await undo(page);
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
