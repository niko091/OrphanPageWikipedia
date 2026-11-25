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

const manualLlmOptions=['ChatGPT','Gemini','Claude','Qwen','Kimi','GLM','Mistral','Altro'];

async function stop(server: ChildProcess): Promise<void> {
  if (server.exitCode !== null || server.signalCode !== null) return;
  await new Promise<void>(resolve => {
    const timer = setTimeout(() => server.kill('SIGKILL'), 5_000);
    server.once('exit', () => {clearTimeout(timer); resolve();});
    server.kill('SIGTERM');
  });
}

const test = base.extend<{traffic: {editorial: string[]}}, {site: string; proposalPreview: boolean}>({
  proposalPreview: [false, {scope: 'worker', option: true}],
  site: [async ({proposalPreview}, use) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'orphanwiki-browser-'));
    let server: ChildProcess | undefined;
    try {
      const {web}=prepareSite(directory,proposalPreview);
      const data=path.join(web,'data');
      for(const name of fs.readdirSync(data))if(name!=='collection.sqlite')fs.rmSync(path.join(data,name),{recursive:true});
      server = spawn(executable, ['serve', '--web-root', web, '--database', path.join(directory, 'reviews.sqlite'),
        '--bind', '127.0.0.1', '--port', '0'], {cwd: root,
        env:proposalPreview?{...env,ORPHANWIKI_LIFT_WING:'preview'}:env, stdio: ['ignore', 'pipe', 'pipe']});
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
  await expect(page.locator('#download, a[download], a[href*="api/export"]')).toHaveCount(0);
  // Cards hydrate after the index: inspect layout only when their controls are ready.
  if (filename === 'deorphanize.html') await expect(page.locator('#candidate-cards button.fix-link')).toHaveCount(16);
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
  await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(16);
  await expect(page.locator('#candidate-cards button.fix-link')).toHaveCount(16);
  await page.locator('#candidate-more').click();
  await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(16);
  await expect(page.locator('#candidate-cards button.fix-link')).toHaveCount(16);
  await expect(page.locator('#candidate-info')).toContainText('Risultati 17–32 · Pagina 2 di 2');
  await inspectPagination(page,testInfo,'candidate');
  await page.locator('#candidate-previous').click();await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(16);
  await page.locator('#candidate-search').fill(title(1));
  await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(1);
  await page.locator('#candidate-cards a[href*="connections.html"]').click();
  await expect(page.locator('#candidate-hidden')).toHaveCount(0);
  await expect(page.locator('.toolbar, #llm-provider, #connection-copy-prompt, #download, #review-service-status')).toHaveCount(0);
  await expect(page.locator('#connection-candidates .connection-card')).toHaveCount(1);
  await expect(page.locator('.connection-evidence').first()).toBeHidden();
  await page.locator('.connection-details summary').first().click();
  await expect(page.locator('.connection-evidence').first()).toBeVisible();
  await expect(page.locator('#connection-title')).toHaveText('Analyzing and Categorizing Orphan Pages in Wikipedia');
  await expect(page.locator('#connection-target')).toContainText(title(1));
  const search=page.getByRole('searchbox',{name:'Cerca per titolo'});
  await expect(search).toBeVisible();
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth+1)).toBe(true);
  await page.setViewportSize({width:1280,height:844});
  await page.locator('#ui-language-toggle').click();
  await page.locator('#connection-reviewed').click();
  await page.locator('html').evaluate(node=>{node.dataset.theme='dark';});
  const searchBounds=await page.getByRole('searchbox',{name:'Search by title'}).boundingBox();
  assert(searchBounds);
  for(const id of ['connection-open','connection-reviewed']) {
    const buttonBounds=await page.locator('#'+id).boundingBox();
    assert(buttonBounds);
    expect(searchBounds.x).toBeGreaterThan(buttonBounds.x+buttonBounds.width);
    expect(searchBounds.y).toBeCloseTo(buttonBounds.y,0);
    expect(searchBounds.height).toBeCloseTo(buttonBounds.height,0);
  }
  await page.locator('.connection-toolbar').screenshot({path:testInfo.outputPath('connection-toolbar-dark.png')});
  expect(traffic.editorial).toEqual([]);
});

test('deorphanize requires a language after aggregate navigation and preserves specific editions',async({page,site,traffic})=>{
  for(const filename of ['index.html','creators.html']) {
    await open(page,site,filename);
    await expect(page.locator('#dataset-date')).toBeVisible();
    await expect(page.locator('#dataset option[value="all"]')).toHaveCount(1);
    await page.locator('#dataset').selectOption('all');
    await expect(page.locator('body')).toHaveAttribute('data-loading','false');
    await page.locator('#deorphanize-link').click();
    await expect(page.locator('#dataset')).toHaveValue('');
    await expect(page.locator('#dataset option[value="all"]')).toHaveCount(0);
    await expect(page.locator('#dataset option:checked')).toHaveText('Seleziona le lingue');
    await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(0);
    await expect(page.locator('#dataset-date')).toHaveCount(0);
    await expect(page.locator('[data-i18n="Data di estrazione"]')).toHaveCount(0);
    await page.locator('#ui-language-toggle').click();
    await expect(page.locator('#dataset option:checked')).toHaveText('Select Languages');
    await page.locator('#ui-language-toggle').click();
    await page.locator('#dataset').selectOption('lmo');
    await expect(page.locator('body')).toHaveAttribute('data-loading','false');
    await page.locator(filename==='index.html'?'#articles-link':'#creators-link').click();
    await expect(page.locator('#dataset')).toHaveValue('lmo');
    await page.locator('#deorphanize-link').click();
    await expect(page.locator('#dataset')).toHaveValue('lmo');
    await expect(page.locator('body')).toHaveAttribute('data-loading','false');
  }
  expect(traffic.editorial).toEqual([]);
});

test('article chart references stay readable in both languages and themes',async({page,site},testInfo)=>{
  await open(page,site,'index.html');
  for(const width of [1280,390])for(const theme of ['light','dark']) {
    await page.setViewportSize({width,height:844});
    await page.locator('html').evaluate((node,value)=>{node.dataset.theme=value;},theme);
    for(const mode of ['percent','cumulative']) {
      await page.locator('#chart-type').selectOption(mode);
      for(const id of ['age','length']) {
        const chart=page.locator('#'+id);await chart.scrollIntoViewIfNeeded();
        // Resize redraws replace the SVG after a debounce. Read the chart and
        // its current labels together so detached text cannot yield zero bounds.
        await expect(async()=>{
          const {bounds,labels}=await chart.evaluate(node=>{
            const rect=node.getBoundingClientRect();
            const labels=Array.from(node.querySelectorAll('svg text')).filter(label=>label.getAttribute('y')===
              String(Number(label.closest('svg')!.getAttribute('viewBox')!.split(' ')[3])-76+30)).map(label=>{
                const rect=label.getBoundingClientRect();
                return {text:label.textContent,left:rect.left,right:rect.right,width:rect.width};
              });
            return {bounds:{left:rect.left,right:rect.right},labels};
          });
          expect(labels.length).toBeGreaterThan(id==='length'?1:0);
          expect(new Set(labels.map(label=>label.text)).size).toBe(labels.length);
          for(const label of labels) {
            expect(label.width).toBeGreaterThan(0);
            expect(label.left).toBeGreaterThanOrEqual(bounds.left-1);
            expect(label.right).toBeLessThanOrEqual(bounds.right+1);
          }
        }).toPass({timeout:10_000});
        if(mode==='percent')await expect(chart).toContainText('Le barre alle estremità');
        else await expect(chart.locator('svg path')).toHaveCount(2);
        await chart.screenshot({path:testInfo.outputPath(`${id}-${mode}-${width}-${theme}.png`)});
      }
    }
  }
  await page.locator('#ui-language-toggle').click();
  await page.locator('#chart-type').selectOption('percent');
  await expect(page.locator('#age')).toContainText('The end bars group values');
  await expect(page.locator('#length')).toContainText('Length (bytes, logarithmic scale)');
});

test('creator chart references stay readable at desktop and mobile widths',async({page,site},testInfo)=>{
  await open(page,site,'creators.html');
  for(const width of [1280,390])for(const theme of ['light','dark']) {
    await page.setViewportSize({width,height:844});
    await page.locator('html').evaluate((node,value)=>{node.dataset.theme=value;},theme);
    // Changing mode redraws the chart using the current available width.
    await page.locator('#chart-type').selectOption('cumulative');
    await page.locator('#chart-type').selectOption('percent');
    const chart=page.locator('#creator_tenure_days-chart');
    await chart.scrollIntoViewIfNeeded();
    const labels=await chart.locator('svg text').evaluateAll(nodes=>nodes.filter(node=>node.getAttribute('y')===
      String(Number(node.closest('svg')!.getAttribute('viewBox')!.split(' ')[3])-76+30)).map(node=>{
        const bounds=node.getBoundingClientRect();return {text:node.textContent,left:bounds.left,right:bounds.right};
      }));
    expect(labels.map(label=>label.text)).toEqual(['10']);
    for(const label of labels) {
      expect(label.left).toBeGreaterThanOrEqual(0);expect(label.right).toBeLessThanOrEqual(width);
    }
    await chart.screenshot({path:testInfo.outputPath(`creator-axis-${width}-${theme}.png`)});
  }
  await page.locator('#ui-language-toggle').click();
  await expect(page.locator('#creator_tenure_days-chart')).toContainText('The end bars group values');
});

test('continuity shows individual months and a complete overflow category',async({page,site},testInfo)=>{
  await open(page,site,'creators.html');
  const chart=page.locator('#creator_continuity_months_before_creation-chart');
  for(const width of [1280,390])for(const theme of ['light','dark']) {
    await page.setViewportSize({width,height:844});
    await page.locator('html').evaluate((node,value)=>{node.dataset.theme=value;},theme);
    await page.locator('#chart-type').selectOption('count');
    await page.locator('#chart-type').selectOption('percent');
    await chart.scrollIntoViewIfNeeded();
    await expect(chart.locator('svg text').filter({hasText:/^Mesi consecutivi$/})).toBeVisible();
    await expect(chart.locator('svg text').filter({hasText:/^>12$/})).toBeVisible();
    await expect(chart.locator('svg rect')).toHaveCount(28);
    await expect(chart.locator('svg rect[aria-label*="· 1 Mesi"]')).toHaveCount(2);
    await expect(chart.locator('svg rect[aria-label*="· 2 Mesi"]')).toHaveCount(2);
    const {bounds,labels}=await chart.evaluate(node=>{
      const bounds=node.getBoundingClientRect();
      const labels=Array.from(node.querySelectorAll('svg text'),label=>{
        const rect=label.getBoundingClientRect();
        return {left:rect.left,right:rect.right};
      });
      return {bounds:{left:bounds.left,right:bounds.right},labels};
    });
    expect(labels.length).toBeGreaterThan(0);
    for(const label of labels) {
      expect(label.left).toBeGreaterThanOrEqual(bounds.left-1);
      expect(label.right).toBeLessThanOrEqual(bounds.right+1);
    }
    await chart.screenshot({path:testInfo.outputPath(`continuity-${width}-${theme}.png`)});
  }
  await page.locator('#chart-type').selectOption('cumulative');
  await expect(chart.locator('svg path')).toHaveCount(2);
  await expect(chart).not.toContainText('Ogni barra mostra un singolo mese');
  await page.locator('#ui-language-toggle').click();
  await expect(chart.locator('svg text').filter({hasText:/^Consecutive months$/})).toBeVisible();
  await page.locator('#chart-type').selectOption('percent');
  await expect(chart).toContainText('the last groups all values above 12 months');
});

test('orphan votes preview before login, survive a successful login and toggle off', async ({page,site},testInfo)=>{
  const card=page.locator('#candidate-cards .candidate-card').first();
  const stars=card.locator('.candidate-rating button');
  let signedIn=false,loginSucceeds=false,holdSave=false;
  let releaseSave=()=>{};
  const savingGate=new Promise<void>(resolve=>{releaseSave=resolve;});
  const votes=new Map<number,number>(),submitted: Record<string,unknown>[]=[];
  await page.route('**/api/auth/session',route=>route.fulfill({json:{enabled:true,username:signedIn?'Test editor':null}}));
  await page.route('**/api/auth/login?*',route=>{
    const returnTo=new URL(route.request().url()).searchParams.get('return_to');
    assert(returnTo);
    const callback=new URL(returnTo,site);
    assert.equal(callback.origin,site);
    signedIn=loginSucceeds;
    // WebKit cannot intercept a navigation with a synthetic redirect response.
    const destination=JSON.stringify(callback.href).replaceAll('<','\\u003c');
    return route.fulfill({contentType:'text/html',body:`<script>location.replace(${destination})</script>`});
  });
  await page.route('**/api/auth/logout',route=>{
    signedIn=false;
    return route.fulfill({json:{enabled:true,username:null}});
  });
  await page.route('**/api/pages',async route=>{
    const response=await route.fetch(), result: unknown=await response.json(), query: unknown=route.request().postDataJSON();
    assert(record(result)&&record(query)&&Array.isArray(result.pages)&&typeof query.offset==='number');
    result.orphan_voting=Object.fromEntries(result.pages.map((item: unknown,index: number)=>{
      assert(record(item)&&typeof item.page_id==='number');
      const context=JSON.stringify({target_id:item.page_id,dataset:query.dataset,sort_order:query.order,
        list_page:Number(query.offset)/16+1,page_position:index+1});
      return [item.page_id,{stars:signedIn?votes.get(item.page_id)??null:null,receipt:{context,signature:Array(32).fill(123)}}];
    }));
    await route.fulfill({response,json:result});
  });
  await page.route('**/api/orphan-votes',async route=>{
    assert(signedIn);
    const body: unknown=route.request().postDataJSON();
    assert(record(body)&&record(body.receipt)&&typeof body.receipt.context==='string'&&(typeof body.stars==='number'||body.stars===null));
    const context: unknown=JSON.parse(body.receipt.context);
    assert(record(context)&&typeof context.target_id==='number');
    submitted.push({stars:body.stars,...context});
    if(body.stars===null)votes.delete(context.target_id);
    else {
      if(holdSave)await savingGate;
      votes.set(context.target_id,body.stars);
    }
    return route.fulfill({json:{stars:body.stars}});
  });
  await open(page,site,'deorphanize.html');
  await expect(page.locator('#candidate-sort')).toHaveValue('length-desc');
  await expect(card.locator('.candidate-title')).toHaveText(title(32));
  await expect(stars).toHaveCount(5);
  await page.locator('#candidate-more').click();
  await expect(page.locator('#candidate-pages [aria-current=page]')).toHaveText('2');
  const targetId=Number(new URL(await card.locator('.connect-button').getAttribute('href')??'',site).searchParams.get('page'));
  expect(targetId).toBeGreaterThan(0);
  for(const [width,theme] of [[1280,'light'],[390,'dark']] as const) {
    await page.setViewportSize({width,height:844});
    await page.locator('html').evaluate((node,value)=>{node.dataset.theme=value;},theme);
    const layout=await stars.evaluateAll(nodes=>nodes.map(node=>node.getBoundingClientRect().top));
    expect(layout.slice(0,3)).toEqual([layout[0],layout[0],layout[0]]);
    expect(layout.slice(3)).toEqual([layout[3],layout[3]]);
    expect(layout[3]).toBeGreaterThan(layout[0]);
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1)).toBe(true);
    await card.screenshot({path:testInfo.outputPath(`orphan-vote-${width}-${theme}.png`)});
  }
  const rating=card.getByRole('button',{name:'4 stelle',exact:true});
  const dialog=page.getByRole('dialog',{name:'Accedi per votare'});
  const signIn=dialog.getByRole('button',{name:'Accedi con Wikimedia',exact:true});
  await rating.focus();
  await page.keyboard.press('Space');
  await expect(dialog).toBeVisible();
  await expect(stars).toHaveText(['★','★','★','★','☆']);
  await expect(dialog.getByRole('button',{name:'Annulla'})).toBeFocused();
  await dialog.screenshot({path:testInfo.outputPath('vote-login-390-dark.png')});
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(rating).toBeFocused();
  await expect(stars).toHaveText(['☆','☆','☆','☆','☆']);
  expect(submitted).toEqual([]);

  await rating.click();
  await signIn.click();
  await expect(page.locator('#candidate-vote-notice')).toHaveText('Voto annullato.');
  await expect(page.locator('#candidate-pages [aria-current=page]')).toHaveText('2');
  await expect(rating).toHaveAttribute('aria-pressed','false');
  expect(submitted).toEqual([]);

  loginSucceeds=true;
  await rating.click();
  await signIn.click();
  await expect(page.locator('#candidate-vote-notice')).toHaveText('Voto salvato.');
  await expect(page.locator('#candidate-pages [aria-current=page]')).toHaveText('2');
  await expect(rating).toHaveAttribute('aria-pressed','true');
  await expect(stars).toHaveText(['★','★','★','★','☆']);
  expect(submitted).toEqual([{stars:4,target_id:targetId,dataset:'fixture.json',sort_order:'length-desc',list_page:2,page_position:1}]);
  expect(new URL(page.url()).searchParams.has('vote-return')).toBe(false);
  expect(await page.evaluate(()=>sessionStorage.getItem('orphanwiki-pending-vote'))).toBeNull();
  await page.locator('#candidate-previous').click();
  await expect(page.locator('#candidate-pages [aria-current=page]')).toHaveText('1');
  await page.locator('#candidate-more').click();
  await expect(rating).toHaveAttribute('aria-pressed','true');
  await rating.click();
  await expect(card.locator('.candidate-rating-status')).toHaveText('Voto rimosso.');
  await expect(stars).toHaveText(['☆','☆','☆','☆','☆']);
  holdSave=true;
  await rating.dblclick();
  try {
    await expect(stars).toHaveText(['☆','☆','☆','☆','☆']);
  } finally {releaseSave();}
  await expect(card.locator('.candidate-rating-status')).toHaveText('Voto rimosso.');
  expect(submitted.map(vote=>vote.stars)).toEqual([4,null,4,null]);
  await page.locator('#ui-language-toggle').click();
  await expect(card.getByRole('button',{name:'4 stars',exact:true})).toHaveAttribute('aria-pressed','false');
  await page.locator('#auth-toggle').click();
  await page.locator('#auth-logout').click();
  await expect(card.locator('.candidate-rating')).toHaveAttribute('aria-label',/Sign in with Wikimedia to vote/);
  await expect(stars.nth(3)).toBeEnabled();
  await expect(stars.nth(3)).toHaveAttribute('aria-pressed','false');
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
    await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(16);
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
    await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(16);
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

const previewTest = test.extend<{}, {proposalPreview: boolean}>({
  proposalPreview: true,
});

previewTest.describe('local Wikimedia proposals preview', () => {
  previewTest('shows proposals and saves reviews for every submitted pair', async ({page,site,traffic},testInfo) => {
    const submissions: unknown[]=[];
    page.on('request',request=>{
      if(new URL(request.url()).pathname==='/api/editorial/proposals')submissions.push(request.postDataJSON());
    });
    await open(page,site,'deorphanize.html');
    await expect(page.locator('#llm-provider option')).toHaveText(['QWEN (LiftWing)',...manualLlmOptions]);
    await expect(page.locator('#llm-provider')).toHaveValue('lift-wing');
    expect(submissions).toEqual([]);
    await page.locator('#candidate-search').fill(title(1));
    const card=page.locator('.candidate-card').filter({has:page.getByRole('link',{name:title(1),exact:true})});
    const trigger=card.getByRole('button',{name:'Fix this',exact:true});
    await expect(card.locator('.candidate-footer .connect-button')).toHaveText('Connetti');
    await trigger.click();
    const dialog=page.getByRole('dialog',{name:'Proposte di collegamento'});
    await expect(dialog).toBeVisible();
    await expect(dialog.getByText('Anteprima locale. Le proposte sono simulate.')).toBeVisible();
    const pairs=page.locator('#proposal-review-rows .review-row');
    await expect(pairs).toHaveCount(2);
    const suggested=pairs.filter({has:page.getByRole('link',{name:title(33),exact:true})});
    await expect(suggested.locator('p:first-child + .review-proposal table tbody tr')).toHaveCount(1);
    await expect(suggested.getByRole('table')).toContainText(`[[${title(1)}]]`);
    await expect(suggested.getByRole('columnheader').last()).toHaveText('Passaggio da modificare');
    await expect(suggested.getByRole('cell').last()).toContainText('Cerca nel wikitesto');
    await expect(suggested.getByRole('link',{name:'Vai al passaggio'})).toHaveCount(0);
    await expect(suggested.getByRole('link',{name:'Modifica su Wikipedia'})).toHaveCount(0);
    const skipped=pairs.filter({has:page.getByRole('link',{name:title(34),exact:true})});
    await expect(skipped.locator('p').nth(1)).toHaveText('Il modello non ha individuato un punto d’inserimento adatto.');
    await expect(skipped.getByRole('table')).toHaveCount(0);
    await expect(suggested.locator('.review-reason')).toHaveCount(0);
    await expect(page.locator('#proposal-status')).toBeEmpty();
    expect(submissions).toEqual([{dataset:'fixture.json',language:'vec',target_id:1,source_ids:[33,34],ui_language:'it'}]);
    expect(traffic.editorial).toEqual([]);
    for(const width of [1280,390])for(const theme of ['light','dark']) {
      await page.setViewportSize({width,height:844});
      await page.locator('html').evaluate((node,value)=>{node.dataset.theme=value;},theme);
      await expect(page.locator('#proposal-close')).toBeInViewport();
      expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1)).toBe(true);
      await dialog.screenshot({path:testInfo.outputPath(`proposals-${width}-${theme}.png`)});
    }
    await page.keyboard.press('Escape');
    await expect(dialog).not.toBeVisible();
    await expect(trigger).toBeFocused();
    await expect(page.locator('#proposal-review-rows table')).toHaveCount(0);
    await trigger.click();
    const badLink=skipped.getByRole('button',{name:'Bad link'});
    await badLink.click();
    await expect(badLink).toHaveAttribute('aria-pressed','true');
    await expect(badLink).toBeFocused();
    await expect(skipped.locator('.review-reason')).toBeVisible();
    await page.locator('#proposal-close').click();
    await open(page,site,'deorphanize.html');
    await page.locator('#candidate-search').fill(title(1));
    await trigger.click();
    await expect(pairs).toHaveCount(1);
    expect(submissions.at(-1)).toMatchObject({source_ids:[33]});
  });

  previewTest('shows zero-suggestion reasons under each pair after retry', async ({page,site},testInfo) => {
    let attempted=0;
    await page.route('**/api/editorial/proposals',async route=>{
      const response=await route.fetch();
      const body: unknown=await response.json();
      assert(record(body));
      assert.deepEqual(body.sent_source_ids,[34,35]);
      const outcomes=[{source_id:34,reason:'insufficient_evidence'},{source_id:35,reason:'unavailable'}];
      await route.fulfill({response,json:++attempted===1?{}:{...body,sent_source_ids:[34],unavailable:1,proposals:[],outcomes}});
    });
    await open(page,site,'deorphanize.html');
    await page.locator('#ui-language-toggle').click();
    await page.locator('#candidate-search').fill(title(2));
    const card=page.locator('.candidate-card').filter({has:page.getByRole('link',{name:title(2),exact:true})});
    await card.getByRole('button',{name:'Fix this',exact:true}).click();
    await expect(page.locator('#proposal-status')).toContainText('Suggestions are temporarily unavailable');
    await page.locator('#proposal-retry').click();
    await expect(page.locator('#proposal-status')).toHaveText('Pairs with unverifiable sources: 1.');
    const dialog=page.getByRole('dialog',{name:'Link suggestions'});
    const reviews=dialog.getByRole('region',{name:'Review candidate page pairs'});
    await expect(dialog.locator('#proposal-title:first-child + #proposal-reviews')).toBeVisible();
    await expect(dialog.getByRole('table')).toHaveCount(0);
    const pairs=reviews.locator('.review-row');
    await expect(pairs).toHaveCount(2);
    const sent=pairs.filter({has:page.getByRole('link',{name:title(34),exact:true})});
    const unavailable=pairs.filter({has:page.getByRole('link',{name:title(35),exact:true})});
    await expect(sent.locator('p').nth(1)).toHaveText('The model did not find enough evidence.');
    await expect(sent.getByRole('button',{name:'Bad link'})).toBeEnabled();
    await expect(unavailable.locator('p').nth(1)).toHaveText('A required source could not be verified.');
    await expect(unavailable.getByRole('button')).toHaveCount(0);
    await page.locator('html').evaluate(node=>{node.dataset.theme='dark';});
    for(const width of [1280,390]) {
      await page.setViewportSize({width,height:844});
      await expect(page.locator('#proposal-close')).toBeInViewport();
      await dialog.screenshot({path:testInfo.outputPath(`proposal-reasons-${width}.png`)});
    }
  });

  previewTest('opens the editor for a single proposed edit without signing in', async ({page,site,traffic},testInfo) => {
    let addLink=false;
    await page.route('**/api/auth/session',route=>route.fulfill({json:{enabled:true,username:null,proposals:'live'}}));
    await page.route('**/api/editorial/proposals',async route=>{
      const response=await route.fetch();
      const body: unknown=await response.json();
      assert(record(body));
      assert.deepEqual(body.sent_source_ids,[35]);
      assert(Array.isArray(body.proposals)&&body.proposals.length===1);
      const proposal: unknown=body.proposals[0];
      assert(record(proposal));
      const anchor=addLink?'Nel 1954, Città era qui':'Città, ieri - oggi';
      const edit=addLink?{action:'ADD LINK',link_text:'Città',wikitext:null}:{};
      await route.fulfill({response,json:{...body,preview:false,proposals:[{
        ...proposal,...edit,anchor,reading_text:anchor,reading_section:'Storia locale',source_revision_id:1035,reference_revision_id:2035,
      }]}});
    });
    await open(page,site,'deorphanize.html');
    await page.locator('#ui-language-toggle').click();
    await expect(page.locator('#llm-provider option').first()).toHaveText('QWEN (LiftWing)');
    await page.locator('#candidate-search').fill(title(3));
    const card=page.locator('.candidate-card').filter({has:page.getByRole('link',{name:title(3),exact:true})});
    const trigger=card.getByRole('button',{name:'Fix this',exact:true});
    await trigger.click();
    const dialog=page.getByRole('dialog',{name:'Link suggestions'});
    const pair=dialog.locator('.review-row');
    await expect(pair).toHaveCount(1);
    await expect(pair.getByRole('table')).toContainText(`[[${title(3)}]]`);
    await expect(pair.getByRole('columnheader').last()).toHaveText('Passage to edit');
    const passage=pair.getByRole('cell').last().getByRole('link',{name:'Go to passage'});
    await expect(passage).toHaveAttribute('href','https://vec.wikipedia.org/wiki/Voce_sintetica_035#Storia_locale:~:text=Citt%C3%A0%2C%20ieri%20%2D%20oggi');
    await expect(passage).toHaveAttribute('target','_blank');
    const edit=pair.getByRole('link',{name:'Edit on Wikipedia'});
    await expect(edit).toHaveAttribute('href','https://vec.wikipedia.org/w/index.php?title=Voce_sintetica_035&action=edit');
    await expect(edit).toHaveAttribute('target','_blank');
    await expect(dialog.locator('#proposal-status')).toBeEmpty();
    await expect(dialog.locator('#proposal-review-status')).toBeHidden();
    await expect(pair.getByRole('button',{name:'Fixed'})).toBeDisabled();
    expect(traffic.editorial).toEqual([]);
    for(const width of [1280,390]) {
      await page.setViewportSize({width,height:844});
      await page.locator('html').evaluate(node=>{node.dataset.theme='dark';});
      await expect(edit).toBeInViewport();
      await dialog.screenshot({path:testInfo.outputPath(`anonymous-proposal-${width}.png`)});
    }
    await dialog.getByRole('button',{name:'Close'}).click();
    addLink=true;
    await trigger.click();
    await expect(passage).toHaveAttribute('href','https://vec.wikipedia.org/wiki/Voce_sintetica_035#Storia_locale:~:text=Nel%201954%2C-,Citt%C3%A0,-era%20qui');
    await expect(pair.getByRole('cell').nth(1)).toContainText('Text to link Città');
    await dialog.getByRole('button',{name:'Close'}).click();
    await page.locator('#llm-provider').selectOption('mistral');
    await trigger.click();
    await page.locator('#llm-prompt-finished').click();
    const review=page.getByRole('dialog',{name:'Review candidate page pairs'});
    await expect(review.locator('.review-row')).toHaveCount(1);
    await expect(review.locator('#review-dialog-status')).toBeHidden();
    await expect(review.getByRole('button',{name:'Fixed'})).toBeDisabled();
  });
});

test('review badges keep the article title readable and disclose review details',async({page,site},testInfo)=>{
  let status: 'fixed' | 'bad_link'='fixed';
  const reviewer='Revisore sintetico con un nome molto lungo';
  await page.route(/\/api\/reviews\?dataset=fixture\.json$/,route=>route.fulfill({json:{
    format:'editorial_reviews_v1',reviews:[{language:'vec',source_id:33,target_id:1,status,version:1,
      updated_at:'2026-10-01T00:00:00Z',reviewer}],
  }}));
  for(const [value,label,width,theme] of [['fixed','Fixed',390,'light'],['bad_link','Bad Link',1280,'dark']] as const) {
    status=value;
    await page.setViewportSize({width,height:844});
    await open(page,site,'connections.html');
    await page.locator('html').evaluate((node,mode)=>{node.dataset.theme=mode;},theme);
    await page.locator('#connection-reviewed').click();
    const card=page.locator('#connection-candidates .connection-card');
    const heading=card.getByRole('heading',{name:title(33),exact:true});
    const articleLink=heading.getByRole('link');
    const badge=card.locator('.review-badge');
    const tooltip=card.getByRole('tooltip',{includeHidden:true});
    await expect(badge).toHaveText(label);
    expect(await articleLink.evaluate(link=>link.getClientRects().length)).toBe(1);
    const headingBox=await heading.boundingBox(),badgeBox=await badge.boundingBox();
    assert(headingBox&&badgeBox);
    expect(badgeBox.x).toBeGreaterThanOrEqual(headingBox.x+headingBox.width);
    expect(Math.abs((badgeBox.y+badgeBox.height/2)-(headingBox.y+headingBox.height/2))).toBeLessThan(1);
    await expect(tooltip).toBeHidden();
    await badge.hover();
    await expect(tooltip).toBeVisible();
    await expect(tooltip).toContainText(`Valutazione di ${reviewer}`);
    await expect(tooltip).toContainText('Data: 1 ott 2026');
    await expect(tooltip).toBeInViewport({ratio:1});
    await page.screenshot({path:testInfo.outputPath(`badge-${theme}-${width}.png`)});
    await tooltip.hover();
    await expect(tooltip).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(tooltip).toBeHidden();
    await articleLink.hover();
    await articleLink.focus();
    await page.keyboard.press('Tab');
    await expect(badge).toBeFocused();
    await expect(badge).toHaveAccessibleDescription(new RegExp(reviewer));
    await expect(tooltip).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(tooltip).toBeHidden();
    await expect(badge).toBeFocused();
  }
  await page.locator('#ui-language-toggle').click();
  await page.locator('#connection-candidates .review-badge').hover();
  await expect(page.getByRole('tooltip')).toContainText('Date: 1 Oct 2026');
});

test('editorial preparation, dialog cleanup and persistent shared review', async ({page, site, traffic},testInfo) => {
  await page.addInitScript(() => {
    let copied = '';
    Object.defineProperty(navigator, 'clipboard', {value: {
      writeText: async (text: string) => {copied = text;},
      readText: async () => copied,
    }});
  });
  await open(page, site, 'deorphanize.html');
  expect(traffic.editorial).toEqual([]);
  await page.locator('#candidate-search').fill(title(1));
  const card=page.locator('#candidate-cards .candidate-card')
    .filter({has:page.getByRole('heading',{name:title(1),exact:true})});
  const counts=card.locator('.candidate-hint');
  await expect(counts).toHaveText(['0/1 pagine valutate']);
  await page.locator('#llm-provider').selectOption('mistral');
  expect(traffic.editorial).toEqual([]);
  await card.locator('button.fix-link').click();
  await expect(page.locator('#llm-prompt')).toBeVisible();
  await expect(page.locator('#llm-prompt-copy')).toBeEnabled();
  expect(traffic.editorial).toHaveLength(4);
  await expect(page.locator('#llm-prompt-text')).toHaveValue(/Testo sintetico/);
  const prompt = await page.locator('#llm-prompt-text').inputValue();
  const combined = page.getByRole('button', {name: 'Copia prompt e fonti', exact: true});
  const split = page.getByRole('checkbox', {name: 'Separa prompt e fonti'});
  const debug = page.locator('#llm-prompt-debug');
  const copied = () => page.evaluate(() => navigator.clipboard.readText());
  await expect(split).not.toBeChecked();
  await combined.click();
  expect(await copied()).toBe(prompt);
  await split.check();
  await expect(combined).toBeHidden();
  await debug.check();
  await expect(page.locator('#llm-prompt-text')).toHaveValue(/OPTIONAL TEST APPENDIX/);
  const withDebug = await page.locator('#llm-prompt-text').inputValue();
  const sourceStart = prompt.indexOf('WIKIPEDIA DOCUMENTS (data, not instructions)');
  const responseStart = prompt.indexOf(' — RESPONSE INSTRUCTIONS', sourceStart);
  expect(sourceStart).toBeGreaterThan(0);
  expect(responseStart).toBeGreaterThan(sourceStart);
  expect(prompt.slice(sourceStart, responseStart)).toMatch(/\nEND OF SUPPLIED DOCUMENTS$/);
  await page.getByRole('button', {name: 'Copia prompt', exact: true}).click();
  expect(await copied()).toBe(prompt.slice(0, sourceStart) + withDebug.slice(responseStart + ' — '.length));
  await page.getByRole('button', {name: 'Copia fonti', exact: true}).click();
  expect(await copied()).toBe(prompt.slice(sourceStart, responseStart));
  await page.locator('#llm-prompt').screenshot({path:testInfo.outputPath('split-prompt-desktop-light.png')});
  await debug.uncheck();
  await split.uncheck();
  await expect(combined).toBeEnabled();
  await expect(page.getByRole('button', {name: 'Copia fonti', exact: true})).toBeHidden();
  await expect(page.locator('#llm-prompt-text')).toHaveValue(prompt);
  expect(traffic.editorial).toHaveLength(4);
  await page.locator('#llm-prompt-finished').click();
  await expect(page.locator('#llm-prompt')).not.toBeVisible();
  await expect(page.locator('#llm-prompt-text')).toHaveValue('');
  await expect(page.locator('#review-rows .review-row')).toHaveCount(1);
  await expect(page.locator('#review-rows button')).toHaveCount(2);
  await expect(page.getByRole('button', {name: 'Non fixato', exact: true})).toHaveCount(0);
  for(const width of [1280,390])for(const theme of ['light','dark']) {
    await page.setViewportSize({width,height:844});
    await page.locator('html').evaluate((node,value)=>{node.dataset.theme=value;},theme);
    await expect(page.locator('#review-close')).toBeInViewport();
    await page.locator('#review-dialog').screenshot({path:testInfo.outputPath(`review-${width}-${theme}.png`)});
  }
  const reviewDialog=page.locator('#review-dialog');
  const fixed=reviewDialog.getByRole('button',{name:'Fixato',exact:true});
  const badLink=reviewDialog.getByRole('button',{name:'Bad link',exact:true});
  await fixed.click();
  await expect(fixed).toHaveAttribute('aria-pressed','true');
  await expect(fixed).toHaveCSS('background-color','rgb(32, 61, 50)');
  await expect(badLink).toHaveAttribute('aria-pressed','false');
  await expect(badLink).toHaveCSS('background-color','rgb(16, 20, 24)');
  await expect(reviewDialog.locator('.review-badge')).toHaveCount(0);
  await fixed.hover();
  await expect(reviewDialog.getByRole('tooltip')).toContainText('Data:');
  await page.keyboard.press('Escape');
  await expect(reviewDialog.getByRole('tooltip')).toHaveCount(0);
  await reviewDialog.screenshot({path:testInfo.outputPath('review-fixed-390-dark.png')});
  await page.locator('#review-close').click();
  await page.locator('#candidate-search').fill(title(1));
  await expect(counts).toHaveText(['1/1 pagine valutate']);
  await page.locator('#ui-language-toggle').click();
  await expect(counts).toHaveText(['1/1 pages reviewed']);
  await expect(counts.locator('strong')).toHaveText(['1','1']);
  await page.locator('#ui-language-toggle').click();
  await open(page, site, 'connections.html');
  await expect(page.locator('#connection-candidates .review-badge')).toHaveCount(0);
  await page.locator('#connection-reviewed').click();
  await expect(page.locator('#connection-candidates .review-badge')).toHaveText('Fixed');
  const search=page.getByRole('searchbox',{name:'Cerca per titolo'});
  await search.fill('SENZA RISULTATI');
  await expect(page.locator('#connection-candidates .connection-card')).toHaveCount(0);
  await expect(page.getByText('Nessuna pagina corrisponde alla ricerca.')).toBeVisible();
  await search.fill(' SINTETICA 033 ');
  await expect(page.locator('#connection-candidates .review-badge')).toHaveText('Fixed');
  await expect(page.getByRole('button', {name: 'Fix this', exact: true})).toHaveCount(0);
  await page.setViewportSize({width:1280,height:844});
  await page.locator('html').evaluate(node=>{node.dataset.theme='light';});
  await page.getByRole('button', {name: 'Valuta coppia', exact: true}).click();
  await fixed.click();
  await expect(page.locator('#review-rows button[aria-pressed="true"]')).toHaveCount(0);
  await badLink.click();
  await expect(badLink).toHaveAttribute('aria-pressed','true');
  await expect(badLink).toHaveCSS('background-color','rgb(254, 231, 230)');
  await expect(fixed).toHaveCSS('background-color','rgb(248, 249, 250)');
  await expect(reviewDialog.locator('.review-badge')).toHaveCount(0);
  await page.locator('#review-title').hover();
  await reviewDialog.screenshot({path:testInfo.outputPath('review-bad-link-1280-light.png')});
  await badLink.click();
  await expect(page.locator('#review-rows button[aria-pressed="true"]')).toHaveCount(0);
  await page.locator('#review-close').click();
  await expect(page.locator('#connection-candidates .connection-card')).toHaveCount(0);
  await page.locator('#connection-open').click();
  await expect(page.getByRole('button', {name: 'Fix this', exact: true})).toBeEnabled();
  await search.fill('034');
  await expect(page.locator('#connection-candidates .connection-card')).toHaveCount(0);
  await search.clear();
  await expect(page.getByRole('button', {name: 'Fix this', exact: true})).toBeEnabled();
  await page.reload();
  await expect(page.getByRole('button', {name: 'Fix this', exact: true})).toBeEnabled();
  expect(traffic.editorial).toHaveLength(4);
  expect(await page.evaluate(() => JSON.stringify(localStorage))).not.toContain('Testo sintetico');
});

test('additional LLM providers retain their selection and expose plain chat links',async({page,site,traffic})=>{
  await open(page,site,'deorphanize.html');
  await expect(page.locator('#llm-provider option')).toHaveText(manualLlmOptions);
  for(const [provider,name,url] of [
    ['chatgpt','ChatGPT','https://chatgpt.com/'],
    ['claude','Claude','https://claude.ai/'],
    ['gemini','Gemini','https://gemini.google.com/'],
  ]) {
    await open(page,site,'deorphanize.html');
    const calls=traffic.editorial.length;
    await expect(page.locator(`#llm-provider option[value="${provider}"]`)).toHaveText(name);
    await page.locator('#llm-provider').selectOption(provider);
    expect(traffic.editorial).toHaveLength(calls);
    await page.reload();
    await expect(page.locator('#candidate-cards button.fix-link')).toHaveCount(16);
    await expect(page.locator('#llm-provider')).toHaveValue(provider);
    expect(traffic.editorial).toHaveLength(calls);
    await page.locator('#candidate-cards button.fix-link').first().click();
    await expect(page.locator('#llm-prompt-copy')).toBeEnabled();
    await expect(page.locator('#llm-prompt-title')).toHaveText(`Prompt per ${name}`);
    await expect(page.locator('#llm-prompt-open')).toHaveAttribute('href',url);
    await page.keyboard.press('Escape');
    await expect(page.locator('#llm-prompt-text')).toHaveValue('');
  }
});

test('mobile layout, keyboard controls and Escape cleanup', async ({page, site},testInfo) => {
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
  const split = page.getByRole('checkbox', {name: 'Split prompt'});
  await page.getByRole('checkbox', {name: 'Add DEBUG (prompt testing)'}).focus();
  await page.keyboard.press('Space');
  await page.keyboard.press('Tab');
  await expect(split).toBeFocused();
  await page.keyboard.press('Space');
  await expect(page.getByRole('button', {name: 'Copy prompt', exact: true})).toBeInViewport({ratio: 1});
  await expect(page.getByRole('button', {name: 'Copy Sources', exact: true})).toBeInViewport({ratio: 1});
  await page.locator('#llm-prompt').screenshot({path:testInfo.outputPath('split-prompt-mobile-dark.png')});
  await page.keyboard.press('Escape');
  await expect(page.locator('#llm-prompt')).not.toBeVisible();
  await expect(page.locator('#llm-prompt-text')).toHaveValue('');
  await expect(page.locator('#llm-prompt-debug')).not.toBeChecked();
  await expect(page.locator('#llm-prompt-split')).not.toBeChecked();
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
    await page.locator('#deorphanize-link').click();await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(16);
    await page.locator('#candidate-more').click();await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(16);
    await expect(page.locator('#candidate-more')).toBeDisabled();
    await page.locator('#candidate-previous').click();await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(16);
    await expect(page.locator('#candidate-more')).toBeEnabled();
    await page.locator('#candidate-more').focus();await page.keyboard.press('Enter');
    await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(16);
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
    await expect(page.locator('#candidate-cards .candidate-card')).toHaveCount(16);
    await expect(page.locator('#candidate-info')).toContainText('Page 2 of 2');
    await expect(page.locator('#candidate-more')).toHaveText('Next');
    await page.locator('nav.list-pagination').scrollIntoViewIfNeeded();
    await page.screenshot({path:testInfo.outputPath('database-mobile-dark.png')});
    expect(resources.some(url=>url.endsWith('/data/fixture.json')||url.includes('-articles.json')||url.includes('-cards-v2.json')||url.includes('-creators.json'))).toBe(false);
    expect(queries.some(value=>record(value)&&value.kind==='articles'&&value.offset===25)).toBe(true);
    expect(queries.some(value=>record(value)&&value.kind==='cards'&&value.offset===16)).toBe(true);
    expect(traffic.editorial).toEqual([]);
});

test('Connect shows attested local mentions first and reveals other candidates on demand',async({page,site,traffic})=>{
  await page.route('**/data/**/1.json',async route=>{
    const response=await route.fetch();
    const detail:unknown=await response.json();
    assert(record(detail)&&record(detail.link_candidates)&&record(detail.link_candidates['1']));
    const result=detail.link_candidates['1'];
    assert(Array.isArray(result.candidates)&&record(result.candidates[0]));
    const base=result.candidates[0];
    result.candidates=[{...base,page_id:41,title:title(41),local_mention:false,mention_revision:50,reference_count:1},
      {...base,page_id:42,title:title(42),local_mention:true,mention_revision:51,reference_count:1}];
    result.candidate_count=2;
    await route.fulfill({response,json:detail});
  });
  await open(page,site,'connections.html');
  const cards=page.locator('.connection-card'),toggle=page.locator('#connection-others');
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText(title(42));
  for(const width of [390,1280]) {
    await page.setViewportSize({width,height:844});
    for(const theme of ['dark','light']) {
      await page.locator('html').evaluate((node,mode)=>{node.dataset.theme=mode;},theme);
      await expect(toggle).toBeVisible();
      expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth+1)).toBe(true);
    }
  }
  await toggle.focus();await page.keyboard.press('Space');
  await expect(cards).toHaveCount(2);
  await expect(cards.first()).toContainText(title(42));
  await page.locator('#ui-language-toggle').click();
  await expect(cards.first()).toContainText('Orphan title found in local text');
  await expect(page.getByLabel('Include candidates without a verified mention')).toBeChecked();
  await page.locator('#connection-search').fill(title(41));
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText(title(41));
  expect(traffic.editorial).toEqual([]);
});
