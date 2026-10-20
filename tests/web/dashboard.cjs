// Dependency-free DOM smoke test. Does not substitute for browser layout review.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const {pathToFileURL}=require('node:url');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {createHash,webcrypto}=require('node:crypto');
const appCode=fs.readFileSync('web/assets/app.js','utf8');
const nativePath=appCode.match(/const nativeModuleUrl='([^']+)';/)[1];
const nativeReady=import(new URL(nativePath,pathToFileURL(path.resolve('web/assets/app.js'))).href).then(async bindings=>{
  const wasmFile=fs.readdirSync('web/assets/native').find(name=>name.endsWith('.wasm'));
  await bindings.default({module_or_path:fs.readFileSync(path.join('web/assets/native',wasmFile))});
  return bindings;
});
const fixtureDir=fs.mkdtempSync(path.join(os.tmpdir(),'orphanwiki-dashboard-'));
const fixtureFile=path.join(fixtureDir,'fixture.json');
const generated=spawnSync(process.env.ORPHANWIKI_PYTHON||'.venv/bin/python',['tests/python/fixtures.py','--output',fixtureFile],{encoding:'utf8'});
if(generated.status!==0){fs.rmSync(fixtureDir,{recursive:true});throw new Error(generated.stderr||generated.error||'Fixture export failed');}
const fixture=()=>JSON.parse(fs.readFileSync(fixtureFile));
class Element {
  constructor(tag) {this.tag=tag;this.children=[];this.style={};this.listeners={};this.value='';this.textContent='';}
  append(...nodes) {this.children.push(...nodes);if(this.tag==='select' && !this.value && this.children.length)this.value=this.children[0].value;}
  replaceChildren(...nodes) {this.children=[];this.append(...nodes);}
  setAttribute(k,v) {this[k]=v;}
  getAttribute(k) {return this[k];}
  addEventListener(type,handler) {this.listeners[type]=handler;}
  showModal() {this.open=true;}
  close() {this.open=false;}
}
const descendants=node=>[node,...node.children.flatMap(descendants)];
const findClass=(node,name)=>descendants(node).find(n=>n.className===name);
const titleLink=card=>findClass(card,'candidate-title').children[0];
function revisionFixture(url,roots) {
  const language=url.hostname.split('.')[0],pageId=Number(url.searchParams.get('pageids'))||null;
  let title=url.searchParams.get('titles'),id=pageId;
  const identities=[];
  for(const root of roots) {
    if(!root?.metadata)continue;
    for(const page of root.pages||[])identities.push({language:page.language||root.metadata.language,id:page.page_id,title:page.title});
    for(const [key,record] of Object.entries(root.link_candidates||{})) {
      const code=key.includes(':')?key.split(':')[0]:root.metadata.language;
      for(const candidate of record.candidates||[]) {
        identities.push({language:code,id:candidate.page_id,title:candidate.title});
        for(const edge of candidate.evidence||[]) {
          identities.push({language:edge.language,id:edge.source_page_id,title:edge.source_title});
          identities.push({language:edge.language,id:edge.target_page_id,title:edge.target_title});
        }
      }
    }
  }
  if(pageId)title=identities.find(row=>row.language===language&&row.id===pageId)?.title;
  else id=identities.find(row=>row.language===language&&row.title===title&&Number.isSafeInteger(row.id)&&row.id>0)?.id||100000;
  assert.equal(typeof title,'string',`No synthetic identity for ${url}`);
  const content=`'''${title}''' (${language})\n== Filmografia ==\nTesto integrale à ł.\n`;
  return revisionContent(id,title,content);
}
function revisionContent(id,title,content) {
  return {query:{pages:[{pageid:id,ns:0,title,revisions:[{revid:id+1000,timestamp:'2026-10-15T12:00:00Z',size:Buffer.byteLength(content),
    sha1:createHash('sha1').update(content).digest('hex'),slots:{main:{contentmodel:'wikitext',content}}}]}]}};
}
async function preparePrompt(result,control) {
  assert.equal(typeof control.listeners.click,'function');
  await control.listeners.click();
  assert.equal(result.nodes['llm-prompt'].open,true);
  return result.nodes['llm-prompt-text'].value;
}
function sourceMetadata(prompt) {
  return [...prompt.matchAll(/^DOCUMENT D\d+\n(\{[\s\S]*?^\})/gm)].map(match=>JSON.parse(match[1]));
}
async function harness(page,raw,search='?dataset=fixture.json',entries=null,preferences={},files=null) {
  const file=page==='articles'?'index.html':page+'.html';
  const html=fs.readFileSync('web/'+file,'utf8');
  const nodes={};
  for(const match of html.matchAll(/<(\w+)[^>]*\bid="([^"]+)"/g))nodes[match[2]]=new Element(match[1]);
  const staticNodes=[];
  for(const match of html.matchAll(/<(\w+)([^<>]*data-i18n[^<>]*)>/g)) {
    const id=match[2].match(/\bid="([^"]+)"/),node=id?nodes[id[1]]:new Element(match[1]);
    node.dataset={};
    for(const attr of match[2].matchAll(/(data-i18n(?:-[\w-]+)?)="([^"]+)"/g)) {
      node.setAttribute(attr[1],attr[2].replaceAll('&amp;','&').replaceAll('&quot;','"').replaceAll('&#x27;',"'"));
      if(attr[1]==='data-i18n')node.dataset.i18n=node.getAttribute(attr[1]);
    }
    staticNodes.push(node);
  }
  if(nodes['category-metric'])nodes['category-metric'].value='rate';
  const document={documentElement:{dataset:{},lang:'it'},body:{dataset:{page}},getElementById:id=>nodes[id]||null,querySelectorAll:selector=>staticNodes.filter(node=>node.getAttribute(selector.slice(1,-1))!=null),createElement:t=>new Element(t),createElementNS:(ns,t)=>new Element(t)};
  const storage=preferences.storage||new Map(),media={matches:preferences.systemDark||false,addEventListener(type,fn){this.listener=fn;}};
  const localStorage={getItem:key=>{if(preferences.blockStorage)throw new Error('storage unavailable');return storage.get(key);},setItem:(key,value)=>{if(preferences.blockStorage)throw new Error('storage unavailable');storage.set(key,value);}};
  const copied=[];
  const navigator=preferences.clipboardUnavailable?{}:{clipboard:{writeText:async text=>{if(preferences.clipboardFailure)throw new Error('clipboard denied');copied.push(text);}}};
  let fetchCount=0;const requests=[],reviewStore=preferences.reviewStore||new Map();
  const context=vm.createContext({window:{navigator,localStorage,matchMedia:()=>media,addEventListener(){},location:{search}},URL,URLSearchParams,TextEncoder,AbortController,crypto:webcrypto,setTimeout,clearTimeout,document,Intl,Map,Set,Math,Number,Option:function(text,value){const n=new Element('option');n.textContent=text;n.value=value;return n;},fetch:async (url,options)=>{requests.push({url,method:options?.method||'GET',body:options?.body,headers:options?.headers,cache:options?.cache,credentials:options?.credentials,signal:options?.signal});fetchCount++;if(preferences.beforeFetch)await preferences.beforeFetch(url,options);
    if(String(url).startsWith('api/reviews')) {
      if(preferences.reviewAPI)return preferences.reviewAPI(url,options);
      if(preferences.reviewsUnavailable)return {ok:false,status:503,json:async()=>({error:'unavailable'})};
      if(options?.method==='POST') {
        const body=JSON.parse(options.body),key=[body.language,body.source_id,body.target_id].join(':');
        const current=reviewStore.get(key);
        if((current?.version??0)!==body.version)return {ok:false,status:409,json:async()=>({current})};
        const review={language:body.language,source_id:body.source_id,target_id:body.target_id,status:body.status,
          version:body.version+1,updated_at:'2026-10-20T12:00:00Z'};reviewStore.set(key,review);
        return {ok:true,status:200,json:async()=>({review})};
      }
      return {ok:true,status:200,json:async()=>({format:'editorial_reviews_v1',reviews:[...reviewStore.values()]})};
    }
    return {ok:true,headers:{get:()=>preferences.contentLengths?.[url]??null},json:async()=>String(url).startsWith('https://')?(preferences.apiResponse?preferences.apiResponse(new URL(url),options):revisionFixture(new URL(url),[raw,...Object.values(files||{})])):options?.cache!=='no-store'&&preferences.cachedFiles?.[url]?preferences.cachedFiles[url]:url.endsWith('manifest.json')?(entries||[{language:'vec',file:'demo.json',date:'2026-10-01',demo:true},{language:'vec',file:'unused.json',date:'2026-09-28',demo:false},{language:'vec',file:'fixture.json',date:'2026-09-29',demo:false}]):files?JSON.parse(JSON.stringify(files[url.slice(5)])):raw};}});
  await nativeReady;
  vm.runInContext(appCode,context,{filename:path.resolve('web/assets/app.js'),importModuleDynamically:vm.constants.USE_MAIN_CONTEXT_DEFAULT_LOADER});
  await vm.runInContext('dashboardReady',context);
  return {nodes,context,html,staticNodes,storage,media,document,copied,requests,getFetchCount:()=>fetchCount};
}
async function checkFreshResources() {
  const {createHash}=require('node:crypto');
  const pages=['index.html','creators.html','deorphanize.html','connections.html'];
  assert.deepEqual(fs.readdirSync('web').filter(name=>name.endsWith('.html')).sort(), [...pages].sort());
  const bindings=fs.readFileSync('web/assets/native/dashboard.js');
  assert.equal(nativePath,`./native/dashboard.js?v=${createHash('sha256').update(bindings).digest('hex').slice(0,12)}`);
  const wasmFile=fs.readdirSync('web/assets/native').find(name=>name.endsWith('.wasm'));
  const wasm=fs.readFileSync(path.join('web/assets/native',wasmFile));
  assert.equal(wasmFile,`dashboard_${createHash('sha256').update(wasm).digest('hex').slice(0,12)}.wasm`);
  assert(bindings.toString().includes(wasmFile));
  for(const asset of ['app.js','style.css']) {
    const version=createHash('sha256').update(fs.readFileSync('web/assets/'+asset)).digest('hex').slice(0,12);
    for(const file of pages) {
      assert(fs.readFileSync('web/'+file,'utf8').includes(`assets/${asset}?v=${version}`),`${file}: stale ${asset} version`);
    }
  }
  for(const file of pages) {
    const html=fs.readFileSync('web/'+file,'utf8');
    assert(!html.includes('id="source"'));
    assert(!html.includes('OrphanWiki ·'));
  }
  const articlesHtml=fs.readFileSync('web/index.html','utf8');
  assert(!articlesHtml.includes('id="download"'));
  assert(!articlesHtml.includes('id="article-load"'));
  assert.match(articlesHtml,/Tassonomia completa: 64 temi[\s\S]*?https:\/\/ojs\.aaai\.org\/index\.php\/ICWSM\/article\/view\/31300/);
  const deorphanizeHtml=fs.readFileSync('web/deorphanize.html','utf8');
  assert.match(deorphanizeHtml,/<div class="toolbar">[\s\S]*?id="llm-provider"[\s\S]*?id="profile-username"[\s\S]*?<\/div>/);
  const raw=fixture();
  for(const page of raw.pages) {
    page.topic_status='predicted';page.topic_labels=['Culture.Media.Music'];
  }
  const result=await harness('articles',raw,'?dataset=fixture.json',null,{cachedFiles:{'data/fixture.json':fixture()}});
  assert.equal(result.requests.length,2);
  assert(result.requests.every(request=>request.cache==='no-store'));
  for(const id of ['topics-full','topics-paper','features-paper']) {
    assert(result.nodes[id].children.some(node=>node.className==='bar-row'));
  }
  console.log('Fresh resources: content-versioned assets on all pages, uncached manifest/data and populated new charts passed.');
}
async function checkPage(creatorsPage) {
  const creatorFields=['creator_prior_edits_main','creator_prior_edits_other','creator_prior_articles','creator_tenure_days','creator_articles_created_total','creator_orphans_current'];
  const raw=fixture();
  const {nodes,context,html}=await harness(creatorsPage?'creators':'articles',raw);
  assert(html.includes(creatorsPage?'Esplora i creatori delle voci':'Esplora le voci'));
  assert(html.includes('class="compact-table"'));
  assert(!html.includes('candidate-cards'));
  assert(!html.includes('OSSERVATORIO WIKIPEDIA'));
  assert(!html.includes('senza inferenze causali'));
  assert(!html.includes('chart-description'));
  assert.match(html, /<div class="toolbar">[\s\S]*?id="dataset"[\s\S]*?id="chart-type"[\s\S]*?<\/div>/);
  assert.match(nodes.status.textContent,/DEMO/);
  assert.equal(nodes.metrics.children.length,4);
  assert.equal(nodes.dataset.value,'vec');
  assert.equal(nodes.dataset.children.length,1);
  assert.equal(nodes['dataset-date'].value,'2026-09-29');
  assert.equal(nodes['dataset-date'].children.length,3);
  assert(nodes.dataset.children.every(option=>!option.textContent.includes('DEMO')));
  assert.equal(nodes['articles-link'].href,'index.html?dataset=fixture.json');
  assert.equal(nodes['deorphanize-link'].href,'deorphanize.html?dataset=fixture.json');
  if(!creatorsPage) {
    assert.equal(nodes['article-rows'].children.length,25);
    assert(nodes['article-rows'].children.every(row=>row.children[2].textContent==='Sì'));
    nodes['article-group'].value='all';nodes['article-group'].listeners.input();
    nodes['article-more'].listeners.click();assert.equal(nodes['article-rows'].children.length,50);
    for(const [order,field,direction] of [['length-desc','length_bytes',-1],['length-asc','length_bytes',1],['date-desc','created_at',-1],['date-asc','created_at',1]]) {
      nodes['article-sort'].value=order;nodes['article-sort'].listeners.input();
      const expected=raw.pages.filter(p=>p[field]!=null).sort((a,b)=>(field==='created_at'?Date.parse(a[field])-Date.parse(b[field]):a[field]-b[field])*direction||a.title.localeCompare(b.title));
      assert.equal(nodes['article-rows'].children[0].children[0].children[0].textContent,expected[0].title);
      assert.equal(nodes['article-rows'].children.length,25);
    }
    nodes['article-search'].value='<img';nodes['article-search'].listeners.input();
    assert.equal(nodes['article-rows'].children.length,0);
    nodes['article-search'].value='';nodes['article-category'].value='scri';nodes['article-category'].listeners.input();
    assert(nodes['article-rows'].children.every(row=>row.children[1].textContent==='Scritori'));
  } else {
    assert.equal(nodes['creator-section'].hidden,false);
    const expected=new Set(raw.pages.filter(p=>p.orphan&&p.creator_key).map(p=>p.creator_key)).size;
    assert.equal(nodes.metrics.children[2].children[0].textContent,String(expected));
    assert.equal(nodes['creator-rows'].children.length,15);
    const rows=nodes['creator-rows'].children;
    const counts=rows.map(row=>Number(row.children[4].textContent));
    assert(counts.every((count,i)=>i===0||count<=counts[i-1]));
    assert(rows.every(row=>row.children[6].textContent==='VEC'));
    const bots=nodes['creator-bots'].children;
    assert.equal(bots.length,3);
    const botCount=raw.pages.filter(p=>p.creator_key&&p.creator_is_bot_now===true).length;
    assert(botCount>0);
    assert.match(bots[0].children[2].textContent,new RegExp(`/${botCount}\\)`));
    nodes['creator-search'].value='creator:14';nodes['creator-search'].listeners.input();
    assert.equal(nodes['creator-rows'].children.length,1);
    raw.metadata.creators.status='not_collected';vm.runInContext('prepareCreators(); creatorList(); creatorCharts()',context);
    assert.equal(nodes['creator-section'].hidden,true);
    assert.match(nodes['creator-info'].textContent,/non disponibili/);
    assert.equal(nodes['creator-rows'].children.length,0);
    raw.metadata.creators.status='collected';vm.runInContext('prepareCreators(); creatorCharts()',context);
  }
  const charts=creatorsPage?creatorFields.map(field=>field+'-chart'):['age','length'];
  for(const chart of charts)assert(nodes[chart].children[0].children.some(n=>n.tag==='rect'));
  nodes['chart-type'].value='cumulative';nodes['chart-type'].listeners.change();
  for(const chart of charts)assert(nodes[chart].children[0].children.some(n=>n.tag==='path'));
  nodes['chart-type'].value='count';nodes['chart-type'].listeners.change();
  for(const width of [300,900,1600]) {
    for(const chart of charts)nodes[chart].clientWidth=width;
    vm.runInContext('renderCharts()',context);
    for(const chart of charts) {
      assert.equal(nodes[chart].children[0].viewBox,`0 0 ${width} ${width<600?380:520}`);
      assert.equal(nodes[chart].children[2].children.length,2);
    }
  }
  const chart=charts[0];nodes[chart].clientWidth=900;
  const draw=()=>vm.runInContext(`distribution('${chart}','fixture','unità','Valore')`,context);
  raw.pages=[{orphan:true,fixture:0},{orphan:true,fixture:1},{orphan:false,fixture:1},{orphan:false,fixture:20},{orphan:true,fixture:null}];
  draw();
  let bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.equal(bars.length,40);
  assert.deepEqual(bars.map((bar,i)=>Number(bar.height)>0?i:null).filter(i=>i!==null),[1,21,38]);
  assert.equal(Number(bars[1].height),206);
  assert.equal(Number(bars[21].height),103);
  assert.match(bars[0]['aria-label'],/<0 unità/);
  assert.match(bars[39]['aria-label'],/>20 unità/);
  assert.match(nodes[chart].children[3].textContent,/1 orfane, 0 non orfane/);
  nodes['chart-type'].value='percent';draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  // Two orphans share the first central bin; the missing value stays in the denominator.
  assert(Math.abs(Number(bars[1].height)-824/3)<1e-9);
  assert.equal(Number(bars[21].height),206); // one / two non-orphans
  assert.match(bars[1]['aria-label'],/66,7% \(2\/3 pagine del gruppo\)/);
  assert(!descendants(nodes[chart]).some(node=>/Denominatori|20 intervalli/.test(node.textContent)));
  const orphanHeight=bars.slice(0,20).reduce((sum,bar)=>sum+Number(bar.height),0);
  assert(Math.abs(orphanHeight/412-2/3)<1e-9); // missing coverage is not redistributed
  raw.pages=[{orphan:true,fixture:0},{orphan:true,fixture:20},
    ...Array.from({length:20},(_,i)=>({orphan:false,fixture:i<10?0:20}))];
  draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.equal(Number(bars[1].height),206);
  assert.equal(Number(bars[21].height),206); // 1/2 and 10/20 must be equally tall
  assert.match(bars[21]['aria-label'],/50% \(10\/20 pagine del gruppo\)/);
  raw.pages=[{orphan:false,fixture:0},{orphan:false,fixture:20}];draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.equal(bars.length,20); // no invented percentages for an empty group
  assert(bars.every(bar=>Number.isFinite(Number(bar.height))));
  assert.equal(nodes[chart].children.length,4);
  nodes['chart-type'].value='count';
  raw.pages=[{orphan:true,fixture:0},{orphan:true,fixture:0},{orphan:false,fixture:0}];
  draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.equal(Number(bars[10].height),206);
  assert.equal(Number(bars[30].height),103);
  nodes['chart-type'].value='cumulative';draw();
  const paths=nodes[chart].children[0].children.filter(n=>n.tag==='path');
  assert.equal(paths.length,2);
  assert(paths.every(path=>path.d.endsWith('V32'))); // original normalization: both reach 100%
  nodes['chart-type'].value='count';
  raw.pages=Array.from({length:101},(_,i)=>({orphan:i%2===0,fixture:i}));draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.match(bars[0]['aria-label'],/<2 unità: 1 pagine/);
  assert.match(bars[39]['aria-label'],/>98 unità: 1 pagine/);
  assert.equal(bars.reduce((sum,bar)=>sum+Number(bar['aria-label'].match(/: (\d+) pagine/)[1]),0),101);
  raw.pages=[];draw();
  assert.equal(nodes[chart].children[0].textContent,'Dati non disponibili.');
  raw.metadata.age_rule='first_public_revision';
  await vm.runInContext("load({file:'fixture.json'})",context);
  if(!creatorsPage)assert.match(nodes['age-note'].textContent,/page_creation_timestamp/);
  console.log(`${creatorsPage?'Creators':'Articles'} page: cards, charts, filters and missing data passed.`);
}
async function checkPaperProfiles() {
  const raw=fixture();
  raw.pages=[
    {page_id:1,title:'A',language:'vec',orphan:true,age_days:0,page_created_at:'2026-10-01',length_bytes:0,creator_key:'vec:1',creator_is_bot_now:true},
    {page_id:2,title:'B',language:'vec',orphan:true,age_days:2,page_created_at:'2026-09-29',length_bytes:10,creator_key:'vec:2',creator_is_bot_now:false},
    {page_id:3,title:'C',language:'vec',orphan:true,age_days:null,page_created_at:null,length_bytes:null,creator_is_bot_now:false},
    {page_id:4,title:'D',language:'vec',orphan:false,age_days:2,page_created_at:'2026-09-29',length_bytes:10},
    {page_id:5,title:'E',language:'en',orphan:true,age_days:100,page_created_at:'2026-06-23',length_bytes:100},
    {page_id:6,title:'F',language:'en',orphan:false,age_days:200,page_created_at:'2026-03-15',length_bytes:200},
    {page_id:7,title:'G',language:'en',orphan:false,age_days:300,page_created_at:'2025-12-05',length_bytes:300},
  ];
  const before=JSON.stringify(raw),{nodes,context,html}=await harness('articles',raw);
  assert(html.indexOf('id="categories"')<html.indexOf('id="topics-full"'));
  assert(html.indexOf('id="topics-paper"')<html.indexOf('id="topics-full"'));
  assert(html.includes('<h2 data-i18n="Macrotemi">Macrotemi</h2>'));
  assert(html.indexOf('id="features-paper"')<html.indexOf('id="age"'));
  for(const id of ['topics-full-metric','topics-paper-metric'])assert(html.includes(`id="${id}"`));
  assert.equal(vm.runInContext('Object.values(topicTaxonomy).flat().length',context),64);
  assert.equal(vm.runInContext('Object.keys(topicTaxonomy).length',context),4);
  const expectedTopics=JSON.parse(fs.readFileSync('config/topics.json','utf8')).labels;
  const actualTopics=vm.runInContext('Object.entries(topicTaxonomy).flatMap(([parent,rows])=>rows.map(row=>parent+"."+row))',context);
  assert.deepEqual(Array.from(actualTopics).sort(),expectedTopics.slice().sort());
  const rows=id=>nodes[id].children.filter(node=>node.className==='bar-row');
  const row=(id,name)=>rows(id).find(node=>node.children[0].textContent===name);
  const value=node=>node.children[2].textContent;
  const width=node=>node.children[1].children[0].style.width;
  for(const [id,count] of [['topics-full',64],['topics-paper',4],['features-paper',6]])assert.equal(rows(id).length,count);
  for(const id of ['topics-full','topics-paper']) {
    assert(rows(id).every(node=>value(node)==='— (0/0)'));
    assert(rows(id).every(node=>node.title.includes('dato non disponibile')));
    assert(!descendants(nodes[id]).some(node=>node.tag==='path'||node.tag==='circle'));
  }
  assert.equal(value(row('features-paper','Voci nuove (< mediana)')),'100% (2/2)');
  assert.equal(value(row('features-paper','Voci vecchie (≥ mediana)')),'25% (1/4)');
  assert.equal(value(row('features-paper','Lunghezza minore (< mediana)')),'100% (2/2)');
  assert(!row('features-paper','Creatore attualmente bot'));
  assert(!row('features-paper','Creatore attualmente non bot'));
  assert(!row('features-paper','Creata da bot'));
  assert(!row('features-paper','Creata da non bot'));
  assert.match(row('features-paper','Voci nuove (< mediana)').title,/copertura 6\/7/);
  assert.equal(value(row('features-paper','Qualità strutturale alta (Johnson V2, ≥ mediana)')),'— (0/0)');
  assert(!row('features-paper','Biografie di donne'));
  assert(!html.includes('id="features-thresholds"'));
  assert(!descendants(nodes['features-paper']).some(node=>/mediana età/.test(node.textContent)));
  assert.equal(JSON.stringify(raw),before);
  assert(descendants(nodes['features-paper']).some(node=>/revisioni diverse dal grafo SQL/.test(node.textContent)));
  vm.runInContext("data.metadata.measures_rule='creation_quality_creators_v2';paperProfiles()",context);
  assert(descendants(nodes['features-paper']).some(node=>/La copertura esclude revisioni diverse/.test(node.textContent)));
  vm.runInContext("data.metadata.measures_rule='creation_quality_creators_v3';paperProfiles()",context);
  assert.equal(JSON.stringify(raw),before);
  raw.pages[0].topic_status='predicted';raw.pages[0].topic_labels=['Culture.Media.Music','Culture.Media.Films','Culture.Biography.Women','STEM.Physics'];
  raw.pages[1].topic_status='predicted';raw.pages[1].topic_labels=[];
  raw.pages[3].topic_status='predicted';raw.pages[3].topic_labels=['Culture.Media.Music','Geography.Geographical'];
  const withTopics=JSON.stringify(raw);
  vm.runInContext('paperProfiles()',context);
  assert.equal(rows('topics-full').length,64);
  assert.equal(rows('topics-paper').length,4);
  assert.equal(value(row('topics-full','Culture · Media.Music')),'50% (1/2)');
  assert.equal(value(row('topics-full','Culture · Media.Films')),'100% (1/1)');
  assert.equal(value(row('topics-paper','Culture')),'50% (1/2)'); // OR children, not 3 duplicate Culture observations.
  assert.equal(value(row('topics-paper','Geography')),'0% (0/1)');
  assert.equal(width(row('topics-paper','Geography')),'0%');
  assert.equal(value(row('topics-paper','History and Society')),'— (0/0)');
  assert.match(row('topics-paper','History and Society').title,/Nessuna voce soddisfa/);
  assert.match(row('topics-paper','Culture').title,/copertura 3\/7/); // Empty predicted labels are known false.
  assert(!row('features-paper','Biografie di donne'));
  assert.equal(rows('topics-full')[0].children[0].textContent,'Culture · Biography.Women');
  const bars=()=>rows('features-paper').map(node=>[value(node),width(node)]);
  const values=bars();
  nodes['ui-language-toggle'].listeners.click();
  assert.deepEqual(bars(),values);
  assert.match(row('features-paper','New articles (< median)').title,/coverage 6\/7/);
  assert(!descendants(nodes['features-paper']).some(node=>/median age/.test(node.textContent)));
  nodes['theme-toggle'].listeners.click();
  assert.deepEqual(bars(),values);
  nodes['chart-type'].value='count';nodes['chart-type'].listeners.change();
  assert.deepEqual(bars(),values); // Numeric distribution selector does not change requirement rates.
  nodes['topics-full-metric'].value='count';nodes['topics-full-metric'].listeners.change();
  assert.equal(value(row('topics-full','Culture · Media.Music')),'1 (1/2)');
  assert.equal(width(row('topics-full','Culture · Media.Music')),'100%');
  assert.equal(value(row('topics-paper','Culture')),'50% (1/2)');
  nodes['topics-paper-metric'].value='count';nodes['topics-paper-metric'].listeners.change();
  assert.equal(value(row('topics-paper','Culture')),'1 (1/2)');
  assert.equal(value(row('topics-paper','Geography')),'0 (0/1)');
  assert.equal(value(row('topics-paper','History and Society')),'0 (0/0)');
  assert.equal(JSON.stringify(raw),withTopics);
  raw.pages.forEach(page=>page.page_created_at=null);vm.runInContext('paperProfiles()',context);
  assert.equal(value(row('features-paper','New articles (< median)')),'— (0/0)');
  // Bot membership does not add article characteristic rows.
  raw.pages=[{orphan:true,creator_key:'actor:1',creator_is_bot_now:true},{orphan:true,creator_key:'actor:2',creator_is_bot_now:false}];
  vm.runInContext('paperProfiles()',context);
  assert(!row('features-paper','Created by a bot'));
  assert(!row('features-paper','Created by a non-bot'));
  raw.pages=[];vm.runInContext('paperProfiles()',context);
  assert(rows('features-paper').every(node=>value(node)==='— (0/0)'));
  assert(rows('features-paper').every(node=>!width(node).includes('NaN')));
  console.log('Paper bars: all taxonomy rows, orphan/requirement denominators, counts, multilabel OR, missing/empty states, medians, translations and unchanged inputs passed.');
}

async function checkDeorphanize() {
  const raw=fixture();
  const {nodes,html}=await harness('deorphanize',raw);
  assert(html.includes('<h1 data-i18n="De-orfanizzare le pagine">De-orfanizzare le pagine</h1>'));
  assert(!html.includes('candidate-coverage'));
  assert(!html.includes('chart-type'));
  assert.equal(nodes['candidate-cards'].children.length,25);
  const card=nodes['candidate-cards'].children[0],footer=findClass(card,'candidate-footer');
  assert.equal(footer.children[0].tag,'span'); // synthetic titles never open Wikipedia
  assert.equal(footer.children[1].textContent,'Connetti');
  assert.match(footer.children[1].href,/^connections\.html\?dataset=fixture\.json&page=\d+$/);
  nodes['candidate-more'].listeners.click();assert.equal(nodes['candidate-cards'].children.length,50);
  nodes['candidate-search'].value='none';nodes['candidate-search'].listeners.input();
  assert.equal(nodes['candidate-cards'].children[0].textContent,'Nessuna voce orfana corrispondente.');
  nodes['candidate-search'].value='';nodes['candidate-category'].value='scri';nodes['candidate-category'].listeners.input();
  assert(nodes['candidate-cards'].children.every(card=>findClass(card,'candidate-category').textContent==='Scritori'));
  console.log('De-orfanizzare le pagine: card navigation, filters and 25-card pagination passed.');
}
async function checkDatasetSelection() {
  const raw=fixture();raw.metadata.demo=false;
  let result=await harness('articles',raw,'',[
    {language:'vec',file:'demo.json',date:'2026-10-03',demo:true},
    {language:'vec',file:'older.json',date:'2026-10-01',demo:false},
    {language:'vec',file:'latest.json',date:'2026-10-02',demo:false}
  ]);
  assert.equal(result.nodes.dataset.children.length,1);
  assert.equal(result.nodes['dataset-date'].children.length,3);
  assert.equal(result.nodes['articles-link'].href,'index.html?dataset=latest.json');
  assert.equal(result.nodes.status.hidden,true);
  result=await harness('articles',raw,'?dataset=demo.json',[{language:'vec',file:'demo.json',date:'2026-10-03',demo:true}]);
  assert.match(result.nodes.status.textContent,/Nessuna raccolta reale/);
  assert.equal(result.nodes.status.hidden,false);
  const invalid=await harness('connections',raw,'?dataset=demo.json&page=1',[{language:'vec',file:'demo.json',date:'2026-10-03',demo:true}]);
  assert.match(invalid.nodes.status.textContent,/Nessuna raccolta reale/);
  console.log('Dataset selection: demos excluded, newest real collection selected, empty state passed.');
}
async function checkCompactLists() {
  const raw=fixture(),template=raw.pages[0];
  raw.pages=Array.from({length:32},(_,i)=>({...template,page_id:i+1,title:`Title ${i}`,creator_key:`local:actor:${i}`,orphan:i%2===0,creator_articles_created_total:i===0?null:i}));
  raw.pages[0].title='<img src=x onerror=alert(1)>';
  let {nodes}=await harness('creators',raw);
  assert.equal(nodes['creator-rows'].children.length,25);
  nodes['creator-more'].listeners.click();assert.equal(nodes['creator-rows'].children.length,32);
  for(const order of ['articles-asc','articles-desc']) {
    nodes['creator-sort'].value=order;nodes['creator-sort'].listeners.input();
    nodes['creator-more'].listeners.click();
    const rows=nodes['creator-rows'].children;
    assert.equal(rows.at(-1).children[2].textContent,'—');
    assert.equal(rows[0].children[2].textContent,order.endsWith('asc')?'1':'31');
  }
  ({nodes}=await harness('articles',raw));
  nodes['article-search'].value='<img';nodes['article-search'].listeners.input();
  assert.equal(nodes['article-rows'].children.length,1);
  const title=nodes['article-rows'].children[0].children[0].children[0];
  assert.equal(title.textContent,raw.pages[0].title);
  assert.equal(title.tag,'span');assert.equal(title.children.length,0);
  nodes['article-search'].value='';nodes['article-group'].value='non-orphans';nodes['article-group'].listeners.input();
  assert(nodes['article-rows'].children.every(row=>row.children[2].textContent==='No'));
  console.log('Compact lists: creator pagination, missing-value sorting, groups and safe titles passed.');
}
async function checkCreatorBots() {
  const raw=fixture(),template=raw.pages[0];
  raw.pages=[
    {...template,page_id:1,orphan:true,creator_key:'actor:1',creator_is_bot_now:true},
    {...template,page_id:2,orphan:false,creator_key:'actor:1',creator_is_bot_now:true},
    {...template,page_id:3,orphan:true,creator_key:'actor:2',creator_is_bot_now:false},
    {...template,page_id:4,orphan:true,creator_key:null,creator_is_bot_now:true},
    {...template,page_id:5,orphan:false,creator_key:'actor:3',creator_is_bot_now:null},
  ];
  const {nodes,context}=await harness('creators',raw);
  const rows=nodes['creator-bots'].children;
  assert.equal(rows[0].children[2].textContent,'50% (1/2)');
  assert.equal(rows[1].children[2].textContent,'100% (1/1)');
  assert.equal(rows[2].children[2].textContent,'50% (1/2)');
  assert.match(nodes['creator-bots-note'].textContent,/Stato bot attuale/);
  assert(!nodes['creator-bots-note'].textContent.includes('bot alla creazione'));
  raw.pages=[{...template,creator_key:null,creator_is_bot_now:null,orphan:true}];
  vm.runInContext('creatorCharts()',context);
  assert.equal(nodes['creator-bots'].children[0].children[2].textContent,'— (0/0)');
  assert.equal(nodes['creator-bots'].children[2].children[2].textContent,'100% (1/1)');
  nodes['ui-language-toggle'].listeners.click();
  assert.match(nodes['creator-bots-note'].textContent,/Current bot status/);
  assert.equal(nodes['creator-bots'].children[0].children[0].textContent,'Bot (current status)');
  console.log('Creator bot chart: current membership, attribution, missing data, empty groups and translations passed.');
}
async function checkCandidateSorting() {
  const raw=fixture();
  const template=raw.pages.find(page=>page.orphan);
  raw.pages=['Zero','Beta','Alfa','Unknown','Absent','Highest'].map((title,i)=>({...template,page_id:i+1,title}));
  raw.link_candidates={
    1:{candidate_count:0},2:{candidate_count:2},3:{candidate_count:2},
    4:{candidate_count:null},6:{candidate_count:5}
  };
  const {nodes,html}=await harness('deorphanize',raw);
  for(const order of ['candidates-desc','candidates-asc'])assert(html.includes(`value="${order}"`));
  const titles=()=>nodes['candidate-cards'].children.map(card=>titleLink(card).textContent);
  assert.deepEqual(titles(),['Highest','Alfa','Beta','Zero','Absent','Unknown']);
  for(const [order,expected] of [
    ['candidates-asc',['Zero','Alfa','Beta','Highest','Absent','Unknown']],
    ['candidates-desc',['Highest','Alfa','Beta','Zero','Absent','Unknown']]
  ]) {
    nodes['candidate-sort'].value=order;nodes['candidate-sort'].listeners.input();
    assert.deepEqual(titles(),expected);
  }
  console.log('Candidate sorting: ascending/descending counts, ties, zero and missing data passed.');
}
async function checkConnections() {
  const raw=fixture();
  raw.metadata.demo=false;
  raw.pages=[{page_id:1,title:'<img src=x onerror=alert(1)>',orphan:true,category:'Tema'}];
  raw.link_candidates={'1':{status:'compared',candidate_count:2,candidates:[
    {page_id:3,title:'Z normale',dead_end:false,evidence:[{language:'lmo',source_title:'Fonte Z',target_title:'Bersaglio'}]},
    {page_id:2,title:'A & B',dead_end:true,evidence:[{language:'lmo',source_title:'Fonte',target_title:'Bersaglio'},{language:'it',source_title:'Fonte italiana',target_title:'Destinazione italiana'}]}
  ]}};
  const result=await harness('connections',raw,'?dataset=fixture.json&page=1'),{nodes,context}=result;
  assert.equal(nodes['connection-title'].textContent,raw.pages[0].title);
  assert.equal(nodes['connection-candidates'].children.length,2);
  const card=nodes['connection-candidates'].children[0];
  assert.equal(titleLink(card).textContent,'A & B');
  assert.equal(titleLink(card).href,'https://vec.wikipedia.org/wiki/A_%26_B');
  assert.equal(findClass(card,'dead-end-badge').textContent,'Dead-end page');
  const proposed=findClass(card,'connection-proposed'),evidence=descendants(card).filter(n=>n.className==='connection-evidence');
  assert.equal(proposed.children[0].textContent,'Collegamento da valutare in VEC');
  assert.equal(proposed.children[1].children[0].href,'https://vec.wikipedia.org/wiki/A_%26_B');
  assert.equal(evidence.length,2);
  assert.equal(evidence[0].children[0].textContent,'Collegamento esistente in LMO');
  assert.equal(evidence[0].children[1].children[0].href,'https://lmo.wikipedia.org/wiki/Fonte');
  assert.equal(evidence[0].children[1].children[2].href,'https://lmo.wikipedia.org/wiki/Bersaglio');
  assert.equal(findClass(card,'candidate-footer').children[0].textContent,'Fix this');
  const prompt=await preparePrompt(result,findClass(card,'fix-link'));
  assert(prompt.includes(`LOCAL_Y = ${JSON.stringify(raw.pages[0].title)}`));
  assert(prompt.includes('local census dead_end=true; Dead-end page'));
  assert.match(prompt,/Evaluate only this single proposed link X → Y/);
  assert(!prompt.includes('Z normale'));assert(!prompt.includes('Fonte Z'));
  assert(prompt.includes('https://lmo.wikipedia.org/wiki/Fonte → REFERENCE_Y'));
  assert(prompt.includes('https://it.wikipedia.org/wiki/Fonte_italiana → REFERENCE_Y'));
  assert.match(prompt,/at most one edit \(one table row\)/);
  assert(!prompt.includes('up to 5'));
  assertAccessInstructions(prompt);
  const otherPrompt=await preparePrompt(result,findClass(nodes['connection-candidates'].children[1],'fix-link'));
  assert(otherPrompt.includes('Z normale'));assert(!otherPrompt.includes('A & B'));
  assert(!otherPrompt.includes('Fonte_italiana'));
  assert(!findClass(nodes['connection-candidates'].children[1],'dead-end-badge'));
  raw.metadata.demo=true;vm.runInContext('renderConnections()',context);
  assert(!descendants(nodes['connection-candidates']).some(n=>n.tag==='a')); // synthetic links never open Wikipedia
  raw.link_candidates['1']={status:'compared',candidate_count:0,candidates:[]};vm.runInContext('renderConnections()',context);
  assert.match(nodes['connection-info'].textContent,/Nessuna pagina candidata/);
  raw.link_candidates['1']={status:'missing_item',candidate_count:null,candidates:[]};vm.runInContext('renderConnections()',context);
  assert.match(nodes['connection-info'].textContent,/non disponibile/);
  raw.link_candidates['1']={candidate_count:60,candidates:Array.from({length:60},(_,i)=>({page_id:i+2,title:`Fonte ${i}`,evidence:[]}))};vm.runInContext('renderConnections()',context);
  assert.equal(nodes['connection-candidates'].children.length,50);
  nodes['connection-more'].listeners.click();assert.equal(nodes['connection-candidates'].children.length,60);
  context.window.location.search='?dataset=fixture.json&page=999';
  assert.throws(()=>vm.runInContext('renderConnections()',context),/non presente/);
  const invalid=await harness('connections',raw,'?dataset=..%2Fsecret.json&page=1');
  assert.match(invalid.nodes.status.textContent,/Raccolta richiesta non disponibile/);
  console.log('Connections page: proposed/existing links, dead-end priority, pagination, safety and invalid routes passed.');
}
function assertAnchorInstructions(prompt) {
  assert.match(prompt,/Draft proposed incoming links X → Y as text only/);
  assert.match(prompt,/No execution, authentication or live verification is requested/);
  assert.match(prompt,/existing unlinked local mention identifying Y: use ADD LINK and keep its wording/);
  assert.match(prompt,/A film title is not a mention of an actor/);
  assert.match(prompt,/If no such mention exists, separately assess minimal NEW CONTENT supported by the references/);
  assert.match(prompt,/an attested relationship is not an invented fact merely because it is absent locally/);
  assert.match(prompt,/Do not create new sections or subsections/);
  assert.match(prompt,/New content must fit an existing local passage, table, list or template invocation; if it requires a new section, skip/);
  assert.match(prompt,/Entries in existing lists and missing template fields are allowed when the specific addition improves X/);
  assert.match(prompt,/Their presence in another language alone is insufficient/);
  assert.match(prompt,/evidence of both the value and the supported local parameter\/syntax in the supplied documents/);
  assert.match(prompt,/never invent parameters or copy foreign-language parameter names/);
  assert.match(prompt,/Edit only the invocation in X, not shared template definitions/);
  assert.match(prompt,/Missing local template support means skip that field proposal/);
  assert.match(prompt,/Judge usefulness in the local context, not just whether an insertion point exists/);
  assert.match(prompt,/No content or See also solely to remove orphan status/);
  assert.match(prompt,/“with” does not establish a leading role/);
  assert.match(prompt,/Interpret editorial comments within their actual section\/field scope/);
  const ending=prompt.split('\n\nEND OF SUPPLIED DOCUMENTS — RESPONSE INSTRUCTIONS\n')[1];
  assert(ending);assert.match(ending,/Start directly with a Markdown table with exactly these four columns/);
  assert.match(ending,/use clickable Markdown links to the supplied local\/reference URLs/);
  assert.match(ending,/unique verbatim single-line Ctrl\+F string within the local passage anchoring the insertion or replacement/);
  assert.match(ending,/For ADD LINK it must be the actual existing unlinked mention of Y/);
  assert.match(ending,/only the shortest complete name or term to link, excluding surrounding articles, verbs, punctuation and sentence context unless integral to the name/);
  assert.match(ending,/Preserve its exact spelling and capitalization; use a piped link label if the destination title differs/);
  assert.match(ending,/ADD LINK rows have no replacement text, rewritten sentence or associated code block/);
  assert.match(ending,/Every non-ADD LINK block must add \[\[.+\]\] and change the supplied local text/);
  assert.match(ending,/an unchanged block or one without the target link is invalid/);
  assert.match(ending,/For REPLACE identify the scope and supply the complete new block, not the old one/);
  assert.match(ending,/preserve the whole affected row and its columns\/roles/);
  assert.match(ending,/Silently check anchors, target links and agreement between rows, blocks and reasons/);
  assert.match(ending,/correct or omit contradictions/);
  assert.match(ending,/A skipped candidate has no table row/);
  assert.match(ending,/Never add section\/subsection headings; skip additions requiring a new section/);
  assert.match(ending,/Missing template fields remain eligible only when locally justified and supported as above/);
  assert.match(ending,/Output only the table, necessary wikitext blocks and optional reasons/);
  assert.match(ending,/at most one short sentence per candidate \(25 words\)/);
  assert.match(ending,/Keep document IDs, revisions, checksums and verification analysis internal/);
  assert.match(ending,/No preamble, headings, capability disclaimers/);
  assert.match(ending,/Zero rows are valid: headers only, no dummy rows/);
  assert(!prompt.includes('New sections, list entries and missing template fields are allowed'));
  assert(!prompt.includes('New sections/fields remain eligible'));
}
function assertAccessInstructions(prompt) {
  assert.match(prompt,/complete Wikipedia main-slot wikitext/);
  assert.match(prompt,/do not browse, search, call APIs or open external citations/);
  assert.match(prompt,/A mention\/link in REFERENCE_X is not a mention\/link in LOCAL_X/);
  assert.match(prompt,/Missing documents, unprovided template expansions and unresolved redirects remain unknown/);
  assert.match(prompt,/Claim absence only from complete relevant source text/);
  assert.match(prompt,/An access failure is not an editorial rejection/);
  assert.match(prompt,/Census interval \(not source retrieval dates\)/);
  assert.match(prompt,/timestamp is its revision date; retrieved_at is its acquisition date/);
  assert(!prompt.includes('source view/API fallback'));
}
async function checkFixPrompts() {
  const raw=fixture();raw.metadata.demo=false;
  const page={page_id:1,title:'Y à & # <img>',orphan:true,category:'Tema',creator_key:'private-actor'};
  raw.pages=[page];
  const candidates=Array.from({length:40},(_,i)=>({page_id:i+2,title:`Fonte ${String(i).padStart(2,'0')}`,dead_end:i>=35,evidence:[{language:'lmo',source_title:`Reference ${i}`,target_title:'Target B'}]}));
  raw.link_candidates={1:{status:'compared',candidate_count:40,candidates}};
  const original=JSON.stringify(raw),result=await harness('deorphanize',raw),{nodes,context}=result;
  const card=()=>nodes['candidate-cards'].children[0],fix=()=>findClass(card(),'fix-link');
  assert.equal(titleLink(card()).href,'https://vec.wikipedia.org/wiki/Y_%C3%A0_%26_%23_%3Cimg%3E');
  assert.equal(fix().tag,'button');assert.equal(fix().type,'button');assert(!fix().href);
  assert.equal(result.requests.filter(row=>row.url.startsWith('https://')).length,0);
  let prompt=await preparePrompt(result,fix());
  assertAnchorInstructions(prompt);assertAccessInstructions(prompt);
  assert.match(prompt,/Reply in Italian/);assert.match(prompt,/Evaluate only these 5 candidates \(selected from 40\)/);
  assert.match(prompt,/Selection is dead-end then alphabetical, not editorial relevance/);
  assert.match(prompt,/suitable Dead-end pages first/);
  assert.match(prompt,/Link pagina lingua da cambiare \| Link pagina lingua di provenienza/);
  assert(!prompt.includes('private-actor'));assert(!prompt.includes('Fonte 00'));
  assert.equal((prompt.match(/^\d+\. LOCAL_X/gm)||[]).length,5);
  assert.equal(sourceMetadata(prompt).length,12); // Y, five local X, five reference X, shared reference Y.
  assert(sourceMetadata(prompt).every(row=>row.status==='READABLE'));
  for(let i=35;i<40;i++)assert(prompt.includes(`Fonte ${i}`)&&prompt.includes(`Reference_${i}`));
  const requests=result.requests.filter(row=>row.url.startsWith('https://'));
  assert.equal(requests.length,12);assert.equal(result.copied.length,0);
  for(const row of requests) {
    const url=new URL(row.url);
    assert.equal(url.pathname,'/w/api.php');assert.equal(url.searchParams.get('action'),'query');
    assert.equal(url.searchParams.get('prop'),'revisions');assert.equal(url.searchParams.get('origin'),'*');
    assert.equal(url.searchParams.get('rvslots'),'main');assert.equal(row.credentials,'omit');assert.equal(row.cache,'no-store');
  }
  assert.equal(requests.filter(row=>new URL(row.url).searchParams.get('titles')==='Target B').length,1);
  assert.match(nodes['llm-prompt-status'].textContent,/12\/12/);
  assert.equal(nodes['llm-prompt-copy'].disabled,false);
  await nodes['llm-prompt-copy'].listeners.click();assert.equal(result.copied[0],prompt);
  assert.equal(JSON.stringify(raw),original);
  const fetches=result.getFetchCount();
  for(const [provider,url] of [['kimi','https://www.kimi.ai/'],['mistral','https://chat.mistral.ai/chat'],['glm','https://chat.z.ai/'],['qwen','https://chat.qwen.ai/']]) {
    nodes['llm-provider'].value=provider;nodes['llm-provider'].listeners.change();
    assert.equal(result.getFetchCount(),fetches);
    assert.equal(nodes['llm-prompt'].open,false);assert.equal(nodes['llm-prompt-text'].value,'');
    assert(fix().title.includes(({qwen:'Qwen',kimi:'Kimi',mistral:'Mistral',glm:'GLM'})[provider]));
    assert.equal(new URL(url).search,'');
  }
  nodes['ui-language-toggle'].listeners.click();assert.equal(result.getFetchCount(),fetches);
  prompt=await preparePrompt(result,fix());assert.match(prompt,/Reply in English/);
  assert.match(prompt,/Page link \(language to edit\) \| Page link \(reference language\)/);
  assert.equal(nodes['llm-prompt-open'].href,'https://chat.qwen.ai/');
  nodes['ui-language-toggle'].listeners.click();
  raw.link_candidates[1]={candidate_count:2,candidates:[candidates[0],candidates[35],candidates[35],{page_id:1,title:page.title},{page_id:0,title:'Invalid'},{page_id:99,title:null}]};
  vm.runInContext('candidateCards()',context);
  prompt=await preparePrompt(result,fix());
  const entries=prompt.split('Candidate source pages X:\n')[1].split('\n').filter(line=>/^\d+\. LOCAL_X/.test(line));
  assert.equal(entries.length,2);assert.match(entries[0],/Dead-end page/);assert.match(entries[1],/dead_end=false/);
  const unknown={page_id:90,title:'Unknown à & #',evidence:[{language:'lmo',source_title:'Reference unknown',source_page_id:501,target_title:'Target B',target_page_id:601}]};
  raw.link_candidates[1]={candidate_count:1,candidates:[unknown]};vm.runInContext('candidateCards()',context);
  prompt=await preparePrompt(result,fix());assert.match(prompt,/dead_end=unknown/);
  assert(sourceMetadata(prompt).some(row=>row.language==='lmo'&&row.page_id===501));
  assert(sourceMetadata(prompt).some(row=>row.language==='lmo'&&row.page_id===601));
  unknown.evidence[0].source_page_id='501&action=submit';unknown.evidence[0].target_page_id=-1;
  const before=result.requests.length;
  vm.runInContext('candidateCards()',context);await preparePrompt(result,fix());
  assert(result.requests.slice(before).some(row=>new URL(row.url).searchParams.get('titles')==='Reference unknown'));
  assert(result.requests.slice(before).every(row=>new URL(row.url).searchParams.get('action')==='query'));
  for(const record of [undefined,{candidate_count:null,candidates},{candidate_count:0,candidates:[]},{candidate_count:1,candidates:[{page_id:2,title:'No evidence',evidence:[{language:'lmo.evil',source_title:'X',target_title:'Y'}]}]}]) {
    raw.link_candidates[1]=record;vm.runInContext('candidateCards()',context);assert.equal(fix().tag,'span');assert.equal(fix()['aria-disabled'],'true');
  }
  raw.link_candidates[1]={candidate_count:40,candidates};raw.metadata.demo=true;vm.runInContext('candidateCards()',context);
  assert.equal(fix().textContent,'Fix this · DEMO');assert(!fix().listeners.click);assert(!titleLink(card()).href);
  console.log('Fix this: click-only revisions, twelve deduplicated complete sources, scope/role instructions, explicit clipboard, safe provider URLs, dead-end ranking and unchanged data passed.');
}
async function checkSingleConnectionPrompts() {
  const raw=fixture();raw.metadata.demo=false;raw.metadata.language='roa-tara';
  raw.pages=[{page_id:1,title:'Orphan à & #',orphan:true,category:'Tema'}];
  const chosen={page_id:90,title:'Chosen à & #',dead_end:false,evidence:['lmo','it','fur'].map((language,i)=>({language,source_title:`Ref ${language}`,source_page_id:100+i,target_title:`Target ${language}`,target_page_id:200+i}))};
  const others=Array.from({length:6},(_,i)=>({page_id:i+2,title:`Other ${i}`,dead_end:true,evidence:[{language:'lmo',source_title:`Unrelated ${i}`,target_title:'Other target'}]}));
  raw.link_candidates={1:{candidate_count:7,candidates:[...others,chosen]}};
  const before=JSON.stringify(raw),result=await harness('connections',raw,'?dataset=fixture.json&page=1'),{nodes,context}=result;
  const card=()=>nodes['connection-candidates'].children.find(row=>titleLink(row).textContent===chosen.title),fix=()=>findClass(card(),'fix-link');
  for(const [provider,url] of [['qwen','https://chat.qwen.ai/'],['kimi','https://www.kimi.ai/'],['mistral','https://chat.mistral.ai/chat'],['glm','https://chat.z.ai/']]) {
    const fetches=result.getFetchCount();nodes['llm-provider'].value=provider;nodes['llm-provider'].listeners.change();
    assert.equal(result.getFetchCount(),fetches);
    const prompt=await preparePrompt(result,fix());
    assert.equal(nodes['llm-prompt-open'].href,url);assert.equal(new URL(url).search,'');
    assert.match(prompt,/Evaluate only this single proposed link X → Y/);
    assert.match(prompt,/at most one edit \(one table row\)/);assert.match(prompt,/include every supplied reference source URL/);
    assertAnchorInstructions(prompt);assertAccessInstructions(prompt);
    assert(!prompt.includes('up to 5'));assert(!prompt.includes('Other_'));assert(!prompt.includes('Unrelated_'));
    assert.equal((prompt.match(/^\d+\. LOCAL_X/gm)||[]).length,1);
    assert.equal((prompt.match(/Observed reference edge/g)||[]).length,3);
    assert.equal(sourceMetadata(prompt).length,8);assert(sourceMetadata(prompt).every(row=>row.status==='READABLE'));
  }
  assert.equal(JSON.stringify(raw),before);assert.equal(result.copied.length,0);
  chosen.evidence.push(...['nap','sc','en','fr'].map((language,i)=>({language,source_title:`Ref ${language}`,source_page_id:300+i,target_title:`Target ${language}`,target_page_id:400+i})));
  vm.runInContext('renderConnections()',context);
  const prompt=await preparePrompt(result,fix());
  assert.equal((prompt.match(/Observed reference edge/g)||[]).length,7);assert.equal(sourceMetadata(prompt).length,16);
  assert(!prompt.includes('Other_'));assert(!prompt.includes('up to 5'));
  for(const evidence of [[],[{language:'lmo.evil',source_title:'Bad',target_title:'Bad'}]]) {
    chosen.evidence=evidence;vm.runInContext('renderConnections()',context);assert.equal(fix().tag,'span');assert.equal(fix()['aria-disabled'],'true');
    assert(nodes['connection-candidates'].children.some(row=>findClass(row,'fix-link').tag==='button'));
  }
  console.log('Single connection: isolated candidate beyond top five, all seven reference pairs without truncation, four plain chat URLs and unchanged data passed.');
}
async function checkLlmSelection() {
  const raw=fixture();raw.metadata.demo=false;
  raw.pages=Array.from({length:30},(_,i)=>({page_id:i+1,title:`Target ${i}`,orphan:true,category:'Tema'}));
  const record={candidate_count:1,candidates:[{page_id:99,title:'Local source',dead_end:true,evidence:[{language:'lmo',source_title:'Reference source',target_title:'Reference target'}]}]};
  raw.link_candidates=Object.fromEntries(raw.pages.map(page=>[page.page_id,record]));
  const result=await harness('deorphanize',raw),{nodes,storage}=result;
  assert.deepEqual(nodes['llm-provider'].children.map(option=>option.textContent),['Qwen','Kimi','Mistral','GLM']);
  nodes['candidate-search'].value='Target';nodes['candidate-search'].listeners.input();nodes['candidate-more'].listeners.click();
  assert.equal(nodes['candidate-cards'].children.length,30);
  const fetches=result.getFetchCount();
  for(const provider of ['kimi','mistral','glm']) {
    nodes['llm-provider'].value=provider;nodes['llm-provider'].listeners.change();
    assert.equal(nodes['candidate-cards'].children.length,30);assert.equal(nodes['candidate-search'].value,'Target');
    assert.equal(storage.get('orphanwiki-llm'),provider);assert.equal(result.getFetchCount(),fetches);assert.equal(result.copied.length,0);
  }
  let next=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{storage});
  assert.equal(next.nodes['llm-provider'].value,'glm');
  await preparePrompt(next,findClass(next.nodes['connection-candidates'].children[0],'fix-link'));
  assert.equal(next.nodes['llm-prompt-open'].href,'https://chat.z.ai/');
  for(const preferences of [{clipboardUnavailable:true},{clipboardFailure:true},{blockStorage:true}]) {
    next=await harness('deorphanize',raw,undefined,null,preferences);
    next.nodes['llm-provider'].value='mistral';next.nodes['llm-provider'].listeners.change();
    await preparePrompt(next,findClass(next.nodes['candidate-cards'].children[0],'fix-link'));
    assert.equal(next.copied.length,0);
    await next.nodes['llm-prompt-copy'].listeners.click();
    assert.equal(next.nodes['llm-prompt'].open,true);
    if(!preferences.blockStorage)assert.match(next.nodes['llm-prompt-status'].textContent,/copialo manualmente/);
  }
  for(const stored of ['javascript:evil','deepseek']) {
    next=await harness('deorphanize',raw,undefined,null,{storage:new Map([['orphanwiki-llm',stored]])});
    assert.equal(next.nodes['llm-provider'].value,'qwen');
    assert.equal(next.requests.filter(request=>request.url.startsWith('https://')).length,0);
  }
  next.nodes['llm-provider'].value='__proto__';next.nodes['llm-provider'].listeners.change();assert.equal(next.nodes['llm-provider'].value,'qwen');
  next.nodes['llm-provider'].value='mistral';next.nodes['llm-provider'].listeners.change();next.nodes['ui-language-toggle'].listeners.click();
  const prompt=await preparePrompt(next,findClass(next.nodes['candidate-cards'].children[0],'fix-link'));
  assert.match(prompt,/Reply in English/);assert.equal(next.nodes['llm-prompt-title'].textContent,'Prompt for Mistral');
  await next.nodes['llm-prompt-copy'].listeners.click();assert.match(next.nodes['llm-prompt-status'].textContent,/Prompt and sources copied/);
  await next.nodes.dataset.listeners.change();
  assert.equal(next.nodes['llm-prompt'].open,false);assert.equal(next.nodes['llm-prompt-text'].value,'');
  console.log('Provider selection: preferences and pagination retained, no background sources/clipboard, explicit manual copy fallback, translations and clearing on dataset change passed.');
}
async function checkWikitextPreparation() {
  const raw=fixture();raw.metadata.demo=false;
  raw.pages=[{page_id:1,title:'Michael Douglas',orphan:true}];
  raw.link_candidates={1:{candidate_count:1,candidates:[{page_id:2,title:'Michael J. Fox',dead_end:false,evidence:[{language:'co',source_page_id:2,source_title:'Michael J. Fox',target_page_id:1,target_title:'Michael Douglas'}]}]}};
  const full='{{Infobox}}\n{| class="wikitable"\n| 1995\n| '+"''[[The American President]]''"+'\n| Lewis Rothschild\n|\n|}\n'+('Testo ł à.\n'.repeat(4000))+'\n````\nFINE INTEGRALE';
  const sources={
    'vec:1':revisionContent(1,'Michael Douglas','Michael Douglas, ator.'),
    'vec:2':revisionContent(2,'Michael J. Fox',full),
    'co:1':revisionContent(1,'Michael Douglas','Michael Douglas, attore.'),
    'co:2':revisionContent(2,'Michael J. Fox',"* ''[[The American President]]'' (1995), cù [[Michael Douglas]]")
  };
  const key=url=>url.hostname.split('.')[0]+':'+url.searchParams.get('pageids');
  const control=result=>findClass(result.nodes['connection-candidates'].children[0],'fix-link');
  const result=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:url=>sources[key(url)]});
  const before=JSON.stringify(raw),prompt=await preparePrompt(result,control(result));
  assert.equal(result.nodes['llm-prompt-debug'].checked,false);
  assert(!prompt.includes('OPTIONAL TEST APPENDIX — DEBUG'));
  assert(prompt.includes(full));assert(prompt.includes('FINE INTEGRALE'));assert(prompt.includes('`````wikitext\n'+full+'\n`````'));
  assert(prompt.indexOf('END OF SUPPLIED DOCUMENTS — RESPONSE INSTRUCTIONS')>prompt.lastIndexOf('DOCUMENT D4\n'));
  assert(prompt.indexOf('END OF SUPPLIED DOCUMENTS — RESPONSE INSTRUCTIONS')>prompt.indexOf('FINE INTEGRALE'));
  assert.equal(sourceMetadata(prompt).length,4);assert(sourceMetadata(prompt).every(row=>row.status==='READABLE'));
  for(const metadata of sourceMetadata(prompt)) {
    const expected=sources[metadata.language+':'+metadata.page_id].query.pages[0].revisions[0];
    assert.equal(metadata.sha1,expected.sha1);assert.equal(metadata.utf8_bytes,expected.size);assert.equal(metadata.revision_id,expected.revid);
    assert(metadata.revision_url.endsWith('oldid='+expected.revid));
  }
  assert.equal(JSON.stringify(raw),before);
  const fetches=result.getFetchCount();
  result.nodes['llm-prompt-debug'].checked=true;result.nodes['llm-prompt-debug'].listeners.change();
  const debug=result.nodes['llm-prompt-text'].value;
  assert(debug.startsWith(prompt+'\n\nOPTIONAL TEST APPENDIX — DEBUG\n'));
  assert.match(debug,/Then append a section titled DEBUG/);
  assert.match(debug,/Do not change the normal proposals to satisfy this appendix/);
  assert.match(debug,/observable evidence checks, not private reasoning or deliberation/);
  assert.match(debug,/For ADD LINK, quote only the exact name\/term from column 3 and its intended wikilink, preserving the original spelling\/capitalization/);
  assert.match(debug,/report FAIL if column 3 includes surrounding sentence context or the normal response adds replacement text\/code for this row/);
  assert.match(debug,/For BEFORE\/AFTER\/REPLACE, quote the exact local Ctrl\+F anchor and the exact added line containing the target link/);
  assert.match(debug,/report FAIL without inventing it or rewriting the normal response/);
  assert.match(debug,/existing local passage, table, list or template invocation hosting the addition/);
  assert.match(debug,/report FAIL if the proposal creates a section or subsection/);
  assert(!debug.includes('For a new section/field'));
  assert.match(debug,/cite supplied evidence of the local parameter support/);
  assert.equal(result.getFetchCount(),fetches);assert.equal(result.copied.length,0);
  await result.nodes['llm-prompt-copy'].listeners.click();assert.equal(result.copied[0],debug);
  result.nodes['llm-prompt-debug'].checked=false;result.nodes['llm-prompt-debug'].listeners.change();
  assert.equal(result.nodes['llm-prompt-text'].value,prompt);assert.equal(result.getFetchCount(),fetches);
  result.nodes['llm-prompt-close'].listeners.click();assert.equal(result.nodes['llm-prompt-text'].value,'');
  assert.equal(result.nodes['llm-prompt-debug'].checked,false);
  assert(![...result.storage.values()].some(value=>value.includes('FINE INTEGRALE')));
  const mutations=[
    payload=>payload.query.pages[0].pageid=999,
    payload=>payload.query.pages[0].title='Wrong page',
    payload=>payload.query.pages[0].revisions[0].sha1='0'.repeat(40),
    payload=>payload.query.pages[0].revisions[0].size++,
    payload=>payload.query.pages[0].revisions[0].slots.main.content='Truncated',
    payload=>payload.query.pages[0].revisions[0].slots.main.contentmodel='json',
    payload=>payload.query.pages[0].revisions[0].slots.main.texthidden=true,
    payload=>payload.query.pages[0].revisions[0].timestamp='invalid',
    payload=>payload.query.pages[0].revisions=[],
    payload=>payload.continue={rvcontinue:'unexpected'}
  ];
  for(const mutate of mutations) {
    const broken=JSON.parse(JSON.stringify(sources['vec:2']));mutate(broken);
    const test=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:url=>key(url)==='vec:2'?broken:sources[key(url)]});
    const partial=await preparePrompt(test,control(test)),metadata=sourceMetadata(partial);
    assert.equal(metadata.filter(row=>row.status==='READABLE').length,3);
    assert.equal(metadata.find(row=>row.language==='vec'&&row.title==='Michael J. Fox').status,'UNREADABLE');
    assert(!partial.includes(full));assert.match(test.nodes['llm-prompt-status'].textContent,/3\/4/);
  }
  const empty=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:url=>key(url)==='vec:2'?revisionContent(2,'Michael J. Fox',''):sources[key(url)]});
  const emptyPrompt=await preparePrompt(empty,control(empty));
  assert.equal(sourceMetadata(emptyPrompt).find(row=>row.language==='vec'&&row.page_id===2).utf8_bytes,0);
  const failed=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:()=>({error:{code:'blocked'}})});
  await preparePrompt(failed,control(failed));assert.equal(failed.nodes['llm-prompt-copy'].disabled,true);
  assert.match(failed.nodes['llm-prompt-status'].textContent,/Nessuna fonte disponibile/);
  const gates=[];
  const pending=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:url=>new Promise(resolve=>gates.push(()=>resolve(sources[key(url)])))});
  const work=control(pending).listeners.click();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(gates.length,3);assert.equal(pending.nodes['llm-prompt-copy'].disabled,true);
  pending.nodes['llm-prompt-debug'].checked=true;pending.nodes['llm-prompt-debug'].listeners.change();
  assert.equal(pending.nodes['llm-prompt-text'].value,'');
  pending.nodes['llm-prompt-close'].listeners.click();
  assert(pending.requests.filter(row=>row.url.startsWith('https://')).every(row=>row.signal.aborted));
  gates.forEach(resolve=>resolve());await work;
  assert.equal(pending.nodes['llm-prompt-text'].value,'');assert.equal(pending.nodes['llm-prompt'].open,false);
  assert.equal(pending.requests.filter(row=>row.url.startsWith('https://')).length,3);
  // A superseded preparation must not overwrite a newer source package.
  let release,hold=true;
  const racing=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:url=>hold&&key(url)==='vec:2'?
    new Promise(resolve=>{release=()=>resolve(sources[key(url)]);}):sources[key(url)]});
  const old=control(racing).listeners.click();
  await new Promise(resolve=>setImmediate(resolve));assert.equal(typeof release,'function');hold=false;
  const latest=await preparePrompt(racing,control(racing));release();await old;
  assert.equal(racing.nodes['llm-prompt-text'].value,latest);
  racing.nodes['llm-prompt'].listeners.cancel({preventDefault(){}});
  assert.equal(racing.nodes['llm-prompt-text'].value,'');assert.equal(racing.nodes['llm-prompt'].open,false);
  // Missing and known IDs for the same reference are one document; different wikis stay separate.
  raw.link_candidates[1].candidates[0].evidence.push({language:'co',source_title:'Michael J. Fox',target_title:'Michael Douglas'});
  const deduplicated=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:url=>sources[key(url)]});
  const deduplicatedPrompt=await preparePrompt(deduplicated,control(deduplicated));
  assert.equal(sourceMetadata(deduplicatedPrompt).length,4);
  assert.equal(deduplicated.requests.filter(row=>row.url.startsWith('https://')).length,4);
  console.log('Wikitext: complete Unicode beyond 32 KB, identity/checksum checks, cross-wiki isolation, opt-in DEBUG without changes to base prompt or requests, partial failures and cancellation passed.');
}
async function checkPreferences() {
  const storage=new Map([['orphanwiki-language','en'],['orphanwiki-theme','dark']]);
  for(const page of ['articles','creators','deorphanize','connections']) {
    const raw=fixture();
    if(page==='connections') {
      raw.pages=[{page_id:1,title:'Titolo originale',category:'Categoria originale',orphan:true}];
      raw.link_candidates={'1':{candidate_count:1,candidates:[{page_id:2,title:'Fonte originale',dead_end:true,evidence:[{language:'lmo',source_title:'Original source',target_title:'Original target'}]}]}};
    }
    const result=await harness(page,raw,page==='connections'?'?dataset=fixture.json&page=1':'?dataset=fixture.json',null,{storage});
    const {nodes,document,context,staticNodes,getFetchCount}=result;
    assert.equal(document.documentElement.lang,'en');
    assert.equal(document.documentElement.dataset.theme,'dark');
    assert.match(result.html, /<div class="header-row"><h1[\s\S]*?<\/h1><div class="preferences">/);
    assert.equal((result.html.match(/id="theme-toggle"/g)||[]).length,1);
    assert.equal((result.html.match(/id="ui-language-toggle"/g)||[]).length,1);
    assert(!result.html.includes('id="theme-light"'));
    assert(!result.html.includes('id="theme-dark"'));
    assert.equal(nodes['ui-language-toggle'].textContent,'EN');assert.equal(nodes['theme-dark-icon'].hidden,false);assert.equal(nodes['theme-light-icon'].hidden,true);
    assert.match(nodes['theme-toggle']['aria-label'],/Dark mode: switch to light mode/);
    assert.equal(nodes['articles-link'].textContent,'Article analysis');
    assert.equal(nodes['deorphanize-link'].textContent,'De-orphan articles');
    assert(!nodes.source);
    for(const node of staticNodes) {
      if(node.dataset.i18n && !['status','connection-title','age-note'].some(id=>nodes[id]===node)) {
        const key=JSON.stringify(node.dataset.i18n);
        assert.equal(node.textContent,vm.runInContext(`t(${key})`,context));
        assert(!/[{}]/.test(node.textContent));
      }
    }
    const fetches=getFetchCount();
    if(page==='articles') {
      assert.equal(nodes['article-search'].placeholder,'Article title');
      assert.equal(nodes['article-rows'].children[0].children[2].textContent,'Yes');
      const chart=nodes.age.children[0],heights=chart.children.filter(node=>node.tag==='rect').map(node=>node.height);
      assert.equal(chart.children[0].textContent,'% of articles in group');
      assert.equal(chart.children[0].fill,'var(--text)');
      assert(!descendants(nodes.age).some(node=>/Denominators: all/.test(node.textContent)));
      assert.equal(vm.runInContext('number(1234.5)',context),'1,234.5');
      nodes['article-group'].value='all';nodes['article-group'].listeners.input();
      nodes['article-more'].listeners.click();
      nodes['ui-language-toggle'].listeners.click();
      assert.equal(nodes['article-rows'].children.length,50);
      assert.equal(nodes['article-group'].value,'all');
      assert.deepEqual(nodes.age.children[0].children.filter(node=>node.tag==='rect').map(node=>node.height),heights);
      assert.equal(vm.runInContext('number(1234.5)',context),new Intl.NumberFormat('it-IT',{maximumFractionDigits:1}).format(1234.5));
    } else if(page==='creators') {
      assert.match(nodes['creator_prior_edits_main-note'].textContent,/Prior article edits.*Scanned languages: VEC/);
      assert.match(nodes['creator_prior_edits_main-chart'].children[0].children.at(-1).textContent,/edits \(logarithmic scale\)/);
      assert(nodes['creator-rows'].children.some(row=>row.children[1].textContent==='Registered'));
      assert(descendants(nodes.origins).some(node=>node.textContent==='Tagged translation'));
    } else if(page==='deorphanize') {
      const footer=findClass(nodes['candidate-cards'].children[0],'candidate-footer');
      assert.equal(footer.children[1].textContent,'Connect');
      assert.match(footer.children[1]['aria-label'],/^Connect Voce dimostrativa/);
      assert(nodes['candidate-cards'].children.every(card=>raw.pages.some(p=>p.title===titleLink(card).textContent)));
      nodes['candidate-more'].listeners.click();
      nodes['ui-language-toggle'].listeners.click();
      assert.equal(nodes['candidate-cards'].children.length,50);
    } else {
      assert.equal(nodes['connection-title'].textContent,'Titolo originale');
      const card=nodes['connection-candidates'].children[0];
      assert.equal(findClass(card,'connection-proposed').children[0].textContent,'Proposed link in VEC');
      assert.equal(findClass(card,'connection-evidence').children[0].textContent,'Existing link in LMO');
      assert.equal(titleLink(card).textContent,'Fonte originale');
    }
    assert.equal(getFetchCount(),fetches); // changing preferences never recollects or refetches data
    if(document.documentElement.lang!=='en')nodes['ui-language-toggle'].listeners.click();
    assert.equal(storage.get('orphanwiki-language'),'en');
  }
  let result=await harness('articles',fixture(),undefined,null,{storage,systemDark:true});
  result.nodes['theme-toggle'].listeners.click();
  assert.equal(result.document.documentElement.dataset.theme,'light');
  assert.equal(result.nodes['theme-light-icon'].hidden,false);
  assert.equal(result.nodes['theme-dark-icon'].hidden,true);
  assert.equal(storage.get('orphanwiki-theme'),'light');
  result.nodes['theme-toggle'].listeners.click();
  assert.equal(result.nodes['theme-light-icon'].hidden,true);
  assert.equal(result.nodes['theme-dark-icon'].hidden,false);
  assert.equal(storage.get('orphanwiki-theme'),'dark');
  result.nodes['theme-toggle'].listeners.click();
  result.media.matches=false;result.media.listener();
  assert.equal(result.document.documentElement.dataset.theme,'light');
  vm.runInContext("theme='system';applyTheme()",result.context);
  result.media.matches=true;result.media.listener();assert.equal(result.document.documentElement.dataset.theme,'dark');
  result.media.matches=false;result.media.listener();assert.equal(result.document.documentElement.dataset.theme,'light');
  result=await harness('articles',fixture(),undefined,null,{storage:new Map([['orphanwiki-language','bad'],['orphanwiki-theme','bad']]),systemDark:true});
  assert.equal(result.document.documentElement.lang,'it');assert.equal(vm.runInContext('theme',result.context),'system');
  assert.equal(result.document.documentElement.dataset.theme,'dark');
  result=await harness('articles',fixture(),undefined,null,{blockStorage:true});
  result.nodes['ui-language-toggle'].listeners.click();
  result.nodes['theme-toggle'].listeners.click();
  assert.equal(result.document.documentElement.lang,'en');assert.equal(result.document.documentElement.dataset.theme,'dark');
  result=await harness('articles',fixture(),undefined,[{language:'vec',file:'demo.json',demo:true,date:'2026-01-01'}],{storage});
  assert.match(result.nodes.status.textContent,/Unable to load: No real datasets available/);
  result.nodes['ui-language-toggle'].listeners.click();
  assert.match(result.nodes.status.textContent,/Impossibile caricare: Nessuna raccolta reale/);
  console.log('Preferences: both languages, all pages, numeric formatting, persistence, system theme, storage failure and unchanged data passed.');
}
async function checkAggregation() {
  const a=fixture(),b=fixture();
  a.metadata.demo=b.metadata.demo=false;
  b.metadata.language='lmo';b.metadata.creators.language='lmo';
  b.metadata.creators.languages_scanned=['lmo'];
  a.pages=a.pages.slice(0,2);b.pages=b.pages.slice(0,2);
  for(const wiki of [a,b])for(const [i,page] of wiki.pages.entries()) {
    page.title=`Shared title ${i+1}`;page.category='Shared category';
    page.creator_key='local:actor:7';page.creator_languages_created=[wiki.metadata.language];
    page.orphan=wiki===b||i===0;
  }
  a.pages[0].age_days=null;
  a.link_candidates={'1':{status:'compared',candidate_count:1,candidates:[{page_id:2,title:'Shared title 2',dead_end:true,evidence:[{language:'lmo',source_title:'Shared title 2',target_title:'Shared title 1'}]}]}};
  b.link_candidates={'1':{status:'missing_item',candidate_count:null,candidates:[]},'2':{status:'compared',candidate_count:0,candidates:[]}};
  const index={metadata:{kind:'aggregate',language:'all',languages:['lmo','vec'],complete:true,demo:false,started_at:a.metadata.started_at,finished_at:a.metadata.finished_at,age_rule:'mixed_per_language'},members:[{language:'vec',file:'vec.json',page_count:2,metadata:a.metadata},{language:'lmo',file:'lmo.json',page_count:2,metadata:b.metadata}]};
  const entries=[{language:'all',file:'all.json',kind:'aggregate',date:'2026-09-29'},{language:'vec',file:'vec.json',date:'2026-09-29'},{language:'lmo',file:'lmo.json',date:'2026-09-29'}];
  const files={'all.json':index,'vec.json':a,'lmo.json':b};
  for(const page of ['articles','creators','deorphanize','connections']) {
    const route='?dataset=all.json'+(page==='connections'?'&page=1&language=vec':'');
    const result=await harness(page,index,route,entries,{},files),{nodes,context}=result;
    assert(!nodes.status.textContent.includes('Impossibile caricare'),nodes.status.textContent);
    assert.equal(result.requests.filter(request=>request.url.startsWith('data/')).length,4);
    assert(result.requests.every(request=>request.cache==='no-store'));
    assert(!nodes.source);
    assert.equal(vm.runInContext('data.metadata.creators.collected_languages',context),2);
    assert.equal(vm.runInContext('data.pages.length',context),4);
    assert.equal(vm.runInContext('data.pages.filter(p=>p.orphan).length',context),3);
    assert.equal(vm.runInContext('new Set(data.pages.map(attributedCreatorKey)).size',context),2);
    assert.equal(vm.runInContext("label(data.pages[0])",context),'VEC · Shared category');
    if(page==='articles') {
      assert.match(nodes['age-note'].textContent,/page_creation_timestamp/);
      assert.equal(nodes['article-rows'].children.length,3);
      assert.match(nodes['article-rows'].children[0].children[0].children[0].href,/vec\.wikipedia/);
      assert(!nodes.download);
      assert.equal(nodes.dataset.children[0].textContent,'Tutte le lingue');
      assert.equal(nodes['dataset-date'].value,'2026-09-29');
    }
    if(page==='creators') {
      assert.equal(nodes['creator-rows'].children.length,2);
      const rows=nodes['creator-rows'].children;
      assert.deepEqual(rows.map(r=>r.children[4].textContent),['2','1']);
      assert.deepEqual(rows.map(r=>r.children[5].textContent),['100%','50%']);
    }
    if(page==='deorphanize') {
      const cards=nodes['candidate-cards'].children;
      assert.equal(cards.length,3);
      assert.match(findClass(cards[0],'connect-button').href,/language=vec/);
      assert.equal(findClass(cards[0],'candidate-badge').textContent,'1');
      assert.equal(findClass(cards[2],'candidate-badge').textContent,'—');
    }
    if(page==='connections') {
      assert.match(titleLink(nodes['connection-candidates'].children[0]).href,/vec\.wikipedia/);
      assert(descendants(nodes['connection-candidates']).some(n=>n.href?.includes('lmo.wikipedia')));
      const prompt=await preparePrompt(result,findClass(nodes['connection-candidates'].children[0],'fix-link'));
      assert(prompt.includes('https://vec.wikipedia.org/wiki/'));assert(!prompt.includes('https://all.wikipedia.org'));
    }
    nodes['ui-language-toggle'].listeners.click();
    if(nodes.dataset)assert.equal(nodes.dataset.children[0].textContent,'All languages');
    assert(!nodes.source);
  }
  let result=await harness('connections',index,'?dataset=all.json&page=1',entries,{},files);
  assert.match(result.nodes.status.textContent,/Pagina orfana non presente/);
  result=await harness('connections',index,'?dataset=all.json&page=1&language=en',entries,{},files);
  assert.match(result.nodes.status.textContent,/Pagina orfana non presente/);
  const bad=JSON.parse(JSON.stringify(index));bad.members[0].file='outside.json';
  result=await harness('articles',bad,'?dataset=all.json',entries,{}, {...files,'all.json':bad});
  assert.match(result.nodes.status.textContent,/Indice aggregato non valido/);
  const changed=JSON.parse(JSON.stringify(b));changed.metadata.input_sha256='stale';
  result=await harness('articles',index,'?dataset=all.json',entries,{}, {...files,'lmo.json':changed});
  assert.match(result.nodes.status.textContent,/Indice aggregato non valido/);
  const partial=JSON.parse(JSON.stringify(b));partial.metadata.creators={status:'not_collected'};
  for(const p of partial.pages) {p.creator_key=null;p.creator_prior_articles=null;}
  const partialIndex=JSON.parse(JSON.stringify(index));partialIndex.members[1].metadata=partial.metadata;
  result=await harness('creators',partialIndex,'?dataset=all.json',entries,{}, {...files,'all.json':partialIndex,'lmo.json':partial});
  assert.equal(vm.runInContext('data.metadata.creators.collected_languages',result.context),1);
  assert.equal(result.nodes['creator-rows'].children.length,1);
  assert.equal(result.nodes['creators-empty'].hidden,true);
  console.log('Aggregation: weighted page populations, local identities, all four pages, evidence, missing creators, translations and invalid/stale routes passed.');
}
async function checkCompactTransport() {
  const a=fixture(),b=fixture();a.metadata.demo=b.metadata.demo=false;
  b.metadata.language='lmo';b.metadata.creators.language='lmo';
  a.pages=a.pages.slice(0,3);b.pages=b.pages.slice(0,5);
  for(const raw of [a,b])for(const page of raw.pages){page.topic_status='predicted';page.topic_labels=['Culture.Media.Music'];}
  const pack=raw=>{const columns=[...new Set(raw.pages.flatMap(Object.keys))];return {format:'observations_v1',metadata:raw.metadata,columns,
    rows:raw.pages.map(page=>columns.map(key=>page[key]??null)),link_candidates:Object.fromEntries(Object.entries(raw.link_candidates).map(([key,record])=>[key,{status:record.status,candidate_count:record.candidate_count}]))};};
  const index={metadata:{kind:'aggregate',language:'all',languages:['vec','lmo'],complete:true,demo:false,started_at:a.metadata.started_at,finished_at:a.metadata.finished_at},members:[{language:'vec',file:'vec.json',page_count:3,metadata:a.metadata},{language:'lmo',file:'lmo.json',page_count:5,metadata:b.metadata}]};
  const entries=[{language:'all',file:'all.json',kind:'aggregate',date:a.metadata.finished_at},...['vec','lmo'].map(language=>({language,file:language+'.json',kind:'wiki',observations_file:language+'-observations.json',date:a.metadata.finished_at}))];
  const files={'all.json':index,'vec-observations.json':pack(a),'lmo-observations.json':pack(b),'vec.json':a,'lmo.json':b};
  const before=JSON.stringify(files);
  for(const page of ['articles','creators']) {
    const result=await harness(page,index,'?dataset=all.json',entries,{},files);
    assert.equal(result.document.body.dataset.loading,'false');
    assert(!result.nodes.status.textContent.includes('Impossibile'),result.nodes.status.textContent);
    assert.deepEqual(result.requests.map(r=>r.url),['data/manifest.json','data/all.json','data/vec-observations.json','data/lmo-observations.json']);
    assert.equal(vm.runInContext('data.pages.length',result.context),8);
    assert(!result.nodes.source);
    const full=await harness(page,index,'?dataset=all.json',entries.map(({observations_file,...entry})=>entry),{},files);
    for(const id of page==='articles'?['topics-full','topics-paper','features-paper','categories']:['origins','creator-bots']) {
      const values=nodes=>nodes[id].children.filter(node=>node.className==='bar-row').map(node=>[node.children[0].textContent,node.children[1].children[0].style.width,node.children[2].textContent]);
      assert.deepEqual(values(result.nodes),values(full.nodes));
    }
    assert.equal(result.nodes[page==='articles'?'article-info':'creator-info'].textContent,full.nodes[page==='articles'?'article-info':'creator-info'].textContent);
  }
  const deorphanize=await harness('deorphanize',a,'?dataset=vec.json',entries,{},files);
  assert.deepEqual(deorphanize.requests.map(r=>r.url),['data/manifest.json','data/vec.json','api/reviews?dataset=vec.json']);
  let result=await harness('articles',index,'?dataset=all.json',entries.map(e=>e.language==='vec'?{...e,observations_file:'../unsafe.json'}:e),{},files);
  assert.match(result.nodes.status.textContent,/Nome dataset non valido/);
  assert.equal(result.document.body.dataset.loading,'true'); // Prior charts never masquerade as the new selection.
  const bad=pack(a);bad.rows[0]=[];
  result=await harness('articles',index,'?dataset=all.json',entries,{}, {...files,'vec-observations.json':bad});
  assert.match(result.nodes.status.textContent,/Osservazioni compatte non valide/);
  const stale=pack(a);stale.metadata={...stale.metadata,input_sha256:'stale'};
  result=await harness('articles',index,'?dataset=all.json',entries,{}, {...files,'vec-observations.json':stale});
  assert.match(result.nodes.status.textContent,/Indice aggregato non valido/);
  assert.equal(JSON.stringify(files),before);
  console.log('Compact transport: identical full-census bars/counts, approved lightweight members, legacy fallback, unchanged evidence pages, stale/unsafe rejection and hidden loading state passed.');
}
async function checkTopicExplorer() {
  const raw=fixture(),template=raw.pages[0];
  raw.pages=Array.from({length:34},(_,i)=>({...template,page_id:i+1,title:`Article ${String(i).padStart(2,'0')}`,orphan:true,
    category:'Music category',topic_status:i===32?'not_collected':'predicted',topic_labels:i===33?[]:i%2?['STEM.Physics']:['Culture.Media.Music','Culture.Media.Films']}));
  const before=JSON.stringify(raw),{nodes,context}=await harness('articles',raw);
  assert.equal(nodes['article-rows'].children.length,25);
  nodes['article-more'].listeners.click();assert.equal(nodes['article-rows'].children.length,34);
  nodes['article-sort'].value='category-asc';nodes['article-sort'].listeners.input();
  nodes['article-classification'].value='macro';nodes['article-classification'].listeners.change();
  assert.equal(nodes['article-rows'].children.length,25);
  assert.equal(nodes['article-categories'].children.length,4);
  assert.equal(nodes['article-category-label'].textContent,'Macrotema');
  assert.equal(nodes['article-category-control'].hidden,true);
  assert.equal(nodes['article-macro-control'].hidden,false);
  assert.deepEqual(nodes['article-macro'].children.map(option=>option.value),['','Culture','Geography','History and Society','STEM']);
  nodes['article-macro'].value='STEM';nodes['article-macro'].listeners.change();
  assert.equal(nodes['article-rows'].children.length,16);
  assert(nodes['article-rows'].children.every(row=>row.children[1].textContent==='STEM'));
  nodes['ui-language-toggle'].listeners.click();
  assert.equal(nodes['article-macro'].value,'STEM');
  assert.equal(nodes['article-macro'].children[0].textContent,'All broad topics');
  assert.equal(nodes['article-rows'].children.length,16);
  nodes['ui-language-toggle'].listeners.click();
  nodes['article-classification'].value='topics';nodes['article-classification'].listeners.change();
  assert.equal(nodes['article-category'].value,'');
  assert.equal(nodes['article-category-control'].hidden,false);
  assert.equal(nodes['article-macro-control'].hidden,true);
  assert.equal(nodes['article-categories'].children.length,64);
  assert.equal(nodes['article-category-heading'].textContent,'Tema');
  nodes['article-category'].value='Media.Music';nodes['article-category'].listeners.input();
  assert.equal(nodes['article-rows'].children.length,16);
  assert(nodes['article-rows'].children.every(row=>row.children[1].textContent.includes('Culture · Media.Music')));
  nodes['article-search'].value='Article 00';nodes['article-search'].listeners.input();
  assert.equal(nodes['article-rows'].children.length,1);
  nodes['ui-language-toggle'].listeners.click();
  assert.equal(nodes['article-category-heading'].textContent,'Topic');
  assert.equal(nodes['article-rows'].children.length,1);
  assert.equal(nodes['article-rows'].children[0].children[1].textContent,'Culture · Media.Films · Culture · Media.Music');
  nodes['theme-toggle'].listeners.click();assert.equal(nodes['article-search'].value,'Article 00');
  assert.equal(vm.runInContext('articleLabels(data.pages[32])[0]',context),'Topics unavailable');
  assert.equal(vm.runInContext('articleLabels(data.pages[33])[0]',context),'No topic above threshold');
  nodes['article-classification'].value='categories';nodes['article-classification'].listeners.change();
  assert.equal(nodes['article-rows'].children[0].children[1].textContent,'Music category');
  assert.equal(JSON.stringify(raw),before);
  console.log('Article classification: Wikipedia categories, every macro/topic choice, multilabel filtering/sorting, missing versus empty labels, pagination, safe translations and unchanged data passed.');
}

async function checkEditorialTransports() {
  // Aggregate card loading must use compact transports for every member.
  // Canonical files are absent, so accidentally loading them fails this test.
  const languages=['co','eml','fur','lij','lmo','nap','pms','roa-tara','sc','scn','vec'];
  const entries=[],members=[],files={};
  for(const language of languages) {
    const raw=fixture();raw.metadata.demo=false;raw.metadata.language=language;
    raw.metadata.creators.language=language;
    raw.metadata.creators.languages_scanned=[language];
    raw.pages=[{page_id:1,title:`Target ${language}`,category:'Shared category',orphan:true},
      {page_id:2,title:'Non-orphan',orphan:false}];
    const referenceLanguage=language==='vec'?'lmo':'vec';
    const candidates=Array.from({length:70},(_,i)=>({page_id:100+i,title:`Source ${String(i).padStart(2,'0')}`,dead_end:i<8,
      evidence:[{language:referenceLanguage,source_title:`Reference ${i}`,target_title:'Reference target'},
        {language:'it',source_title:`Italian ${i}`,target_title:'Italian target'}]}));
    const record={status:'compared',candidate_count:70,candidates};
    const directory=`${language}-connections-v1`;
    entries.push({language,file:language+'.json',date:raw.metadata.finished_at,kind:'wiki',cards_file:language+'-cards.json',connections_dir:directory});
    members.push({language,file:language+'.json',metadata:raw.metadata,page_count:2});
    files[language+'-cards.json']={format:'orphan_cards_v1',metadata:raw.metadata,census_page_count:2,pages:[raw.pages[0]],
      link_candidates:{1:{...record,candidates:candidates.slice(0,5).map(c=>({...c,evidence:c.evidence.slice(0,1)}))}}};
    files[directory+'/1.json']={format:'connections_v1',metadata:raw.metadata,census_page_count:2,pages:[raw.pages[0]],link_candidates:{1:record}};
  }
  const index={metadata:{kind:'aggregate',language:'all',languages,complete:true,demo:false,started_at:members[0].metadata.started_at,finished_at:members[0].metadata.finished_at},members};
  files['all.json']=index;
  entries.unshift({language:'all',file:'all.json',date:index.metadata.finished_at,kind:'aggregate'});
  const before=JSON.stringify(files);
  const all=await harness('deorphanize',index,'?dataset=all.json',entries,{},files);
  assert.equal(all.document.body.dataset.loading,'false',all.nodes.status.textContent);
  assert.equal(all.nodes['candidate-cards'].children.length,11);
  const allData=all.requests.filter(request=>request.url.startsWith('data/'));
  assert.equal(allData.length,13); // manifest, index, eleven compact card files
  assert(allData.slice(2).every(request=>request.url.endsWith('-cards.json')));
  assert(!all.nodes.status.textContent.includes('Impossibile'));
  for(const card of all.nodes['candidate-cards'].children)assert.equal(findClass(card,'candidate-badge').textContent,'70');
  const pms=all.nodes['candidate-cards'].children.find(card=>titleLink(card).textContent==='Target pms');
  assert.match(findClass(pms,'connect-button').href,/dataset=all.json&page=1&language=pms/);
  const normal=await harness('deorphanize',index,'?dataset=pms.json',entries,{},files);
  assert.deepEqual(normal.requests.filter(request=>request.url.startsWith('data/')).map(request=>request.url),['data/manifest.json','data/pms-cards.json']);
  const overviewPrompt=await preparePrompt(normal,findClass(normal.nodes['candidate-cards'].children[0],'fix-link'));
  const detail=await harness('connections',index,'?dataset=all.json&page=1&language=pms',entries,{},files);
  assert.equal(detail.document.body.dataset.loading,'false',detail.nodes.status.textContent);
  assert.deepEqual(detail.requests.filter(request=>request.url.startsWith('data/')).map(request=>request.url),['data/manifest.json','data/all.json','data/pms-connections-v1/1.json']);
  assert.equal(detail.nodes['connection-candidates'].children.length,50);
  detail.nodes['connection-more'].listeners.click();
  assert.equal(detail.nodes['connection-candidates'].children.length,70);
  assert.equal(descendants(detail.nodes['connection-candidates']).filter(node=>node.className==='connection-evidence').length,140);
  const last=detail.nodes['connection-candidates'].children.at(-1);
  const prompt=await preparePrompt(detail,findClass(last,'fix-link'));
  assert(prompt.includes('Source 69'));assert(!prompt.includes('Source 00'));
  assert(prompt.includes('Italian_69'));assert(prompt.includes('Reference_69'));
  assert.match(prompt,/Evaluate only this single proposed link/);
  const detailOverview=await preparePrompt(detail,detail.nodes['connection-copy-prompt']);
  assert.equal(detailOverview.split('\n\nWIKIPEDIA DOCUMENTS')[0],overviewPrompt.split('\n\nWIKIPEDIA DOCUMENTS')[0]);
  assert.equal(detail.copied.length,0);
  for(const mutation of [
    card=>card.metadata={...card.metadata,input_sha256:'stale'},
    card=>card.census_page_count=3,
    card=>card.pages[0].orphan=false,
    card=>card.link_candidates['999']={},
    card=>card.format='observations_v1'
  ]) {
    const bad=JSON.parse(JSON.stringify(files));mutation(bad['pms-cards.json']);
    const invalid=await harness('deorphanize',index,'?dataset=all.json',entries,{},bad);
    assert.match(invalid.nodes.status.textContent,/Impossibile caricare/);
    assert.equal(invalid.document.body.dataset.loading,'true');
  }
  const unsafe=entries.map(entry=>entry.language==='pms'?{...entry,connections_dir:'../outside'}:entry);
  const invalid=await harness('connections',index,'?dataset=all.json&page=1&language=pms',unsafe,{},files);
  assert.match(invalid.nodes.status.textContent,/Nome dataset non valido/);
  assert(!invalid.requests.some(request=>request.url.includes('outside')));
  const unknown=await harness('connections',index,'?dataset=all.json&page=1&language=en',entries,{},files);
  assert.match(unknown.nodes.status.textContent,/Pagina orfana non presente/);
  assert.equal(unknown.requests.length,2);
  const legacy=await harness('deorphanize',index,'?dataset=pms.json',entries.map(({cards_file,connections_dir,...entry})=>entry),
    {contentLengths:{'data/pms.json':2.7*1024**3}},files);
  assert.match(legacy.nodes.status.textContent,/Dataset troppo grande per il browser/);
  assert.match(legacy.nodes.status.textContent,/orphanwiki dashboard/);
  assert.equal(legacy.document.body.dataset.loading,'true');
  assert.equal(JSON.stringify(files),before);
  console.log('Editorial transports: eleven-language cards without canonical downloads, focused detail, all evidence, prompt parity, stale/unsafe rejection and unchanged inputs passed.');
}
async function checkConnectionCopy() {
  const raw=fixture();raw.metadata.demo=false;
  const target=raw.pages.find(page=>page.orphan),key=String(target.page_id);
  raw.link_candidates[key]={status:'compared',candidate_count:7,candidates:Array.from({length:7},(_,i)=>({page_id:100+i,title:`Local ${i}`,dead_end:i===6,evidence:[{language:'lmo',source_title:`Reference ${i}`,target_title:target.title}]}))};
  const result=await harness('connections',raw,`?dataset=fixture.json&page=${key}`),{nodes}=result;
  assert.equal(result.copied.length,0);assert.equal(nodes['connection-copy-prompt'].disabled,false);
  const prompt=await preparePrompt(result,nodes['connection-copy-prompt']);
  assert(prompt.includes('Local 6'));assert.equal((prompt.match(/^\d+\. LOCAL_X/gm)||[]).length,5);
  assert.equal(result.copied.length,0);assert(!prompt.includes('creator_key'));
  await nodes['llm-prompt-copy'].listeners.click();assert.equal(result.copied[0],prompt);
  const single=await preparePrompt(result,findClass(nodes['connection-candidates'].children[0],'fix-link'));
  assert.match(single,/Evaluate only this single proposed link/);
  nodes['ui-language-toggle'].listeners.click();assert.equal(result.copied.length,1);
  const english=await preparePrompt(result,nodes['connection-copy-prompt']);assert.match(english,/Reply in English/);
  raw.link_candidates[key].candidate_count=0;raw.link_candidates[key].candidates=[];
  const disabled=await harness('connections',raw,`?dataset=fixture.json&page=${key}`);
  assert.equal(disabled.nodes['connection-copy-prompt'].disabled,true);
  await disabled.nodes['connection-copy-prompt'].listeners.click();assert.equal(disabled.copied.length,0);
  console.log('Connection preparation: five priority candidates, single-card scope, separate explicit copy, translated prompts and disabled empty state passed.');
}
async function checkSharedEditorialReviews() {
  const raw=fixture();raw.metadata.demo=false;
  raw.pages=[{page_id:1,title:'Target one',orphan:true},{page_id:10,title:'Target two',orphan:true}];
  const candidates=[2,3,4].map(id=>({page_id:id,title:'Source '+id,dead_end:id===2,evidence:[
    {language:'lmo',source_title:'Reference '+id,target_title:'Foreign target'},
    {language:'it',source_title:'Italian '+id,target_title:'Italian target'}]}));
  raw.link_candidates={1:{candidate_count:3,status:'compared',candidates},
    10:{candidate_count:1,status:'compared',candidates:[{page_id:11,title:'Other source',evidence:candidates[0].evidence}]}};
  const before=JSON.stringify(raw),reviewStore=new Map();
  const overview=await harness('deorphanize',raw,undefined,null,{reviewStore});
  assert.equal(overview.requests.filter(request=>request.url.startsWith('api/reviews')).length,1);
  assert.equal(overview.requests.filter(request=>request.method==='POST').length,0);
  assert.equal(overview.nodes['llm-prompt-finished'].disabled,true);
  const card=()=>overview.nodes['candidate-cards'].children.find(card=>titleLink(card).textContent==='Target one');
  await preparePrompt(overview,findClass(card(),'fix-link'));
  assert.equal(overview.nodes['llm-prompt-finished'].disabled,false);
  overview.nodes['llm-prompt-debug'].checked=true;
  await overview.nodes['llm-prompt-finished'].listeners.click();
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(overview.nodes['llm-prompt'].open,false);
  assert.equal(overview.nodes['llm-prompt-text'].value,'');assert.equal(overview.nodes['llm-prompt-debug'].checked,false);
  assert.equal(overview.nodes['review-dialog'].open,true);
  assert.equal(overview.nodes['review-rows'].children.length,3);
  const choose=async(result,index,statusIndex)=>{
    const row=result.nodes['review-rows'].children[index],button=findClass(row,'review-actions').children[statusIndex];
    assert.equal(button.disabled,false);button.listeners.click();
    await new Promise(resolve=>setImmediate(resolve));
  };
  await choose(overview,0,0);await choose(overview,1,2);await choose(overview,2,1);
  assert.equal(reviewStore.size,3);
  assert.deepEqual([...reviewStore.values()].map(review=>review.status),['fixed','bad_link','not_fixed']);
  assert.match(overview.nodes['review-dialog-status'].textContent,/salvata.*tutti/);
  const posts=overview.requests.filter(request=>request.method==='POST');
  assert.equal(posts.length,3);
  for(const request of posts) {
    assert.equal(request.url,'api/reviews');assert.equal(request.headers['Content-Type'],'application/json');
    const body=JSON.parse(request.body);
    assert.deepEqual(Object.keys(body).sort(),['dataset','language','source_id','status','target_id','version']);
    assert.equal(body.target_id,1);assert.equal(body.language,'vec');
  }
  overview.nodes['review-close'].listeners.click();
  assert.equal(overview.nodes['review-rows'].children.length,0);
  const prompt=await preparePrompt(overview,findClass(card(),'fix-link'));
  assert(prompt.includes('Source 4'));assert(!prompt.includes('Source 2'));assert(!prompt.includes('Source 3'));

  // A separate browser sees all statuses; closed pairs retain all evidence.
  const connection=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{reviewStore});
  const cards=()=>connection.nodes['connection-candidates'].children;
  assert.deepEqual(cards().map(card=>titleLink(card).textContent),['Source 4','Source 2','Source 3']);
  assert.equal(descendants(connection.nodes['connection-candidates']).filter(node=>node.className==='connection-evidence').length,6);
  assert(cards().slice(1).every(card=>findClass(card,'fix-link').getAttribute('aria-disabled')==='true'));
  assert.match(findClass(cards()[1],'review-badge').textContent,/Fixato/);
  assert.match(findClass(cards()[2],'review-badge').textContent,/Bad link/);
  await preparePrompt(connection,findClass(cards()[0],'fix-link'));
  connection.nodes['llm-prompt-finished'].listeners.click();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(connection.nodes['review-rows'].children.length,1);
  await choose(connection,0,0);
  await overview.nodes['review-refresh'].listeners.click();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(titleLink(overview.nodes['candidate-cards'].children.at(-1)).textContent,'Target one');
  assert.equal(findClass(card(),'fix-link').getAttribute('aria-disabled'),'true');
  assert.equal(connection.nodes['connection-candidates'].children.length,3);

  // Closed pairs can be reopened from Connect without preparing another prompt.
  connection.nodes['review-close'].listeners.click();
  const source2=cards().find(card=>titleLink(card).textContent==='Source 2');
  findClass(source2,'review-edge').listeners.click();await new Promise(resolve=>setImmediate(resolve));
  await choose(connection,0,1);
  assert.equal(titleLink(cards()[0]).textContent,'Source 2');
  assert.equal(findClass(cards()[0],'fix-link').tag,'button');
  assert.equal(reviewStore.get('vec:2:1').status,'not_fixed');

  // Optimistic version checks surface a concurrent review without overwriting it.
  reviewStore.set('vec:2:1',{...reviewStore.get('vec:2:1'),status:'bad_link',version:3});
  await choose(connection,0,0);
  assert.equal(reviewStore.get('vec:2:1').status,'bad_link');
  assert.match(connection.nodes['review-dialog-status'].textContent,/altro utente/);
  assert.match(connection.nodes['review-rows'].children[0].children[1].textContent,/Bad link/);
  connection.nodes['ui-language-toggle'].listeners.click();
  assert.equal(connection.nodes['review-dialog'].open,false);
  assert.equal(connection.nodes['review-rows'].children.length,0);
  assert.match(findClass(cards().find(card=>titleLink(card).textContent==='Source 2'),'review-badge').textContent,/Shared review/);
  assert.equal(JSON.stringify(raw),before);
  assert([...overview.storage.keys()].every(key=>!key.includes('review')));

  const offline=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{reviewsUnavailable:true});
  findClass(offline.nodes['connection-candidates'].children[0],'review-edge').listeners.click();
  await new Promise(resolve=>setImmediate(resolve));
  assert.match(offline.nodes['review-dialog-status'].textContent,/non disponibili/);
  assert(findClass(offline.nodes['review-rows'].children[0],'review-actions').children.every(button=>button.disabled));
  assert.equal(offline.requests.filter(request=>request.method==='POST').length,0);
  const failed=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{
    reviewAPI:async(url,options)=>options?.method==='POST'?{ok:false,status:503,json:async()=>({error:'unavailable'})}:
      {ok:true,status:200,json:async()=>({format:'editorial_reviews_v1',reviews:[]})}
  });
  findClass(failed.nodes['connection-candidates'].children[0],'review-edge').listeners.click();
  await new Promise(resolve=>setImmediate(resolve));await choose(failed,0,0);
  assert.match(failed.nodes['review-dialog-status'].textContent,/non confermato/);
  assert.equal(vm.runInContext('editorialReviews.size',failed.context),0);
  assert.equal(findClass(failed.nodes['connection-candidates'].children[0],'fix-link').tag,'button');

  let releaseSave;
  const stale=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{
    reviewAPI:async(url,options)=>options?.method==='POST'?new Promise(resolve=>{
      const body=JSON.parse(options.body);
      releaseSave=()=>resolve({ok:true,status:200,json:async()=>({review:{language:body.language,source_id:body.source_id,
        target_id:body.target_id,status:body.status,version:1,updated_at:'2026-10-20T12:00:00Z'}})});
    }):{ok:true,status:200,json:async()=>({format:'editorial_reviews_v1',reviews:[]})}
  });
  findClass(stale.nodes['connection-candidates'].children[0],'review-edge').listeners.click();
  await new Promise(resolve=>setImmediate(resolve));
  findClass(stale.nodes['review-rows'].children[0],'review-actions').children[0].listeners.click();
  const saving=stale.requests.find(request=>request.method==='POST');
  stale.nodes['review-close'].listeners.click();releaseSave();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(saving.signal.aborted,true);
  assert.equal(stale.nodes['review-dialog'].open,false);
  assert.equal(vm.runInContext('editorialReviews.size',stale.context),0);
  assert.equal(stale.nodes['review-rows'].children.length,0);

  const wide=JSON.parse(JSON.stringify(raw));
  wide.link_candidates[1]={candidate_count:7,status:'compared',candidates:Array.from({length:7},(_,i)=>({
    page_id:20+i,title:'Wide source '+i,evidence:candidates[0].evidence}))};
  const capped=await harness('deorphanize',wide);
  await preparePrompt(capped,findClass(capped.nodes['candidate-cards'].children[0],'fix-link'));
  capped.nodes['llm-prompt-finished'].listeners.click();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(capped.nodes['review-rows'].children.length,5);
  assert(!capped.nodes['review-rows'].children.some(row=>row.children[0].children[0].textContent==='Wide source 5'));
  console.log('Shared editorial reviews: Finished pair scope, no text persistence, independent readers, closed-pair ordering/filtering, full Connect evidence, reopening, conflicts, translations, failed/stale saves and unavailable service passed.');
}
async function checkPersonalization() {
  const raw=fixture();raw.metadata.demo=false;
  raw.metadata.topics={status:'predicted'};
  const target=raw.pages.find(page=>page.orphan);
  for(const page of raw.pages){page.topic_status='predicted';page.topic_labels=page.page_id===target.page_id?['Culture.Media.Music']:[];}
  const entries=[{language:'vec',file:'fixture.json',date:raw.metadata.finished_at,demo:false,observations_file:'fixture-observations.json'}];
  const files={'fixture.json':raw,'fixture-observations.json':{
    format:'observations_v1',metadata:raw.metadata,columns:['page_id','topic_labels','topic_status'],
    rows:[[target.page_id,['Culture.Media.Music'],'predicted']]}};
  const apiResponse=url=>{
    if(url.hostname==='meta.wikimedia.org')return {query:{globaluserinfo:{id:1,name:'Editor',merged:[
      {wiki:'vecwiki',url:'https://vec.wikipedia.org',id:11,editcount:1},
      {wiki:'enwiki',url:'https://en.wikipedia.org',id:22,editcount:1}]}}};
    if(url.hostname==='vec.wikipedia.org')return {query:{usercontribs:[{userid:11,pageid:target.page_id,ns:0,timestamp:'2026-10-01T00:00:00Z'}]}};
    if(url.hostname==='en.wikipedia.org')return {query:{usercontribs:[{userid:22,pageid:999,ns:0,timestamp:'2026-09-30T00:00:00Z'}]}};
    throw new Error('Unapproved API');
  };
  const result=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{apiResponse},files);
  assert.equal(result.requests.filter(request=>request.url.startsWith('https://')).length,0);
  result.nodes['profile-username'].value='Editor';
  result.nodes['profile-apply'].listeners.click();
  await new Promise(resolve=>setImmediate(resolve));await new Promise(resolve=>setImmediate(resolve));
  assert.equal(result.nodes['candidate-sort'].value,'personalized');
  assert.equal(result.nodes['profile-sort-option'].disabled,false);
  assert.equal(titleLink(result.nodes['candidate-cards'].children[0]).textContent,target.title);
  assert.match(result.nodes['profile-status'].textContent,/2 modifiche.*1/);
  const api=result.requests.filter(request=>request.url.startsWith('https://')).map(request=>new URL(request.url));
  assert.equal(api.length,3);
  assert(api.every(url=>url.searchParams.get('origin')==='*'&&['globaluserinfo','usercontribs'].includes(url.searchParams.get('meta')||url.searchParams.get('list'))));
  assert(api.filter(url=>url.searchParams.get('list')==='usercontribs').every(url=>url.searchParams.get('uclimit')==='100'&&url.searchParams.has('ucuserids')));
  const uncovered=await harness('deorphanize',raw,'?dataset=fixture.json',entries.map(({observations_file,...entry})=>entry),{apiResponse},{'fixture.json':raw});
  vm.runInContext(`globalThis.interestCalls=0;const originalCalculate=nativeCalculate;
    nativeCalculate=(kind,input)=>{if(kind==='interests')interestCalls++;return originalCalculate(kind,input);}`,uncovered.context);
  uncovered.nodes['profile-username'].value='Editor';uncovered.nodes['profile-apply'].listeners.click();
  await new Promise(resolve=>setImmediate(resolve));await new Promise(resolve=>setImmediate(resolve));
  assert.equal(uncovered.nodes['profile-sort-option'].disabled,true);
  assert.match(uncovered.nodes['profile-status'].textContent,/Nessuna.*temi utilizzabili/);
  assert.equal(vm.runInContext('interestCalls',uncovered.context),0);
  const outside=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{apiResponse:url=>{
    if(url.hostname==='meta.wikimedia.org')return {query:{globaluserinfo:{id:1,name:'Editor',merged:[
      {wiki:'enwiki',url:'https://en.wikipedia.org',id:22,editcount:1}]}}};
    return {query:{usercontribs:[{userid:22,pageid:999,ns:0,timestamp:'2026-10-01T00:00:00Z'}]}};
  }},files);
  outside.nodes['profile-username'].value='Editor';outside.nodes['profile-apply'].listeners.click();
  await new Promise(resolve=>setImmediate(resolve));await new Promise(resolve=>setImmediate(resolve));
  assert.equal(outside.nodes['profile-sort-option'].disabled,true);
  assert.match(outside.nodes['profile-status'].textContent,/Nessuna delle ultime 100 modifiche/);
  const missing=JSON.parse(JSON.stringify(raw));delete missing.metadata.topics;
  const noModel=await harness('deorphanize',missing,'?dataset=fixture.json',entries,{apiResponse},
    {'fixture.json':missing,'fixture-observations.json':{...files['fixture-observations.json'],metadata:missing.metadata}});
  noModel.nodes['profile-username'].value='Editor';noModel.nodes['profile-apply'].listeners.click();
  await new Promise(resolve=>setImmediate(resolve));await new Promise(resolve=>setImmediate(resolve));
  assert.match(noModel.nodes['profile-status'].textContent,/Nessuna.*temi utilizzabili/);
  assert.equal(noModel.nodes['profile-sort-option'].disabled,true);

  // English edits inform the ranking of Venetian cards without loading English
  // cards or an aggregate. Read only the newest real census for each language.
  const crossRaw=JSON.parse(JSON.stringify(raw));
  const crossTarget=crossRaw.pages.filter(page=>page.orphan).at(-1);
  assert.notEqual(crossTarget.page_id,target.page_id);
  for(const page of crossRaw.pages)page.topic_labels=page.page_id===crossTarget.page_id?['Culture.Media.Music']:[];
  const englishMetadata={...raw.metadata,language:'en',finished_at:'2026-10-02T00:00:00Z'};
  const englishObservations={format:'observations_v1',metadata:englishMetadata,
    columns:['page_id','topic_labels','topic_status'],rows:[[999,['Culture.Media.Music'],'predicted']]};
  const crossEntries=[...entries,
    {language:'en',file:'en-old.json',date:'2026-09-01T00:00:00Z',observations_file:'en-old-observations.json'},
    {language:'en',file:'en-new.json',date:englishMetadata.finished_at,observations_file:'en-observations.json'},
    {language:'en',file:'en-demo.json',date:'2027-01-01T00:00:00Z',demo:true,observations_file:'en-demo-observations.json'},
    {language:'all',kind:'aggregate',file:'all.json',date:'2027-01-01T00:00:00Z',observations_file:'all-observations.json'}];
  const crossFiles={...files,'fixture.json':crossRaw,'en-observations.json':englishObservations};
  const apply=async result=>{
    result.nodes['profile-username'].value='Editor';result.nodes['profile-apply'].listeners.click();
    await new Promise(resolve=>setImmediate(resolve));await new Promise(resolve=>setImmediate(resolve));
  };
  const cross=await harness('deorphanize',crossRaw,'?dataset=fixture.json',crossEntries,{apiResponse},crossFiles);
  assert.equal(cross.requests.filter(request=>request.url.endsWith('-observations.json')).length,0);
  await apply(cross);
  assert.equal(cross.nodes['candidate-sort'].value,'personalized');
  assert.equal(titleLink(cross.nodes['candidate-cards'].children[0]).textContent,crossTarget.title);
  assert.equal(cross.nodes['candidate-cards'].children.length,25);
  assert.match(cross.nodes['candidate-info'].textContent,new RegExp(`^${crossRaw.pages.filter(page=>page.orphan).length} orfane`));
  assert.match(cross.nodes['profile-status'].textContent,/2 modifiche.*2.*EN, VEC/);
  assert(cross.requests.some(request=>request.url==='data/en-observations.json'));
  assert(!cross.requests.some(request=>/en-old|en-demo|all\.json|all-observations|en-new\.json/.test(request.url)));
  assert(cross.nodes['candidate-cards'].children.every(card=>titleLink(card).href.startsWith('https://vec.wikipedia.org/')));

  // Foreign topics alone suffice when local observations are unavailable.
  const foreignOnly=await harness('deorphanize',crossRaw,'?dataset=fixture.json',crossEntries.filter(entry=>entry.language!=='vec')
    .concat(entries.map(({observations_file,...entry})=>entry)),{apiResponse},crossFiles);
  await apply(foreignOnly);
  assert.equal(foreignOnly.nodes['candidate-sort'].value,'personalized');
  assert.equal(titleLink(foreignOnly.nodes['candidate-cards'].children[0]).textContent,crossTarget.title);
  assert.match(foreignOnly.nodes['profile-status'].textContent,/2 modifiche.*1.*EN\./);

  for(const patch of [{language:'lmo'},{finished_at:'2026-09-01T00:00:00Z'},{complete:false},{demo:true}]) {
    const invalid=await harness('deorphanize',crossRaw,'?dataset=fixture.json',crossEntries,{apiResponse},
      {...crossFiles,'en-observations.json':{...englishObservations,metadata:{...englishMetadata,...patch}}});
    await apply(invalid);
    assert.equal(invalid.nodes['profile-sort-option'].disabled,true);
    assert.match(invalid.nodes['profile-status'].textContent,/Personalizzazione non disponibile/);
  }
  console.log('Personalization: explicit identity lookup, cross-language topics, latest manifest census, coverage, validation and topic-first ranking passed.');
}
async function checkProfileTopicShards() {
  const raw=fixture();raw.metadata.demo=false;raw.metadata.topics={status:'predicted'};
  const entry={language:'vec',file:'fixture.json',date:raw.metadata.finished_at,
    observations_file:'must-not-load.json',profile_topics_dir:'topics'};
  const index={format:'profile_topics_index_v1',metadata:raw.metadata,shard_size:1024,shards:[0,1],census_page_count:4};
  const shard={format:'profile_topics_shard_v1',metadata:raw.metadata,bucket:0,
    rows:[[1,'predicted',['Culture.Media.Music']],[2,'predicted',[]],[3,'missing',null]]};
  const files={'fixture.json':raw,'topics/index.json':index,'topics/0.json':shard,
    'topics/1.json':{...shard,bucket:1,rows:[[1024,'predicted',['Culture.Media.Films']]]}};
  const result=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{},files);
  result.context.edits=[1,2,3,1024,99999,1].map(page_id=>({language:'vec',page_id,namespace:0,
    timestamp:'2026-10-01T00:00:00Z',topic_labels:null}));
  const enriched=await vm.runInContext('observedTopics(edits,profileRequestVersion)',result.context);
  assert.deepEqual(JSON.parse(JSON.stringify(enriched.map(edit=>edit.topic_labels))),
    [['Culture.Media.Music'],[],null,['Culture.Media.Films'],null,['Culture.Media.Music']]);
  assert.equal(result.requests.filter(request=>request.url.startsWith('data/topics/')).length,3);
  const before=result.requests.length;
  await vm.runInContext('observedTopics(edits,profileRequestVersion)',result.context);
  assert.equal(result.requests.length,before);
  for(const [path,patch] of [
    ['topics/index.json',{metadata:{...raw.metadata,language:'en'}}],
    ['topics/index.json',{shard_size:0}],['topics/index.json',{shards:[1,0]}],
    ['topics/0.json',{bucket:1}],['topics/0.json',{metadata:{...raw.metadata,finished_at:'old'}}],
    ['topics/0.json',{rows:[[1024,'predicted',[]]]}],
    ['topics/0.json',{rows:[[1,'predicted',[]],[1,'predicted',[]]]}],
    ['topics/0.json',{rows:Array.from({length:1025},(_,id)=>[id+1,'predicted',[]])}]
  ]) {
    const invalid=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{},
      {...files,[path]:{...files[path],...patch}});
    invalid.context.edits=[result.context.edits[0]];
    await assert.rejects(vm.runInContext('observedTopics(edits,profileRequestVersion)',invalid.context),/Trasporto osservazioni non valido/);
  }
  const unsafe=await harness('deorphanize',raw,'?dataset=fixture.json',[{...entry,profile_topics_dir:'../topics'}],{},files);
  unsafe.context.edits=[result.context.edits[0]];
  await assert.rejects(vm.runInContext('observedTopics(edits,profileRequestVersion)',unsafe.context),/Trasporto osservazioni non valido/);

  let release;
  const stale=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{
    beforeFetch:url=>url==='data/topics/index.json'?new Promise(resolve=>{release=resolve;}):undefined
  },files);
  stale.context.edits=[result.context.edits[0]];
  const loading=vm.runInContext('observedTopics(edits,profileRequestVersion)',stale.context);
  vm.runInContext('++profileRequestVersion;profileController.abort();profileController=new AbortController()',stale.context);
  release();
  await assert.rejects(loading,error=>error.name==='AbortError');
  assert.equal(vm.runInContext('profileTopicCache.size',stale.context),0);
  assert(stale.requests.find(request=>request.url==='data/topics/index.json').signal.aborted);
  assert(!stale.requests.some(request=>request.url==='data/topics/0.json'));
  console.log('Profile topic shards: sparse IDs, empty/missing topics, deduplication, bounded caching, metadata validation and cancellation passed.');
}
async function benchmarkPersonalization() {
  // Fixed census and edit sequences; compare deterministic work, not noisy CPU
  // timings or public API latency. No production files or network are used.
  const raw=fixture();raw.metadata={language:'vec',complete:true,demo:false,
    started_at:'2026-10-01T00:00:00Z',finished_at:'2026-10-01T00:00:00Z',topics:{status:'predicted'}};
  const languages=['vec','en','fr','de'],size=65536,files={'fixture.json':raw};
  const columns=['page_id','topic_status','topic_labels','title','orphan','length_bytes',
    'age_days','creator_key','in_degree','out_degree','quality_score','category'];
  const entries=languages.map(language=>({language,file:language==='vec'?'fixture.json':language+'.json',
    date:raw.metadata.finished_at,observations_file:language+'-observations.json',profile_topics_dir:language+'-topics'}));
  for(const entry of entries) {
    const metadata={...raw.metadata,language:entry.language};
    const rows=Array.from({length:size},(_,i)=>{
      const id=i+1,status=id%13===0?'missing':'predicted';
      const labels=status==='missing'?null:id%7===0?[]:[id%2?'Culture.Media.Music':'Culture.Media.Films'];
      return [id,status,labels,'Synthetic article '+id+' '+ 'x'.repeat(80),id%3===0,5000,1000,'registered:12',3,7,0.5,'Synthetic category'];
    });
    files[entry.observations_file]={format:'observations_v1',metadata,columns,rows};
    const buckets=new Map();
    for(const row of rows) {
      const bucket=Math.floor(row[0]/1024);
      if(!buckets.has(bucket))buckets.set(bucket,[]);
      buckets.get(bucket).push(row.slice(0,3));
    }
    files[entry.profile_topics_dir+'/index.json']={format:'profile_topics_index_v1',metadata,
      shard_size:1024,shards:[...buckets.keys()],census_page_count:size};
    for(const [bucket,shardRows] of buckets)files[entry.profile_topics_dir+'/'+bucket+'.json']={
      format:'profile_topics_shard_v1',metadata,bucket,rows:shardRows};
  }
  let gated=false;
  const topicPending=[];
  const result=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{
    beforeFetch:(url,options)=>gated&&String(url).includes('-topics/')?new Promise((resolve,reject)=>{
      topicPending.push(resolve);options.signal.addEventListener('abort',()=>reject(options.signal.reason),{once:true});
    }):undefined
  },files);
  for(const workload of ['clustered','dispersed']) {
    const edits=Array.from({length:100},(_,i)=>({language:languages[i%4],
      page_id:workload==='clustered'?100+Math.floor(i/4):1+Math.floor(i/4)*2048,
      namespace:0,timestamp:'2026-10-01T00:00:00Z',topic_labels:null}));
    // Include repeated edits, missing pages, missing predictions and known
    // negatives; preserve every edit and its original order in both paths.
    edits[96]={...edits[0]};edits[97].page_id=999999;edits[98].namespace=1;edits[99].page_id=14;
    const reference=edits.map(edit=>{
      const entry=entries.find(entry=>entry.language===edit.language);
      const row=files[entry.observations_file].rows.find(row=>row[0]===edit.page_id);
      return {...edit,topic_labels:edit.namespace===0&&row?.[1]==='predicted'?row[2]:null};
    });
    const legacyFiles=entries.map(entry=>files[entry.observations_file]);
    const baseline={requests:legacyFiles.length,rows:legacyFiles.reduce((n,file)=>n+file.rows.length,0),
      bytes:legacyFiles.reduce((n,file)=>n+Buffer.byteLength(JSON.stringify(file)),0)};
    vm.runInContext('profileTopicCache.clear()',result.context);
    result.context.benchmarkEdits=edits;
    const offset=result.requests.length;
    gated=true;
    let finished=false,rounds=0,maxConcurrent=0;
    const loading=vm.runInContext('observedTopics(benchmarkEdits,profileRequestVersion)',result.context)
      .then(value=>{finished=true;return value;});
    for(let turn=0;!finished;turn++) {
      assert(turn<40,'topic scheduler must finish');
      await new Promise(resolve=>setImmediate(resolve));
      if(topicPending.length) {
        rounds++;maxConcurrent=Math.max(maxConcurrent,topicPending.length);
        assert(topicPending.length<=4,'shared topic request budget');
        for(const release of topicPending.splice(0))release();
      }
    }
    gated=false;
    const actual=await loading;
    assert.deepEqual(JSON.parse(JSON.stringify(actual)),reference);
    const requests=result.requests.slice(offset);
    assert(requests.every(request=>!request.url.endsWith('-observations.json')));
    const optimized={requests:requests.length,rows:requests.reduce((n,request)=>n+(files[request.url.slice(5)].rows?.length??0),0),
      bytes:requests.reduce((n,request)=>n+Buffer.byteLength(JSON.stringify(files[request.url.slice(5)])),0)};
    assert.deepEqual({requests:optimized.requests,rows:optimized.rows},workload==='clustered'?
      {requests:8,rows:4092}:{requests:100,rows:98300});
    assert(optimized.bytes<baseline.bytes*0.2);
    assert(optimized.rows<baseline.rows*0.4);
    const sequentialRounds=entries.reduce((n,entry)=>n+1+Math.ceil(requests.filter(request=>
      request.url.startsWith('data/'+entry.profile_topics_dir+'/')&&!request.url.endsWith('/index.json')).length/4),0);
    assert.equal(rounds,workload==='clustered'?2:25);
    assert.equal(maxConcurrent,4);
    assert(rounds<sequentialRounds);
    const warmOffset=result.requests.length;
    const warm=await vm.runInContext('observedTopics(benchmarkEdits,profileRequestVersion)',result.context);
    assert.deepEqual(JSON.parse(JSON.stringify(warm)),reference);
    assert.equal(result.requests.length,warmOffset);
    console.log('Personalization deterministic benchmark: '+JSON.stringify({workload,edits:100,census_pages:size*4,
      baseline,optimized,sequential_sharded_rounds:sequentialRounds,parallel_sharded_rounds:rounds,max_concurrent:maxConcurrent,warm_requests:0}));
  }

  // A logical round completes only when the test releases all outstanding
  // responses. This makes the contribution fan-out benchmark reproducible.
  const wikiLanguages=Array.from({length:24},(_,i)=>'a'+String.fromCharCode(97+i));
  const pending=[],contributions=wikiLanguages.flatMap((language,index)=>Array.from({length:100},(_,i)=>({
    language,page_id:1000+i,namespace:0,timestamp:new Date(Date.UTC(2026,9,1,0,Math.floor(index/2),Math.floor(i/2))).toISOString(),topic_labels:null})));
  const scheduled=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{
    beforeFetch:(url,options)=>String(url).includes('list=usercontribs')?new Promise((resolve,reject)=>{
      pending.push(resolve);options.signal.addEventListener('abort',()=>reject(options.signal.reason),{once:true});
    }):undefined,
    apiResponse:url=>url.hostname==='meta.wikimedia.org'?{query:{globaluserinfo:{id:1,name:'Editor',merged:wikiLanguages.map((language,i)=>({
      wiki:language+'wiki',url:'https://'+language+'.wikipedia.org',id:i+1}))}}}:
      {query:{usercontribs:contributions.filter(edit=>edit.language===url.hostname.split('.')[0]).map(edit=>({
        userid:wikiLanguages.indexOf(edit.language)+1,pageid:edit.page_id,ns:edit.namespace,timestamp:edit.timestamp}))}}
  },files);
  vm.runInContext(`globalThis.retainedEditPeaks=[];const originalSort=Array.prototype.sort;
    Array.prototype.sort=function(...args){
      if(this.length&&this[0]?.namespace!==undefined&&this[0]?.topic_labels===null)retainedEditPeaks.push(this.length);
      return originalSort.apply(this,args);
    };`,scheduled.context);
  let finished=false;
  const recent=vm.runInContext("recentWikipediaEdits('Editor',profileController.signal)",scheduled.context).then(value=>{finished=true;return value;});
  let rounds=0,maxConcurrent=0;
  while(!finished) {
    await new Promise(resolve=>setImmediate(resolve));
    if(pending.length) {
      maxConcurrent=Math.max(maxConcurrent,pending.length);rounds++;
      for(const release of pending.splice(0).reverse())release();
    }
    assert(rounds<=3);
  }
  const actual=await recent;
  contributions.sort((a,b)=>b.timestamp.localeCompare(a.timestamp)||a.language.localeCompare(b.language)||a.page_id-b.page_id);
  assert.deepEqual(JSON.parse(JSON.stringify(actual)),contributions.slice(0,100));
  assert.equal(rounds,3);assert.equal(maxConcurrent,8);
  const retainedPeak=vm.runInContext('Math.max(...retainedEditPeaks)',scheduled.context);
  assert.equal(retainedPeak,200);
  console.log('Personalization deterministic contribution benchmark: '+JSON.stringify({wikipedias:24,edits:100,
    baseline_rounds:6,optimized_rounds:rounds,max_concurrent:maxConcurrent,previous_retained_peak:2400,retained_peak:retainedPeak}));
}
async function checkCreatorHistory() {
  const raw=fixture();
  raw.metadata.history={status:'collected',matched_pages:raw.pages.length};
  raw.pages.forEach((page,index)=>{
    page.creator_continuity_months_before_creation=index%4;
    page.creator_groups_at_creation_history=index%3===0?['sysop','rollbacker']:['editor'];
  });
  const {nodes}=await harness('creators',raw);
  assert.equal(nodes['history-section'].hidden,false);
  assert(nodes['creator_continuity_months_before_creation-chart'].children[0].children.some(node=>node.tag==='rect'));
  assert(nodes['creator-groups'].children.some(node=>node.className==='bar-row'));
  assert.match(nodes['creator-groups-note'].textContent,/prima revisione/);
  console.log('Historical creators: prior continuity and multilabel groups are rendered separately from current bot status.');
}
async function checkPreparedSummaries() {
  const raw=fixture(),summary=JSON.parse(fs.readFileSync(path.join(fixtureDir,'fixture-summary.json')));
  const creatorIndex=JSON.parse(fs.readFileSync(path.join(fixtureDir,'fixture-creators.json')));
  raw.metadata.demo=false;summary.metadata.demo=false;creatorIndex.metadata.demo=false;
  const entry={language:'vec',file:'fixture.json',date:raw.metadata.finished_at,demo:false,
    summary_file:'fixture-summary.json',creator_index_file:'fixture-creators.json'};
  assert.equal(summary.census_page_count,raw.pages.length);
  assert.equal(summary.reports.overview.orphans,raw.pages.filter(page=>page.orphan).length);
  const files={'fixture-summary.json':summary,'fixture-creators.json':creatorIndex,'fixture.json':raw};
  const articles=await harness('articles',raw,'?dataset=fixture.json',[entry],{},files);
  assert.deepEqual(articles.requests.map(request=>request.url),['data/manifest.json','data/fixture-summary.json']);
  assert.equal(articles.nodes['article-load'],undefined);
  assert(articles.nodes['topics-full'].children.some(row=>row.className==='bar-row'));
  assert.equal(articles.nodes['article-rows'].children.length,25);
  assert.match(articles.nodes['article-info'].textContent,/Anteprima: 25 di/);
  articles.nodes['article-more'].listeners.click();
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(articles.nodes['article-rows'].children.length,50);
  assert(articles.requests.some(request=>request.url==='data/fixture.json'));
  const stale=JSON.parse(JSON.stringify(summary));stale.article_preview.pop();
  const rejected=await harness('articles',raw,'?dataset=fixture.json',[entry],{}, {...files,'fixture-summary.json':stale});
  assert.match(rejected.nodes.status.textContent,/Riepilogo non valido/);
  const legacy=JSON.parse(JSON.stringify(summary));delete legacy.article_preview;
  const old=await harness('articles',raw,'?dataset=fixture.json',[entry],{}, {...files,'fixture-summary.json':legacy});
  assert.equal(old.nodes['article-rows'].children.length,0);
  assert.equal(old.nodes['article-more'].hidden,false);
  assert.match(old.nodes['article-info'].textContent,/Anteprima non disponibile/);
  const creators=await harness('creators',raw,'?dataset=fixture.json',[entry],{},files);
  assert.deepEqual(creators.requests.map(request=>request.url),
    ['data/manifest.json','data/fixture-summary.json','data/fixture-creators.json']);
  assert.equal(creators.nodes['creator-section'].hidden,false);
  assert.equal(creators.nodes['creator-rows'].children.length,15);
  console.log('Prepared Rust summaries: the first 25 articles render from the summary without a full transport or load button.');
  await checkArticleIndex();
}
async function checkArticleIndex() {
  const raw=fixture(),summary=JSON.parse(fs.readFileSync(path.join(fixtureDir,'fixture-summary.json')));
  const observations=JSON.parse(fs.readFileSync(path.join(fixtureDir,'fixture-observations.json')));
  const articleIndex=JSON.parse(fs.readFileSync(path.join(fixtureDir,'fixture-articles.json')));
  for(const document of [raw,summary,observations,articleIndex])document.metadata.demo=false;
  for(const [position,page] of raw.pages.entries()) {
    page.topic_status=position%3?'predicted':'missing';
    page.topic_labels=position%3===1?['Culture.Media.Music','STEM.STEM*']:position%3===2?[]:null;
    if(position%11===0)page.length_bytes=null;
    if(position%13===0)page.created_at=null;
    if(position%17===0)page.category=null;
  }
  const byID=new Map(raw.pages.map(page=>[page.page_id,page]));
  for(const document of [observations,articleIndex])
    document.rows=document.rows.map(row=>document.columns.map(key=>byID.get(row[document.columns.indexOf('page_id')])[key]??null));
  const entry={language:'vec',file:'fixture.json',date:raw.metadata.finished_at,demo:false,
    summary_file:'fixture-summary.json',observations_file:'fixture-observations.json',articles_file:'fixture-articles.json'};
  const files={'fixture.json':raw,'fixture-summary.json':summary,'fixture-observations.json':observations,'fixture-articles.json':articleIndex};
  const legacyEntry={...entry};delete legacyEntry.articles_file;
  const legacy=await harness('articles',raw,'?dataset=fixture.json',[legacyEntry],{},files);
  const indexed=await harness('articles',raw,'?dataset=fixture.json',[entry],{},files);
  for(const result of [legacy,indexed])result.nodes['article-more'].listeners.click();
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(indexed.nodes['article-rows'].children.length,50);
  assert(indexed.requests.some(request=>request.url==='data/fixture-articles.json'));
  assert(!indexed.requests.some(request=>['data/fixture.json','data/fixture-observations.json'].includes(request.url)));
  assert(legacy.requests.some(request=>request.url==='data/fixture-observations.json'));
  const rows=result=>result.nodes['article-rows'].children.map(row=>[
    row.children[0].children[0].textContent,...row.children.slice(1).map(cell=>cell.textContent)]);
  const parity=()=>assert.deepEqual(rows(indexed),rows(legacy));
  const change=(id,value)=>{
    for(const result of [legacy,indexed]) {
      const node=result.nodes[id];node.value=value;(node.listeners.input||node.listeners.change)();
    }
    parity();
  };
  parity();
  for(const group of ['all','non-orphans','orphans']) {
    change('article-group',group);
    for(const order of ['title-asc','category-asc','length-desc','length-asc','date-desc','date-asc','candidates-desc','candidates-asc'])
      change('article-sort',order);
  }
  change('article-search','1');change('article-search','');
  change('article-category','scri');change('article-category','');
  change('article-classification','macro');change('article-macro','Culture');
  change('article-classification','topics');change('article-category','Music');
  change('article-classification','categories');change('article-category','');change('article-group','all');
  for(const result of [legacy,indexed])result.nodes['article-more'].listeners.click();
  parity();assert.equal(indexed.nodes['article-rows'].children.length,50);
  for(const id of ['theme-toggle','ui-language-toggle']) {
    for(const result of [legacy,indexed])result.nodes[id].listeners.click();
    parity();
    assert.deepEqual(indexed.nodes.metrics.children.map(node=>node.children[0].textContent),
      legacy.nodes.metrics.children.map(node=>node.children[0].textContent));
  }
  const bytes=document=>Buffer.byteLength(JSON.stringify(document));
  assert(bytes(articleIndex)<bytes(observations)*.5);
  for(const mutate of [
    document=>document.census_page_count++,
    document=>{document.rows.pop();document.census_page_count--;},
    document=>document.metadata.finished_at='2000-01-01',
    document=>document.metadata.measures_rule='obsolete',
    document=>document.format='observations_v1',
    document=>{document.columns.pop();document.rows.forEach(row=>row.pop());},
    document=>document.rows[1][0]=document.rows[0][0]
  ]) {
    const bad=JSON.parse(JSON.stringify(articleIndex));mutate(bad);
    const result=await harness('articles',raw,'?dataset=fixture.json',[entry],{}, {...files,'fixture-articles.json':bad});
    result.nodes['article-more'].listeners.click();await new Promise(resolve=>setImmediate(resolve));
    assert.match(result.nodes['article-info'].textContent,/non valid/);
  }
  const lmo=JSON.parse(fs.readFileSync(path.join(fixtureDir,'lmo-demo-articles.json')));lmo.metadata.demo=false;
  const lmoEntry={language:'lmo',file:'lmo-demo.json',date:lmo.metadata.finished_at,demo:false,articles_file:'lmo-demo-articles.json'};
  const aggregate={metadata:{...raw.metadata,language:'all',kind:'aggregate',languages:['vec','lmo']},pages:[],
    members:[{language:'vec',file:entry.file,page_count:raw.pages.length,metadata:raw.metadata},
      {language:'lmo',file:lmoEntry.file,page_count:lmo.census_page_count,metadata:lmo.metadata}]};
  const pooled=await harness('articles',raw,'?dataset=fixture.json',[entry,lmoEntry],{}, {...files,'lmo-demo-articles.json':lmo});
  pooled.context.articleAggregate=aggregate;
  const result=await vm.runInContext('loadAggregate(articleAggregate,loadVersion,true)',pooled.context);
  assert.equal(result.pages.length,raw.pages.length+lmo.census_page_count);
  assert(pooled.requests.some(request=>request.url==='data/lmo-demo-articles.json'));
  assert(!pooled.requests.some(request=>request.url.endsWith('-observations.json')||request.url==='data/lmo-demo.json'));
  console.log('Article explorer index: '+JSON.stringify({census_pages:raw.pages.length,legacy_bytes:bytes(observations),index_bytes:bytes(articleIndex),identical_global_filters_and_orders:true}));
}
async function checkReconstructedCreatorFallback() {
  const raw=fixture();raw.metadata.creators.status='not_collected';
  raw.metadata.history={status:'collected',matched_pages:raw.pages.length};
  for(const [index,page] of raw.pages.entries()) {
    page.creator_key=null;page.history_creator_key=`vec:history:${index%7}`;
    page.creator_prior_articles=null;page.creator_tenure_days=null;
    page.creator_prior_edits_main_history=index%9;
    page.creator_prior_edits_other_history=index%4;
  }
  const {nodes}=await harness('creators',raw);
  assert.equal(nodes['creator-section'].hidden,false);
  assert.equal(nodes['creator-rows'].children.length,7);
  assert.match(nodes['creator-info'].textContent,/creatori ricostruiti/);
  assert.equal(nodes['creator_prior_articles-chart'].children[0].textContent,'Dati non disponibili.');
  assert(nodes['creator_prior_edits_main-chart'].children[0].children.some(node=>node.tag==='rect'));
  assert(!nodes.source);
  console.log('Reconstructed creators: all six panels remain visible, observed history is labeled and unsupported measures stay missing.');
}
async function checkMissingCreationAge() {
  const raw=fixture();
  raw.metadata.age_rule='unavailable_no_approved_creation_event';
  raw.metadata.historical_age_rule='mediawiki_history_first_observed_revision_v1';
  raw.pages.forEach((page,index)=>{
    page.age_days=9000;page.page_created_at=null;
    page.history_first_revision_age_days=index%5===0?null:index*30;
  });
  const {nodes}=await harness('articles',raw);
  assert.equal(nodes['age-heading'].textContent,'Età dalla creazione ricostruita');
  assert.match(nodes['age-note'].textContent,/page_creation_timestamp/);
  assert.equal(nodes.age.children[0].textContent,'Dati non disponibili.');
  const newArticles=nodes['features-paper'].children.find(node=>
    node.className==='bar-row'&&node.children[0].textContent==='Voci nuove (< mediana)');
  assert.equal(newArticles.children[2].textContent,'— (0/0)');
  console.log('Missing creation age: neither the chart nor New/Old features fall back to first-revision ages.');
}
async function checkReconstructedCreationQualityAndCreators() {
  const raw=fixture();
  raw.metadata.age_rule='mediawiki_history_page_creation_v2';
  raw.metadata.creators.status='not_collected';
  raw.metadata.history={status:'collected',matched_pages:4};
  raw.metadata.historical_age_rule='mediawiki_history_first_observed_revision_v1';
  raw.pages=raw.pages.slice(0,4).map((page,index)=>({...page,
    orphan:index%2===0,page_created_at:'2020-01-01T00:00:00Z',age_days:index===3?null:(index+1)*365.25,
    history_first_revision_age_days:9000,creator_key:null,history_creator_key:'vec:history-user:42',
    creator_account_type:'no_bot_flag',creator_is_bot_now:false,creator_tenure_days:100+index,
    creator_prior_articles:index,creator_prior_edits_main_history:index,creator_prior_edits_other_history:0,
    quality_score:[.1,.3,0,null][index],quality_status:index===3?'revision_mismatch':'scored'}));
  const original=JSON.stringify(raw);
  const articles=await harness('articles',raw);
  const bar=name=>articles.nodes['features-paper'].children.find(node=>node.className==='bar-row'&&node.children[0].textContent===name);
  assert.equal(articles.nodes['age-heading'].textContent,'Età dalla creazione ricostruita');
  assert.match(articles.nodes['age-note'].textContent,/page_creation_timestamp/);
  assert.match(articles.nodes.age.children[0]['aria-label'],/age_days/);
  assert.match(articles.nodes.age.children[1].textContent,/2 anni \(n=2\).*2 anni \(n=1\)/);
  assert.match(articles.nodes.age.children[3].textContent,/0 orfane, 1 non orfane/);
  assert.equal(bar('Voci nuove (< mediana)').children[2].textContent,'100% (1/1)');
  assert.equal(bar('Voci vecchie (≥ mediana)').children[2].textContent,'50% (1/2)');
  assert.equal(bar('Qualità strutturale alta (Johnson V2, ≥ mediana)').children[2].textContent,'50% (1/2)');
  assert.equal(bar('Qualità strutturale bassa (Johnson V2, < mediana)').children[2].textContent,'100% (1/1)');
  assert.match(bar('Qualità strutturale alta (Johnson V2, ≥ mediana)').title,/copertura 3\/4/);
  articles.nodes['ui-language-toggle'].listeners.click();
  assert.equal(articles.nodes['age-heading'].textContent,'Age since reconstructed page creation');
  assert.match(articles.nodes['age-note'].textContent,/page_creation_timestamp/);
  const creators=await harness('creators',raw);
  for(const field of ['creator_prior_articles','creator_tenure_days']) {
    assert(creators.nodes[field+'-chart'].children[0].children.some(node=>node.tag==='rect'));
  }
  const row=creators.nodes['creator-rows'].children[0];
  assert.equal(row.children.length,6);
  assert.equal(row.children[1].textContent,'Senza flag bot');
  assert.equal(row.children[2].textContent,'4');
  assert.equal(creators.nodes['creator-articles-heading'].hidden,true);
  assert.match(creators.nodes['creator_prior_articles-heading'].textContent,/censimento/);
  assert.equal(JSON.stringify(raw),original);
  console.log('Reconstructed creation, structural quality and creators: measured bars, coverage, populated tenure/prior panels, snapshot bot flag, one census column and translations passed.');
}
function checkThemeContrast() {
  const css=fs.readFileSync('web/assets/style.css','utf8');
  const light=css.match(/:root\s*\{([^}]+)\}/)[1],dark=css.match(/:root\[data-theme="dark"\]\s*\{([^}]+)\}/)[1];
  const luminance=hex=>{
    let rgb=hex.slice(1);if(rgb.length===3)rgb=rgb.split('').map(c=>c+c).join('');
    const values=[0,2,4].map(i=>parseInt(rgb.slice(i,i+2),16)/255).map(c=>c<=0.04045?c/12.92:((c+0.055)/1.055)**2.4);
    return values[0]*0.2126+values[1]*0.7152+values[2]*0.0722;
  };
  for(const theme of [light,dark]) {
    const tokens=Object.fromEntries([...theme.matchAll(/--([\w-]+):\s*(#[\da-f]+)/g)].map(m=>[m[1],m[2]]));
    for(const [foreground,background] of [['text','surface'],['muted','surface'],['link','surface'],['surface','teal'],['warning-text','warning-surface']]) {
      const a=luminance(tokens[foreground]),b=luminance(tokens[background]);
      assert((Math.max(a,b)+0.05)/(Math.min(a,b)+0.05)>=4.5,`${foreground}/${background} contrast`);
    }
  }
  console.log('Theme tokens: normal text, muted text, links, buttons and badges meet 4.5:1 contrast in both themes.');
}
(async()=>{try{await checkFreshResources();await checkPage(false);await checkPage(true);await checkPaperProfiles();await checkDeorphanize();await checkCandidateSorting();await checkConnections();await checkFixPrompts();await checkSingleConnectionPrompts();await checkLlmSelection();await checkWikitextPreparation();await checkDatasetSelection();await checkCompactLists();await checkCreatorBots();await checkPreferences();await checkAggregation();await checkCompactTransport();await checkEditorialTransports();await checkTopicExplorer();await checkConnectionCopy();await checkSharedEditorialReviews();await checkPersonalization();await checkProfileTopicShards();await benchmarkPersonalization();await checkCreatorHistory();await checkPreparedSummaries();await checkReconstructedCreatorFallback();await checkMissingCreationAge();await checkReconstructedCreationQualityAndCreators();checkThemeContrast();}finally{fs.rmSync(fixtureDir,{recursive:true});}})().catch(error=>{console.error(error);process.exitCode=1;});
