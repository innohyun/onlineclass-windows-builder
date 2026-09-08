import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { createRequire } from 'node:module';

// Run from the integrated repository to reuse its Playwright installation.
const { chromium } = createRequire(path.join(process.cwd(), 'package.json'))('playwright');
const desktop = path.resolve(import.meta.dirname, '..');
const dist = path.join(desktop, 'dist');
const output = path.resolve(process.env.OBSERVATION_QA_OUTPUT || path.join(desktop, '../artifacts/observation-evidence-desktop'));
await mkdir(output, { recursive: true });
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const evidence = { origin: 'http://127.0.0.1:8794', captures: [], errors: [] };
try {
  for (const viewport of [{ width: 1040, height: 720 }, { width: 640, height: 520 }]) {
    const page = await browser.newPage({ viewport });
    const errors = [];
    page.on('pageerror', error => errors.push(`page:${error.message}`));
    page.on('console', message => { if (message.type() === 'error') errors.push(`console:${message.text()}`); });
    page.on('requestfailed', request => errors.push(`request:${request.url()}:${request.failure()?.errorText}`));
    page.on('response', response => { if (response.status() >= 400) errors.push(`http:${response.status()}:${response.url()}`); });
    await page.route('http://127.0.0.1:8794/**', async route => {
      const pathname = decodeURIComponent(new URL(route.request().url()).pathname);
      const file = path.resolve(dist, `.${pathname === '/' ? '/index.html' : pathname}`);
      if (!file.startsWith(`${dist}${path.sep}`)) return route.abort();
      try {
        const contentType = ({ '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.svg': 'image/svg+xml', '.png': 'image/png' })[path.extname(file)] || 'application/octet-stream';
        await route.fulfill({ status: 200, contentType, body: await readFile(file) });
      } catch { await route.fulfill({ status: 404, body: 'not found' }); }
    });
    await page.goto(`${evidence.origin}/?designPreview=quick-observation`, { waitUntil: 'networkidle' });
    await page.evaluate(() => { document.body.classList.add('is-desktop-shell'); document.querySelector('#desktopShellBar').hidden = false; document.querySelector('#desktopLocalArchive')?.classList.add('is-active'); });
    await page.locator('.quick-student-tile').first().click();
    await page.locator('#quickObservationContext button[data-value="recess"]').click();
    if (await page.locator('#quickObservationTimePrecision').inputValue() !== 'unknown') errors.push('unknown-not-default');
    if (!await page.locator('#quickObservationTime').isDisabled()) errors.push('unknown-time-not-disabled');
    await page.locator('#quickObservationDate').fill('2026-09-07');
    await page.locator('#quickObservationTimePrecision').selectOption('approximate');
    await page.locator('#quickObservationTime').fill('13:25');
    await page.locator('#quickObservationNote').fill('쉬는 시간에 있었던 일을 다음 날 기록하는 확인용 메모');
    await page.locator('#quickObservationNow').click();
    if (await page.locator('#quickObservationTimePrecision').inputValue() !== 'exact') errors.push('now-not-exact');
    await page.locator('#quickObservationTimePrecision').selectOption('unknown');
    if (await page.locator('#quickObservationTime').inputValue()) errors.push('unknown-time-shows-stale-value');
    await page.locator('#quickObservationContext button[data-value="lesson"]').click();
    if (!await page.locator('#quickObservationTimeFields').isHidden()) errors.push('lesson-has-daily-time');
    await page.locator('#quickObservationContext button[data-value="recess"]').click();
    await page.locator('.quick-occurrence-fields').evaluate(element => element.scrollIntoView({ block: 'center', behavior: 'instant' }));
    const screenshot = path.join(output, `occurrence-${viewport.width}x${viewport.height}.png`);
    await page.screenshot({ path: screenshot, fullPage: false });
    const geometry = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scrollWidth: document.documentElement.scrollWidth }));
    if (geometry.scrollWidth > geometry.width) errors.push('horizontal-overflow');
    await page.locator('#quickObservationHelp').click();
    const tutorial = [];
    for (let step = 1; step <= 5; step++) {
      await page.locator('#quickObservationTutorialStep').getByText(`${step} / 5`, { exact: true }).waitFor();
      const state = await page.evaluate(() => {
        const target = document.querySelector('.quick-tutorial-target')?.getBoundingClientRect();
        const panel = document.querySelector('#quickObservationTutorial')?.getBoundingClientRect();
        return { target: target?.toJSON(), panel: panel?.toJSON(), width: innerWidth, height: innerHeight };
      });
      const targetVisible = state.target && state.target.top >= 0 && state.target.bottom <= state.height && state.target.left >= 0 && state.target.right <= state.width;
      const panelVisible = state.panel && state.panel.top >= 0 && state.panel.bottom <= state.height && state.panel.left >= 0 && state.panel.right <= state.width;
      const overlap = state.target && state.panel && Math.min(state.target.right, state.panel.right) > Math.max(state.target.left, state.panel.left) && Math.min(state.target.bottom, state.panel.bottom) > Math.max(state.target.top, state.panel.top);
      if (!targetVisible) errors.push(`tutorial-target-invisible:${step}`);
      if (!panelVisible) errors.push(`tutorial-panel-cropped:${step}`);
      if (overlap) errors.push(`tutorial-panel-overlaps-target:${step}`);
      tutorial.push({ step, ...state, targetVisible, panelVisible, overlap });
      if (step === 3) await page.screenshot({ path: path.join(output, `tutorial-occurrence-${viewport.width}x${viewport.height}.png`) });
      await page.locator('#quickObservationTutorialNext').click();
    }
    await page.locator('#quickObservationSave').click();
    await page.getByText('시안 모드에서는 실제 저장하지 않습니다.').waitFor();
    evidence.captures.push({ viewport, screenshot, geometry, tutorial });
    evidence.errors.push(...errors.map(error => `${viewport.width}x${viewport.height}:${error}`));
    await page.close();
  }
} finally { await browser.close(); }
evidence.passed = evidence.errors.length === 0;
await writeFile(path.join(output, 'evidence.json'), `${JSON.stringify(evidence, null, 2)}\n`);
console.log(JSON.stringify(evidence));
if (!evidence.passed) process.exitCode = 1;
