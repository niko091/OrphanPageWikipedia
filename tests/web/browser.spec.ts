import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import type {ChildProcess} from 'node:child_process';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {test as base, expect} from '@playwright/test';
import type {Page, TestInfo} from '@playwright/test';

import {root,executable,title,record,env,prepareSite} from './fixture.ts';

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
      const {web}=prepareSite(directory);
      const data=path.join(web,'data');
      for(const name of fs.readdirSync(data))if(name!=='collection.sqlite')fs.rmSync(path.join(data,name),{recursive:true});
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

async function inspectPagination(page: Page,testInfo: TestInfo,prefix: string) {
  for(const width of [1280,390]) {
    await page.setViewportSize({width,height:844});
    const pagination=page.locator('nav.list-pagination');
    await pagination.scrollIntoViewIfNeeded();
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth+1)).toBe(true);
    const bounds=await pagination.boundingBox();assert(bounds);
    const controls=await pagination.locator(':scope > :not([hidden])').evaluateAll(nodes=>nodes.map(node=>{
      const rect=node.getBoundingClientRect();return {left:rect.left,right:rect.right};
    }));
    const center=(Math.min(...controls.map(rect=>rect.left))+Math.max(...controls.map(rect=>rect.right)))/2;
    expect(Math.abs(center-(bounds.x+bounds.width/2))).toBeLessThanOrEqual(1);
    await pagination.screenshot({path:testInfo.outputPath(`${prefix}-pagination-${width}.png`)});
  }
}

test('four pages load real assets, charts and prepared records', async ({page, site, traffic},testInfo) => {
  const wasm = page.waitForResponse(response => new URL(response.url()).pathname.endsWith('.wasm') && response.ok());
  await open(page, site, 'index.html');
  expect((await wasm).headers()['content-type']).toContain('application/wasm');
  await expect(page.locator('#length svg')).toBeVisible();
  await page.locator('#article-pages').getByRole('button',{name:'Pagina 2',exact:true}).click();
  await expect(page.locator('#article-rows tr')).toHaveCount(7);
  await expect(page.locator('#article-info')).toContainText('Pagina 2 di 2');
  await expect(page.locator('#article-more')).toBeDisabled();
  await inspectPagination(page,testInfo,'article');
  await page.locator('#creators-link').click();
  await expect(page.locator('body')).toHaveAttribute('data-loading', 'false');
  await expect(page.locator('#creator-rows tr')).toHaveCount(25);
  await page.locator('#creator-more').click();
  await expect(page.locator('#creator-rows tr')).toHaveCount(7);
  await expect(page.locator('#creator-info')).toContainText('Pagina 2 di 2');
  await inspectPagination(page,testInfo,'creator');
  await page.locator('#creator-previous').click();await expect(page.locator('#creator-rows tr')).toHaveCount(25);
  await page.locator('#creator-search').fill('creator:31');
  await expect(page.locator('#creator-rows tr')).toHaveCount(1);
  await page.locator('#deorphanize-link').click();
  await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(25);
  await expect(page.locator('#candidate-cards button.fix-link')).toHaveCount(25);
  await page.locator('#candidate-more').click();
  await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(7);
  await expect(page.locator('#candidate-cards button.fix-link')).toHaveCount(7);
  await expect(page.locator('#candidate-info')).toContainText('Pagina 2 di 2');
  await inspectPagination(page,testInfo,'candidate');
  await page.locator('#candidate-previous').click();await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(25);
  await page.locator('#candidate-search').fill(title(1));
  await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(1);
  await page.locator('#candidate-cards a[href*="connections.html"]').click();
  await expect(page.locator('#connection-candidates .connection-card')).toHaveCount(1);
  await expect(page.locator('#connection-title')).toContainText(title(1));
  expect(traffic.editorial).toEqual([]);
});

test('hidden pages opens independent options with keyboard and mobile support', async ({page, site, traffic}, testInfo) => {
  for (const width of [1280, 390]) {
    await page.setViewportSize({width, height: 844});
    await open(page, site, 'deorphanize.html');
    const button = page.getByRole('button', {name: 'Pagine nascoste', exact: true});
    const options = page.locator('#candidate-hidden-options');
    const disambiguation = page.getByRole('checkbox', {name: 'Mostra pagine di disambiguazione', exact: true});
    const redirects = page.getByRole('checkbox', {name: 'Mostra pagine di redirect', exact: true});
    const connections = page.getByRole('checkbox', {name: 'Mostra pagine senza collegamenti possibili', exact: true});
    await expect(options).toBeHidden();
    await expect(button).toHaveAttribute('aria-expanded', 'false');
    await button.click();
    await expect(options).toBeVisible();
    await expect(options.getByRole('group', {name:'Pagine da deorfanizzare', exact:true})).toBeVisible();
    await expect(options.getByRole('group', {name:'Collegamenti consigliati', exact:true})).toBeVisible();
    await expect(button).toHaveAttribute('aria-expanded', 'true');
    await expect(disambiguation).not.toBeChecked();
    await expect(redirects).not.toBeChecked();
    await expect(connections).not.toBeChecked();
    await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(25);
    const bounds = await options.boundingBox();
    expect(bounds).not.toBeNull();
    assert(bounds);
    expect(bounds.x).toBeGreaterThanOrEqual(0);
    expect(bounds.x + bounds.width).toBeLessThanOrEqual(width);
    expect(bounds.y).toBeGreaterThanOrEqual(0);
    expect(bounds.y + bounds.height).toBeLessThanOrEqual(844);
    for(const theme of ['light','dark']) {
      await page.locator('html').evaluate((node,value)=>{node.dataset.theme=value;},theme);
      await page.screenshot({path:testInfo.outputPath(`hidden-${width}-${theme}.png`)});
    }
    await disambiguation.check();
    await expect(redirects).not.toBeChecked();
    await expect(connections).not.toBeChecked();
    await expect(options).toBeVisible();
    await connections.check();
    await redirects.check();
    await disambiguation.uncheck();
    await expect(connections).toBeChecked();
    await expect(redirects).toBeChecked();
    await page.keyboard.press('Escape');
    await expect(options).toBeHidden();
    await expect(button).toBeFocused();
    await button.click();
    await expect(disambiguation).not.toBeChecked();
    await expect(connections).toBeChecked();
    await expect(redirects).toBeChecked();
    await connections.uncheck();
    await redirects.uncheck();
    const sourceFilter=page.getByRole('checkbox',{name:'Nascondi collegamenti da disambigue',exact:true});
    await sourceFilter.check();
    await expect(disambiguation).not.toBeChecked();
    await sourceFilter.uncheck();
    await button.click();
    await expect(options).toBeHidden();
    await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(25);
  }
  expect(traffic.editorial).toEqual([]);
});

test('article search, filtering, sorting and pagination', async ({page, site}) => {
  await open(page, site, 'index.html');
  await page.locator('#article-group').selectOption('all');
  await expect(page.locator('#article-rows tr')).toHaveCount(25);
  await page.locator('#article-more').click();
  await expect(page.locator('#article-rows tr')).toHaveCount(25);
  await expect(page.locator('#article-rows tr').first()).toContainText(title(26));
  await page.locator('#article-pages').getByRole('button',{name:'Pagina 3',exact:true}).click();
  await expect(page.locator('#article-rows tr')).toHaveCount(14);
  await expect(page.locator('#article-info')).toContainText('Risultati 51–64 · Pagina 3 di 3');
  await expect(page.locator('#article-more')).toBeDisabled();
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

test('database alone serves paginated articles, creators and cards on desktop and mobile',async({page,site,traffic},testInfo)=>{
    const resources:string[]=[],queries:unknown[]=[];
    page.on('request',request=>{
      if(request.url().includes('/data/'))resources.push(request.url());
      if(request.url().endsWith('/api/pages'))queries.push(request.postDataJSON());
    });
    await open(page,site,'index.html');
    await expect(page.locator('#article-rows tr')).toHaveCount(25);
    await page.locator('#article-group').selectOption('all');
    await expect(page.locator('#article-info')).toContainText('64');
    await expect(page.locator('#article-previous')).toBeDisabled();
    await page.locator('#article-more').click();await expect(page.locator('#article-rows tr')).toHaveCount(25);
    await expect(page.locator('#article-info')).toContainText('Risultati 26–50 · Pagina 2 di 3');
    await expect(page.locator('#article-rows tr').first()).toContainText(title(26));
    await page.locator('#article-pages').getByRole('button',{name:'Pagina 3',exact:true}).click();
    await expect(page.locator('#article-rows tr')).toHaveCount(14);
    await expect(page.locator('#article-more')).toBeDisabled();
    await expect(page.locator('#article-pages [aria-current="page"]')).toBeFocused();
    await page.locator('#article-previous').click();await expect(page.locator('#article-rows tr')).toHaveCount(25);
    await inspectPagination(page,testInfo,'article');
    await page.locator('#article-search').fill(title(64));await expect(page.locator('#article-rows tr')).toHaveCount(1);
    await expect(page.locator('#article-rows')).toContainText(title(64));
    await page.locator('#creators-link').click();await expect(page.locator('#creator-rows tr')).toHaveCount(25);
    await page.locator('#creator-more').click();await expect(page.locator('#creator-rows tr')).toHaveCount(7);
    await inspectPagination(page,testInfo,'creator');
    await expect(page.locator('#creator-info')).toContainText('Pagina 2 di 2');
    await expect(page.locator('#creator-more')).toBeDisabled();
    await page.locator('#creator-previous').click();await expect(page.locator('#creator-rows tr')).toHaveCount(25);
    await page.locator('#creator-more').click();await expect(page.locator('#creator-rows tr')).toHaveCount(7);
    await page.locator('#creator-search').fill('creator:31');await expect(page.locator('#creator-rows tr')).toHaveCount(1);
    await page.locator('#deorphanize-link').click();await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(25);
    await page.locator('#candidate-more').click();await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(7);
    await expect(page.locator('#candidate-more')).toBeDisabled();
    await page.locator('#candidate-previous').click();await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(25);
    await page.locator('#candidate-more').focus();await page.keyboard.press('Enter');
    await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(7);
    await inspectPagination(page,testInfo,'candidate');
    for(const width of [1280,390]) {
      await page.setViewportSize({width,height:844});
      await page.getByRole('button',{name:'Pagine nascoste',exact:true}).click();
      await expect(page.locator('#candidate-hidden-options')).toBeVisible();await page.keyboard.press('Escape');
      await expect(page.locator('#candidate-hidden')).toBeFocused();
      expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth+1)).toBe(true);
      if(width===1280)await page.screenshot({path:testInfo.outputPath('database-desktop-light.png')});
    }
    await page.locator('#ui-language-toggle').click();await expect(page.locator('html')).toHaveAttribute('lang','en');
    await page.locator('#theme-toggle').click();await expect(page.locator('html')).toHaveAttribute('data-theme','dark');
    await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(7);
    await expect(page.locator('#candidate-info')).toContainText('Page 2 of 2');
    await expect(page.locator('#candidate-more')).toHaveText('Next');
    await page.locator('nav.list-pagination').scrollIntoViewIfNeeded();
    await page.screenshot({path:testInfo.outputPath('database-mobile-dark.png')});
    expect(resources.some(url=>url.endsWith('/data/fixture.json')||url.includes('-articles.json')||url.includes('-cards-v2.json')||url.includes('-creators.json'))).toBe(false);
    expect(queries.some(value=>record(value)&&value.offset===25)).toBe(true);
    expect(traffic.editorial).toEqual([]);
});
