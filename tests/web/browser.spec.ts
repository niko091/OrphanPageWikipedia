import assert from 'node:assert/strict';
import {spawn, spawnSync} from 'node:child_process';
import type {ChildProcess} from 'node:child_process';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {test as base, expect} from '@playwright/test';
import type {Page} from '@playwright/test';

const root = fileURLToPath(new URL('../../', import.meta.url));
const executable = path.join(root, 'target/debug/orphanwiki');
const title = (id: number): string => `Voce sintetica ${String(id).padStart(3, '0')}`;
const record = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);
// Never inherit editorial credentials, tracing destinations or research settings.
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('ORPHANWIKI_')));

function native(args: string[]): void {
  const result = spawnSync(executable, args, {cwd: root, env, encoding: 'utf8', timeout: 60_000});
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
}

function prepareSite(directory: string): string {
  const web = path.join(directory, 'web');
  const data = path.join(web, 'data');
  fs.mkdirSync(data, {recursive: true});
  for (const page of ['index', 'creators', 'deorphanize', 'connections']) {
    fs.copyFileSync(path.join(root, 'web', `${page}.html`), path.join(web, `${page}.html`));
  }
  fs.cpSync(path.join(root, 'web/assets'), path.join(web, 'assets'), {recursive: true});
  const inputs = ['vec', 'lmo'].map(language => {
    const graph = {
      metadata: {
        language, complete: true, demo: true, source: 'Synthetic browser test; no research data',
        item_mapping: {source: 'page_props.wikibase_item', complete: true},
        started_at: '2026-09-29T00:00:00Z', finished_at: '2026-09-29T00:00:00Z',
      },
      pages: Array.from({length: 64}, (_, index) => {
        const id = index + 1;
        return {
          page_id: id, title: title(id), wikibase_item: `Q${id}`, length_bytes: 100 + id,
          categories: ['Categoria sintetica'], page_created_at: '2020-01-01T00:00:00Z',
          links: id <= 32 ? [] : [title(id === 64 ? 33 : id + 1), ...(language === 'lmo' ? [title(id - 32)] : [])],
        };
      }),
    };
    const file = path.join(directory, `${language}-raw.json`);
    fs.writeFileSync(file, JSON.stringify(graph));
    return file;
  });
  const comparison = path.join(directory, 'comparison');
  native(['compare', ...inputs, '--output-dir', comparison, '--work-dir', path.join(directory, 'staging')]);
  const manifest: unknown = JSON.parse(fs.readFileSync(path.join(comparison, 'manifest.json'), 'utf8'));
  assert(Array.isArray(manifest));
  const member: unknown = manifest.find((value: unknown) => record(value) && value.language === 'vec');
  assert(record(member) && typeof member.file === 'string');
  const dataset: unknown = JSON.parse(fs.readFileSync(path.join(comparison, member.file), 'utf8'));
  assert(record(dataset) && record(dataset.metadata) && Array.isArray(dataset.pages));
  // Exercise editorial controls on private synthetic exports only. Production demo
  // exports deliberately disable them. All Wikimedia requests are intercepted below.
  dataset.metadata.demo = false;
  dataset.metadata.creators = {status: 'collected', language: 'vec', languages_scanned: ['vec']};
  for (const value of dataset.pages as unknown[]) {
    assert(record(value) && typeof value.page_id === 'number');
    Object.assign(value, {
      creator_key: `synthetic:creator:${(value.page_id - 1) % 32}`, creator_attribution: 'synthetic',
      creator_account_type: 'registered', creator_articles_created_total: 2, creator_articles_in_census: 2,
      creator_orphans_current: 1, creator_orphans_current_share: 0.5,
      creator_prior_articles: 0, creator_prior_edits_main: 0, creator_prior_edits_other: 0,
      creator_tenure_days: 10, creator_languages_created: ['vec'], creator_data_status: 'matched',
    });
  }
  fs.writeFileSync(path.join(data, 'fixture.json'), JSON.stringify(dataset));
  fs.writeFileSync(path.join(data, 'manifest.json'), JSON.stringify([
    {file: 'fixture.json', language: 'vec', date: dataset.metadata.finished_at, demo: false},
  ]));
  native(['dashboard', '--data-dir', data]);
  return web;
}

async function stop(server: ChildProcess): Promise<void> {
  if (server.exitCode !== null || server.signalCode !== null) return;
  await new Promise<void>(resolve => {
    const timer = setTimeout(() => server.kill('SIGKILL'), 5_000);
    server.once('exit', () => {clearTimeout(timer); resolve();});
    server.kill('SIGTERM');
  });
}

const test = base.extend<{traffic: {editorial: string[]}}, {site: string}>({
  site: [async ({}, use) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'orphanwiki-browser-'));
    let server: ChildProcess | undefined;
    try {
      const web = prepareSite(directory);
      server = spawn(executable, ['serve', '--web-root', web, '--database', path.join(directory, 'reviews.sqlite'),
        '--bind', '127.0.0.1', '--port', '0'], {cwd: root, env, stdio: ['ignore', 'pipe', 'pipe']});
      const process = server;
      let stderr = '';
      process.stderr?.on('data', (chunk: Buffer) => {stderr += chunk.toString();});
      const url = await new Promise<string>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`Server startup timed out: ${stderr}`)), 20_000);
        process.once('error', error => {clearTimeout(timer); reject(error);});
        process.once('exit', code => {clearTimeout(timer); reject(new Error(`Server exited (${code}): ${stderr}`));});
        let stdout = '';
        process.stdout?.on('data', (chunk: Buffer) => {
          stdout += chunk.toString();
          const match = stdout.match(/Dashboard: (http:\/\/127\.0\.0\.1:\d+)/);
          if (match) {clearTimeout(timer); resolve(match[1]);}
        });
      });
      await use(url);
    } finally {
      if (server) await stop(server);
      fs.rmSync(directory, {recursive: true, force: true});
    }
  }, {scope: 'worker', timeout: 120_000}],
  traffic: [async ({context, site}, use) => {
    const editorial: string[] = [], unexpected: string[] = [], errors: string[] = [];
    context.on('page', page => {
      page.on('pageerror', error => errors.push(error.message));
      page.on('response', response => {
        if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`);
      });
    });
    await context.route('**/*', async route => {
      const request = route.request(), url = new URL(request.url());
      if (url.origin === site) {await route.continue(); return;}
      const id = Number(url.searchParams.get('pageids'));
      if (request.method() === 'GET' && /^https:\/\/(vec|lmo)\.wikipedia\.org$/.test(url.origin) &&
          url.pathname === '/w/api.php' && url.searchParams.get('action') === 'query' &&
          url.searchParams.get('prop') === 'revisions' && Number.isInteger(id) && id >= 1 && id <= 64) {
        editorial.push(request.url());
        const content = `Testo sintetico per ${title(id)}. Relazione con ${title(1)}. È una fixture.`;
        await route.fulfill({json: {query: {pages: [{pageid: id, ns: 0, title: title(id), revisions: [{
          revid: 1000 + id, timestamp: '2026-09-29T00:00:00Z',
          size: Buffer.byteLength(content), sha1: createHash('sha1').update(content).digest('hex'),
          slots: {main: {contentmodel: 'wikitext', content}},
        }]}]}}});
        return;
      }
      unexpected.push(request.url());
      await route.abort('blockedbyclient');
    });
    await use({editorial});
    expect(unexpected, 'Unexpected outbound requests').toEqual([]);
    expect(errors, 'Browser errors or HTTP failures').toEqual([]);
  }, {auto: true}],
});

async function open(page: Page, site: string, filename: string): Promise<void> {
  await page.goto(`${site}/${filename}?dataset=fixture.json${filename === 'connections.html' ? '&page=1' : ''}`);
  await expect(page.locator('body')).toHaveAttribute('data-loading', 'false');
  // Cards hydrate after the index: inspect layout only when their controls are ready.
  if (filename === 'deorphanize.html') await expect(page.locator('#candidate-cards button.fix-link')).toHaveCount(25);
}

test('four pages load real assets, charts and prepared records', async ({page, site, traffic}) => {
  const wasm = page.waitForResponse(response => new URL(response.url()).pathname.endsWith('.wasm') && response.ok());
  await open(page, site, 'index.html');
  expect((await wasm).headers()['content-type']).toContain('application/wasm');
  await expect(page.locator('#length svg')).toBeVisible();
  await page.locator('#creators-link').click();
  await expect(page.locator('body')).toHaveAttribute('data-loading', 'false');
  await expect(page.locator('#creator-rows tr')).toHaveCount(25);
  await page.locator('#creator-more').click();
  await expect(page.locator('#creator-rows tr')).toHaveCount(32);
  await page.locator('#creator-search').fill('creator:31');
  await expect(page.locator('#creator-rows tr')).toHaveCount(1);
  await page.locator('#deorphanize-link').click();
  await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(25);
  await expect(page.locator('#candidate-cards button.fix-link')).toHaveCount(25);
  await page.locator('#candidate-more').click();
  await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(32);
  await expect(page.locator('#candidate-cards button.fix-link')).toHaveCount(32);
  await page.locator('#candidate-search').fill(title(1));
  await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(1);
  await page.locator('#candidate-cards a[href*="connections.html"]').click();
  await expect(page.locator('#connection-candidates .connection-card')).toHaveCount(1);
  await expect(page.locator('#connection-title')).toContainText(title(1));
  expect(traffic.editorial).toEqual([]);
});

test('article search, filtering, sorting and pagination', async ({page, site}) => {
  await open(page, site, 'index.html');
  await page.locator('#article-group').selectOption('all');
  await expect(page.locator('#article-rows tr')).toHaveCount(25);
  await page.locator('#article-more').click();
  await expect(page.locator('#article-rows tr')).toHaveCount(50);
  await page.locator('#article-search').fill(title(64));
  await expect(page.locator('#article-rows tr')).toHaveCount(1);
  await expect(page.locator('#article-rows')).toContainText(title(64));
  await page.locator('#article-group').selectOption('orphans');
  await expect(page.locator('#article-rows tr')).toHaveCount(0);
  await page.locator('#article-search').fill('');
  await page.locator('#article-sort').selectOption('length-desc');
  await expect(page.locator('#article-rows tr').first()).toContainText(title(32));
});

test('editorial preparation, dialog cleanup and persistent shared review', async ({page, site, traffic}) => {
  await open(page, site, 'connections.html');
  expect(traffic.editorial).toEqual([]);
  await page.locator('#llm-provider').selectOption('mistral');
  expect(traffic.editorial).toEqual([]);
  await page.getByRole('button', {name: 'Fix this', exact: true}).click();
  await expect(page.locator('#llm-prompt')).toBeVisible();
  await expect(page.locator('#llm-prompt-copy')).toBeEnabled();
  expect(traffic.editorial).toHaveLength(4);
  await expect(page.locator('#llm-prompt-text')).toHaveValue(/Testo sintetico/);
  const prompt = await page.locator('#llm-prompt-text').inputValue();
  await page.locator('#llm-prompt-debug').check();
  await expect(page.locator('#llm-prompt-text')).toHaveValue(/OPTIONAL TEST APPENDIX/);
  await page.locator('#llm-prompt-debug').uncheck();
  await expect(page.locator('#llm-prompt-text')).toHaveValue(prompt);
  expect(traffic.editorial).toHaveLength(4);
  await page.locator('#llm-prompt-finished').click();
  await expect(page.locator('#llm-prompt')).not.toBeVisible();
  await expect(page.locator('#llm-prompt-text')).toHaveValue('');
  await expect(page.locator('#review-rows .review-row')).toHaveCount(1);
  await page.getByRole('button', {name: 'Fixato', exact: true}).click();
  await expect(page.getByRole('button', {name: 'Fixato', exact: true})).toHaveAttribute('aria-pressed', 'true');
  await page.locator('#review-close').click();
  await page.reload();
  await expect(page.locator('#connection-candidates .review-badge')).toContainText('Fixato');
  await expect(page.getByRole('button', {name: 'Fix this', exact: true})).toHaveCount(0);
  await page.getByRole('button', {name: 'Valuta coppia', exact: true}).click();
  await page.getByRole('button', {name: 'Non fixato', exact: true}).click();
  await expect(page.getByRole('button', {name: 'Non fixato', exact: true})).toHaveAttribute('aria-pressed', 'true');
  await page.locator('#review-close').click();
  await expect(page.getByRole('button', {name: 'Fix this', exact: true})).toBeEnabled();
  expect(traffic.editorial).toHaveLength(4);
  expect(await page.evaluate(() => JSON.stringify(localStorage))).not.toContain('Testo sintetico');
});

test('mobile layout, keyboard controls and Escape cleanup', async ({page, site}) => {
  await page.setViewportSize({width: 390, height: 844});
  for (const filename of ['index.html', 'creators.html', 'deorphanize.html', 'connections.html']) {
    await open(page, site, filename);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1), filename).toBe(true);
  }
  await page.locator('#ui-language-toggle').focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  await page.keyboard.press('Tab');
  await expect(page.locator('#theme-toggle')).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  const fix = page.getByRole('button', {name: 'Fix this', exact: true});
  await fix.focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('#llm-prompt-copy')).toBeEnabled();
  await page.locator('#llm-prompt-debug').check();
  await page.keyboard.press('Escape');
  await expect(page.locator('#llm-prompt')).not.toBeVisible();
  await expect(page.locator('#llm-prompt-text')).toHaveValue('');
  await expect(page.locator('#llm-prompt-debug')).not.toBeChecked();
  await expect(fix).toBeFocused();
});
