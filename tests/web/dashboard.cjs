// Dependency-free DOM smoke test. Does not substitute for browser layout review.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const {pathToFileURL}=require('node:url');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
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
  let fetchCount=0;const requests=[];
  const context=vm.createContext({window:{navigator,localStorage,matchMedia:()=>media,addEventListener(){},location:{search}},URLSearchParams,setTimeout,clearTimeout,document,Intl,Map,Set,Math,Number,Option:function(text,value){const n=new Element('option');n.textContent=text;n.value=value;return n;},fetch:async (url,options)=>{requests.push({url,cache:options?.cache});fetchCount++;return {ok:true,headers:{get:()=>preferences.contentLengths?.[url]??null},json:async()=>options?.cache!=='no-store'&&preferences.cachedFiles?.[url]?preferences.cachedFiles[url]:url.endsWith('manifest.json')?(entries||[{language:'vec',file:'demo.json',date:'2026-10-01',demo:true},{language:'vec',file:'unused.json',date:'2026-09-28',demo:false},{language:'vec',file:'fixture.json',date:'2026-09-29',demo:false}]):files?JSON.parse(JSON.stringify(files[url.slice(5)])):raw};}});
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
  assert.equal(nodes.dataset.value,'0');
  assert.equal(nodes.dataset.children.length,2);
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
  assert.deepEqual(bars.map((bar,i)=>Number(bar.height)>0?i:null).filter(i=>i!==null),[0,1,21,39]);
  assert(bars.filter(bar=>Number(bar.height)>0).every(bar=>Number(bar.height)===103));
  assert.match(bars[0]['aria-label'],/\[0, 1\)/);
  assert.match(bars[39]['aria-label'],/\[19, 20\]/);
  assert.match(nodes[chart].children[3].textContent,/1 orfane, 0 non orfane/);
  nodes['chart-type'].value='percent';draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  // One orphan per bin / THREE total orphans, including the missing value.
  assert(Math.abs(Number(bars[0].height)-412/3)<1e-9);
  assert.equal(Number(bars[21].height),206); // one / two non-orphans
  assert.match(bars[0]['aria-label'],/33,3% \(1\/3 pagine del gruppo\)/);
  assert.match(nodes[chart].children[4].textContent,/tutte le 3 orfane e tutte le 2 non orfane/);
  const orphanHeight=bars.slice(0,20).reduce((sum,bar)=>sum+Number(bar.height),0);
  assert(Math.abs(orphanHeight/412-2/3)<1e-9); // missing coverage is not redistributed
  raw.pages=[{orphan:true,fixture:0},{orphan:true,fixture:20},
    ...Array.from({length:20},(_,i)=>({orphan:false,fixture:i<10?0:20}))];
  draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.equal(Number(bars[0].height),206);
  assert.equal(Number(bars[20].height),206); // 1/2 and 10/20 must be equally tall
  assert.match(bars[20]['aria-label'],/50% \(10\/20 pagine del gruppo\)/);
  raw.pages=[{orphan:false,fixture:0},{orphan:false,fixture:20}];draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.equal(bars.length,20); // no invented percentages for an empty group
  assert(bars.every(bar=>Number.isFinite(Number(bar.height))));
  assert.match(nodes[chart].children[4].textContent,/tutte le 0 orfane/);
  nodes['chart-type'].value='count';
  raw.pages=[{orphan:true,fixture:0},{orphan:true,fixture:0},{orphan:false,fixture:0}];
  draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.equal(Number(bars[0].height),206);
  assert.equal(Number(bars[20].height),103);
  nodes['chart-type'].value='cumulative';draw();
  const paths=nodes[chart].children[0].children.filter(n=>n.tag==='path');
  assert.equal(paths.length,2);
  assert(paths.every(path=>path.d.endsWith('V32'))); // original normalization: both reach 100%
  raw.pages=[];draw();
  assert.equal(nodes[chart].children[0].textContent,'Dati non disponibili.');
  raw.metadata.age_rule='first_public_revision';
  await vm.runInContext("load({file:'fixture.json'})",context);
  if(!creatorsPage)assert.match(nodes['age-note'].textContent,/prima revisione pubblica disponibile/);
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
  for(const [id,count] of [['topics-full',64],['topics-paper',4],['features-paper',10]])assert.equal(rows(id).length,count);
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
  assert.equal(value(row('features-paper','Creata da bot')),'— (0/0)');
  assert.equal(value(row('features-paper','Creata da non bot')),'— (0/0)');
  assert.match(row('features-paper','Voci nuove (< mediana)').title,/copertura 6\/7/);
  assert.equal(value(row('features-paper','Qualità alta (Johnson 2021)')),'— (0/0)');
  assert.equal(value(row('features-paper','Biografie di donne')),'— (0/0)');
  assert.match(nodes['features-thresholds'].textContent,/VEC: mediana età 2 giorni/);
  assert.match(nodes['features-thresholds'].textContent,/EN: mediana età 200 giorni/);
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
  assert.equal(value(row('features-paper','Biografie di donne')),'— (0/0)');
  assert.equal(rows('topics-full')[0].children[0].textContent,'Culture · Biography.Women');
  const bars=()=>rows('features-paper').map(node=>[value(node),width(node)]);
  const values=bars();
  nodes['ui-language-toggle'].listeners.click();
  assert.deepEqual(bars(),values);
  assert.match(row('features-paper','New articles (< median)').title,/coverage 6\/7/);
  assert.match(nodes['features-thresholds'].textContent,/median age 200 days/);
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
  // Today's group membership never fills historical bot-at-creation rows.
  raw.pages=[{orphan:true,creator_key:'actor:1',creator_is_bot_now:true},{orphan:true,creator_key:'actor:2',creator_is_bot_now:false}];
  vm.runInContext('paperProfiles()',context);
  assert.equal(value(row('features-paper','Created by a bot')),'— (0/0)');
  assert.equal(value(row('features-paper','Created by a non-bot')),'— (0/0)');
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
  assert.equal(result.nodes.dataset.children.length,2);
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
  const {nodes,context}=await harness('connections',raw,'?dataset=fixture.json&page=1');
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
  const prompt=new URL(findClass(card,'fix-link').href).searchParams.get('text');
  assert(prompt.includes(`Y = ${JSON.stringify(raw.pages[0].title)}`));
  assert(prompt.includes('X = "A & B" [local census dead_end=true; Dead-end page]'));
  assert.match(prompt,/Evaluate only this single proposed link X → Y/);
  assert(!prompt.includes('Z normale'));assert(!prompt.includes('Fonte Z'));
  assert(prompt.includes('https://lmo.wikipedia.org/wiki/Fonte → Y_B https://lmo.wikipedia.org/wiki/Bersaglio'));
  assert(prompt.includes('https://it.wikipedia.org/wiki/Fonte_italiana → Y_B https://it.wikipedia.org/wiki/Destinazione_italiana'));
  assert.match(prompt,/at most one edit \(one table row\)/);
  assert(!prompt.includes('up to 5'));
  const otherPrompt=new URL(findClass(nodes['connection-candidates'].children[1],'fix-link').href).searchParams.get('text');
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
async function checkFixPrompts() {
  const raw=fixture();raw.metadata.demo=false;
  const page={page_id:1,title:'Y à & # <img>',orphan:true,category:'Tema',creator_key:'private-actor'};
  raw.pages=[page];
  const candidates=Array.from({length:40},(_,i)=>({page_id:i+2,title:`Fonte ${String(i).padStart(2,'0')}`,dead_end:i>=35,evidence:[{language:'lmo',source_title:`Reference ${i}`,target_title:'Target B'}]}));
  raw.link_candidates={1:{status:'compared',candidate_count:40,candidates}};
  const originalData=JSON.stringify(raw);
  const result=await harness('deorphanize',raw),{nodes,context,getFetchCount}=result;
  const card=()=>nodes['candidate-cards'].children[0];
  const fix=()=>findClass(card(),'fix-link');
  const prompt=()=>new URL(fix().href).searchParams.get('text');
  assert.equal(titleLink(card()).href,'https://vec.wikipedia.org/wiki/Y_%C3%A0_%26_%23_%3Cimg%3E');
  assert.equal(titleLink(card()).textContent,page.title);
  assert.equal(fix().textContent,'Fix this');
  const url=new URL(fix().href);
  assert.equal(url.origin,'https://chat.qwen.ai');assert.equal(url.pathname,'/');
  assert.equal(url.searchParams.get('inputFeature'),'search');
  assert.deepEqual([...url.searchParams.keys()],['inputFeature','text']); // prefill, never automatic submission
  assert.equal(fix().target,'_blank');assert.equal(fix().rel,'noopener noreferrer');
  assert(fix().href.length<=7500);
  let text=prompt();
  assert.match(text,/X → Y/);assert.match(text,/First read Y and each local X, then both reference pages X_B and Y_B/);
  assert.match(text,/Reply in Italian/);assert.match(text,/propose up to 5 distinct relevant edits/);
  assert.match(text,/always preferring suitable Dead-end pages/);
  assert.match(text,/unique, exact existing text string in local X source/);assert.match(text,/copyable Wikipedia wikitext/);
  assert.match(text,/exactly ADD LINK/);assert.match(text,/Markdown table with exactly these four columns/);
  assert.match(text,/Link pagina lingua da cambiare \| Link pagina lingua di provenienza \| Stringa da cercare con Ctrl\+F \| Stringa da inserire \/ ADD LINK/);
  assert.match(text,/neutrality, verifiability, no original research/);
  // Editorial evidence must stay on Wikipedia, with verified source anchors,
  // explicit census flags, relevant relationships and copyable edit instructions.
  assert.match(text,/Use ONLY the supplied Wikipedia pages/);
  assert.match(text,/Do not search the web, open external citation links/);
  assert.match(text,/Transfer only facts explicitly present in the reference Wikipedia pages/);
  assert.match(text,/never invent sources/);assert(!text.includes("checking the reference article's original citations"));
  assert.match(text,/Read local source wikitext for exact anchors and syntax/);
  assert.match(text,/if raw access fails, try Wikipedia's source view\/API on the same wiki/);
  assert.match(text,/never guess text from rendered output/);
  assert.match(text,/Truncated\/error\/login pages are unreadable/);
  assert.match(text,/Orphan tags\/name absence do not prove absent links/);
  assert.match(text,/including piped\/redirect links/);
  assert.match(text,/Verify actual href\/redirect targets/);
  assert.match(text,/bold text or navbox labels are not proof of links/);
  assert.match(text,/If unverified, existing-link status is unknown/);
  assert.match(text,/Use ONLY supplied local census dead_end flags/);
  assert.match(text,/unknown stays unknown/);assert.match(text,/Never infer them from rendered text/);
  assert.match(text,/shared navbox\/category, profession or team alone does not justify new prose/);
  assert.match(text,/inclusion criteria, date, place and ordering/);
  assert.match(text,/birthplace, residence and place of death are not interchangeable/);
  assert.match(text,/BEFORE, AFTER or REPLACE outside the copyable code/);
  assert.match(text,/complete replacement text/);
  assert.match(text,/numbered fenced wikitext block below the table with real newlines/);
  assert.match(text,/exact unlinked words referring to Y/);
  assert.match(text,/explicit Markdown link to local X/);
  assert.match(text,/row-numbered evidence notes/);assert.match(text,/No deliberation/);
  assert.match(text,/Fewer than five rows, including none, is acceptable/);
  assert(text.includes('Target source view: https://vec.wikipedia.org/w/index.php?curid=1&action=raw'));
  const referenceViews=text.split('Reference source views:\n')[1].split('\n');
  assert.equal(referenceViews.length,6); // shared Y_B raw URL appears once, not once per candidate
  assert.equal(new Set(referenceViews).size,6);
  assert(referenceViews.includes('https://lmo.wikipedia.org/w/index.php?title=Target_B&action=raw'));
  assert.match(text,/Do not edit Wikipedia or submit anything/);assert(!text.includes('private-actor'));
  let entries=text.split('Candidate source pages X:\n')[1].split('\n').filter(line=>/^\d+\. Local X/.test(line));
  assert.equal(entries.length,5);assert.match(entries[0],/Fonte 35.*Dead-end page/);
  assert(entries.every(line=>line.includes('Dead-end page')));
  assert.match(text,/5 candidate source pages \(selected from 40\)/);
  for(let i=35;i<40;i++) {
    assert(text.includes(`https://vec.wikipedia.org/wiki/Fonte_${i}`));
    assert(text.includes(`https://vec.wikipedia.org/w/index.php?curid=${i+2}&action=raw`));
    assert(text.includes(`https://lmo.wikipedia.org/wiki/Reference_${i} → Y_B https://lmo.wikipedia.org/wiki/Target_B`));
    assert(referenceViews.includes(`https://lmo.wikipedia.org/w/index.php?title=Reference_${i}&action=raw`));
  }
  for(const [provider,parameter] of [['kimi','prefill_prompt'],['mistral','q'],['deepseek','q'],['qwen','text']]) {
    nodes['llm-provider'].value=provider;nodes['llm-provider'].listeners.change();
    assert.equal(new URL(fix().href).searchParams.get(parameter),text); // Unicode, &, #, full five candidates and every supplied reference URL survive encoding
    assert(!fix().listeners.click);assert.equal(result.copied.length,0);
  }
  const fetches=getFetchCount();
  nodes['ui-language-toggle'].listeners.click();
  text=prompt();assert.match(text,/Reply in English/);
  assert.match(text,/Page link \(language to edit\) \| Page link \(reference language\)/);
  assert.match(text,/article language \(vec\)/);assert(text.includes(page.title));
  assert.equal(titleLink(card()).textContent,page.title);assert.equal(getFetchCount(),fetches);
  assert.equal(JSON.stringify(raw),originalData); // all source URLs are derived, never stored in the graph
  nodes['ui-language-toggle'].listeners.click();
  raw.link_candidates[1]={candidate_count:2,candidates:[candidates[0],candidates[35],candidates[35],{page_id:1,title:page.title},{page_id:0,title:'Invalid'},{page_id:99,title:null}]};
  vm.runInContext('candidateCards()',context);
  text=prompt();entries=text.split('Candidate source pages X:\n')[1].split('\n').filter(line=>/^\d+\. Local X/.test(line));
  assert.equal(entries.length,2);assert.match(entries[0],/Dead-end page/);
  assert(!entries[1].includes('Dead-end page'));
  assert.match(entries[1],/dead_end=false/);
  assert.match(text,/Do not add other candidates or force links/);assert.match(text,/2 candidate source pages \(selected from 2\)/);
  const unknown={page_id:90,title:'Unknown à & #',evidence:[{language:'lmo',source_title:'Reference unknown',target_title:'Target B'}]};
  const before=JSON.stringify(unknown);
  raw.link_candidates[1]={candidate_count:3,candidates:[unknown,candidates[0],candidates[35]]};
  vm.runInContext('candidateCards()',context);
  text=prompt();entries=text.split('Candidate source pages X:\n')[1].split('\n').filter(line=>/^\d+\. Local X/.test(line));
  assert.equal(entries.length,3);assert.match(entries[0],/dead_end=true/);
  assert.match(entries[1],/dead_end=false/);assert.match(entries[2],/dead_end=unknown/);
  assert(text.includes('https://vec.wikipedia.org/w/index.php?curid=90&action=raw'));
  assert(text.includes('https://vec.wikipedia.org/wiki/Unknown_%C3%A0_%26_%23'));
  assert.equal(JSON.stringify(unknown),before); // unknown remains missing in the research data
  // Reference page IDs belong to their own wiki, never the local source/target.
  unknown.evidence[0].source_page_id=501;unknown.evidence[0].target_page_id=601;
  const withReferenceIds=JSON.stringify(raw);
  vm.runInContext('candidateCards()',context);
  text=prompt();
  assert(text.includes('https://lmo.wikipedia.org/w/index.php?curid=501&action=raw'));
  assert(text.includes('https://lmo.wikipedia.org/w/index.php?curid=601&action=raw'));
  assert(!text.includes('https://lmo.wikipedia.org/w/index.php?curid=90&action=raw'));
  assert.equal(JSON.stringify(raw),withReferenceIds);
  // Invalid IDs cannot inject query parameters; derive a title URL instead.
  unknown.evidence[0].source_page_id='501&action=submit';unknown.evidence[0].target_page_id=-1;
  unknown.evidence[0].source_title='Reference à & # <img>';
  vm.runInContext('candidateCards()',context);
  const sources=prompt().split('Reference source views:\n')[1].split('\n').map(value=>new URL(value));
  const fallback=sources.find(value=>value.searchParams.get('title')==='Reference_à_&_#_<img>');
  assert(fallback);assert.equal(fallback.searchParams.get('action'),'raw');
  assert(!fallback.searchParams.has('curid'));
  assert(sources.every(value=>value.protocol==='https:'&&value.hostname==='lmo.wikipedia.org'&&value.searchParams.get('action')==='raw'));
  raw.metadata.language='roa-tara';vm.runInContext('candidateCards()',context);
  assert.match(titleLink(card()).href,/roa-tara\.wikipedia/);
  assert.match(prompt(),/article language \(roa-tara\)/);
  assert(prompt().includes('https://roa-tara.wikipedia.org/wiki/Fonte_35'));
  assert(prompt().includes('https://roa-tara.wikipedia.org/w/index.php?curid=37&action=raw'));
  // Realistic title lengths exercise provider URL limits; IDs/flags are synthetic.
  for(const [language,title,names,referenceNames] of [
    ['vec','Graham Hill',['Ayrton Senna','Fernando Alonso','Jackie Stewart','Lewis Hamilton','Mario Andretti']],
    ['vec','Hebe Camargo',['1929','2012','29 de setenbre','8 de marso','San Poło del Braxil'],['1929','2012','29 09','08 03','São Paulo']],
    ['roa-tara','Tapogliano',['Aiello del Friuli','Amaro','Ampezzo (UD)','Aquileia','Arta Terme'],['Aiello del Friuli','Amaro','Ampezzo','Aquileia','Arta Terme']]
  ]) {
    raw.metadata.language=language;page.title=title;
    raw.link_candidates[1]={candidate_count:5,candidates:names.map((name,i)=>({page_id:i+2,title:name,dead_end:true,evidence:[{language:'lmo',source_title:(referenceNames||names)[i],source_page_id:100+i,target_title:title,target_page_id:200}]}))};
    const unchanged=JSON.stringify(raw);
    for(const [provider,parameter] of [['qwen','text'],['kimi','prefill_prompt'],['mistral','q'],['deepseek','q']]) {
      nodes['llm-provider'].value=provider;nodes['llm-provider'].listeners.change();
      const url=new URL(fix().href),request=url.searchParams.get(parameter);
      assert(request);assert(fix().href.length<=7500);assert(!fix().listeners.click);
      assert(request.includes('Reference source views:\n'));
      assert.equal(request.split('Reference source views:\n')[1].split('\n').length,6);
      assert.match(request,/5 candidate source pages \(selected from 5\)/);
    }
    assert.equal(JSON.stringify(raw),unchanged);
  }
  nodes['llm-provider'].value='qwen';nodes['llm-provider'].listeners.change();
  for(const record of [undefined,{candidate_count:null,candidates},{candidate_count:0,candidates:[]},{candidate_count:1,candidates:[{page_id:2,title:'No evidence',evidence:[{language:'lmo.evil',source_title:'X',target_title:'Y'}]}]}]) {
    raw.link_candidates[1]=record;vm.runInContext('candidateCards()',context);
    assert.equal(fix().tag,'span');assert.equal(fix()['aria-disabled'],'true');assert(!fix().href);
  }
  raw.link_candidates[1]={candidate_count:40,candidates};raw.metadata.demo=true;
  vm.runInContext('candidateCards()',context);
  assert.equal(fix().textContent,'Fix this · DEMO');assert(!fix().href);assert(!titleLink(card()).href);
  raw.metadata.demo=false;page.title='à'.repeat(1000);vm.runInContext('candidateCards()',context);
  assert.equal(fix().href,'https://chat.qwen.ai/'); // long prompts use the complete copy fallback, never drop candidates
  fix().listeners.click();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(nodes['llm-prompt'].open,true);assert.equal(result.copied.length,1);
  assert.equal(result.copied[0],nodes['llm-prompt-text'].value);
  assert.match(nodes['llm-prompt-text'].value,/5 candidate source pages/);
  assert(nodes['llm-prompt-text'].value.includes(page.title));
  nodes['llm-prompt-close'].listeners.click();assert.equal(nodes['llm-prompt'].open,false);
  assert.equal(getFetchCount(),fetches); // external services are contacted only when the user follows the link
  console.log('Fix this: Wikipedia-only instructions, clean edits, exact source anchors, derived/deduplicated raw URLs, unchanged graphs, explicit dead-end flags, five candidates, provider prefilling, generic wiki codes and full-copy fallback passed.');
}
async function checkSingleConnectionPrompts() {
  const raw=fixture();raw.metadata.demo=false;raw.metadata.language='roa-tara';
  const page={page_id:1,title:'Orphan à & #',orphan:true,category:'Tema'};
  const chosen={page_id:90,title:'Chosen à & #',dead_end:false,evidence:['lmo','it','fur'].map((language,i)=>({language,source_title:`Ref ${language}`,source_page_id:100+i,target_title:`Target ${language}`,target_page_id:200+i}))};
  const others=Array.from({length:6},(_,i)=>({page_id:i+2,title:`Other ${i}`,dead_end:true,evidence:[{language:'lmo',source_title:`Unrelated ${i}`,target_title:'Other target'}]}));
  raw.pages=[page];raw.link_candidates={1:{candidate_count:7,candidates:[...others,chosen]}};
  const unchanged=JSON.stringify(raw);
  const result=await harness('connections',raw,'?dataset=fixture.json&page=1'),{nodes,context,getFetchCount}=result;
  const card=()=>nodes['connection-candidates'].children.find(row=>titleLink(row).textContent===chosen.title);
  const fix=()=>findClass(card(),'fix-link');
  const fetches=getFetchCount();
  for(const [provider,parameter] of [['qwen','text'],['kimi','prefill_prompt'],['mistral','q'],['deepseek','q']]) {
    nodes['llm-provider'].value=provider;nodes['llm-provider'].listeners.change();
    const prompt=new URL(fix().href).searchParams.get(parameter);
    assert(prompt);assert(fix().href.length<=7500);assert(!fix().listeners.click);
    assert.match(prompt,/Evaluate only this single proposed link X → Y/);
    assert.match(prompt,/Do not evaluate, suggest or edit other local source pages/);
    assert.match(prompt,/at most one edit \(one table row\)/);
    assert.match(prompt,/include every supplied reference source URL in that same row/);
    assert.match(prompt,/local census dead_end=false/);
    assert(!prompt.includes('up to 5'));assert(!prompt.includes('Other '));assert(!prompt.includes('Unrelated'));
    assert.equal((prompt.match(/^\d+\. Local X/gm)||[]).length,1);
    assert.equal((prompt.match(/Observed reference edge/g)||[]).length,3);
    assert(prompt.includes('https://roa-tara.wikipedia.org/wiki/Chosen_%C3%A0_%26_%23'));
    for(const edge of chosen.evidence) {
      assert(prompt.includes(`https://${edge.language}.wikipedia.org/wiki/Ref_${edge.language} → Y_B https://${edge.language}.wikipedia.org/wiki/Target_${edge.language}`));
      assert(prompt.includes(`https://${edge.language}.wikipedia.org/w/index.php?curid=${edge.source_page_id}&action=raw`));
      assert(prompt.includes(`https://${edge.language}.wikipedia.org/w/index.php?curid=${edge.target_page_id}&action=raw`));
    }
  }
  nodes['ui-language-toggle'].listeners.click();
  let prompt=new URL(fix().href).searchParams.get('q');
  assert.match(prompt,/Reply in English/);assert.match(prompt,/Page link \(language to edit\)/);
  assert.match(prompt,/article language \(roa-tara\)/);assert.equal(JSON.stringify(raw),unchanged);
  assert.equal(getFetchCount(),fetches);assert.equal(result.copied.length,0);
  // More than five reference languages are evidence for ONE local candidate, never a shortlist.
  chosen.evidence.push(...['nap','sc','en','fr'].map((language,i)=>({language,source_title:`Ref ${language}`,source_page_id:300+i,target_title:`Target ${language}`,target_page_id:400+i})));
  const fullData=JSON.stringify(raw);
  nodes['llm-provider'].value='qwen';nodes['llm-provider'].listeners.change();
  prompt=new URL(fix().href).searchParams.get('text');
  if(!prompt) {
    fix().listeners.click();await new Promise(resolve=>setImmediate(resolve));
    prompt=nodes['llm-prompt-text'].value;
    assert.equal(result.copied[0],prompt);
    nodes['llm-prompt-close'].listeners.click();
  }
  assert.equal((prompt.match(/Observed reference edge/g)||[]).length,7);
  assert.equal(prompt.split('Reference source views:\n')[1].split('\n').length,14);
  assert(!prompt.includes('Other '));assert(!prompt.includes('up to 5'));
  assert.equal(JSON.stringify(raw),fullData);assert.equal(getFetchCount(),fetches);
  // Invalid/missing evidence must disable only its own card, without borrowing another candidate.
  for(const evidence of [[],[{language:'lmo.evil',source_title:'Bad',target_title:'Bad'}]]) {
    chosen.evidence=evidence;vm.runInContext('renderConnections()',context);
    assert.equal(fix().tag,'span');assert.equal(fix()['aria-disabled'],'true');
    assert(nodes['connection-candidates'].children.some(row=>findClass(row,'fix-link').href));
  }
  console.log('Single connection prompts: isolated candidate beyond top five, every reference language, four providers, translations, unchanged data, complete oversized fallback and per-card disabled states passed.');
}
async function checkLlmSelection() {
  const raw=fixture();raw.metadata.demo=false;
  raw.pages=Array.from({length:30},(_,i)=>({page_id:i+1,title:`Target ${i}`,orphan:true,category:'Tema'}));
  const record={candidate_count:1,candidates:[{page_id:99,title:'Local source',dead_end:true,evidence:[{language:'lmo',source_title:'Reference source',target_title:'Reference target'}]}]};
  raw.link_candidates=Object.fromEntries(raw.pages.map(page=>[page.page_id,record]));
  const result=await harness('deorphanize',raw),{nodes,copied,storage,getFetchCount,html}=result;
  assert.match(html,/<div class="toolbar">[\s\S]*id="dataset"[\s\S]*id="llm-provider"[\s\S]*<\/div>/);
  assert.deepEqual(nodes['llm-provider'].children.map(option=>option.textContent),['Qwen','Kimi','Mistral','DeepSeek']);
  assert.equal(nodes['llm-provider'].value,'qwen');assert.equal(copied.length,0);
  nodes['candidate-search'].value='Target';nodes['candidate-search'].listeners.input();
  nodes['candidate-more'].listeners.click();assert.equal(nodes['candidate-cards'].children.length,30);
  const fetches=getFetchCount();
  nodes['llm-provider'].value='kimi';nodes['llm-provider'].listeners.change();
  const kimi=findClass(nodes['candidate-cards'].children[0],'fix-link'),kimiUrl=new URL(kimi.href);
  assert.equal(kimiUrl.origin,'https://www.kimi.ai');
  assert.equal(kimiUrl.searchParams.get('send_immediately'),'false');assert.equal(kimiUrl.searchParams.get('force_search'),'true');
  assert.match(kimiUrl.searchParams.get('prefill_prompt'),/Reply in Italian/);
  assert.deepEqual([...kimiUrl.searchParams.keys()],['send_immediately','force_search','prefill_prompt']);
  assert(!kimi.listeners.click);assert.equal(copied.length,0);
  assert.equal(nodes['candidate-cards'].children.length,30);
  for(const [provider,url] of [['mistral','https://chat.mistral.ai/chat'],['deepseek','https://chat.deepseek.com/']]) {
    const before=copied.length;
    nodes['llm-provider'].value=provider;nodes['llm-provider'].listeners.change();
    assert.equal(nodes['candidate-cards'].children.length,30);assert.equal(nodes['candidate-search'].value,'Target');
    assert.equal(nodes.dataset.value,'0');assert.equal(storage.get('orphanwiki-llm'),provider);
    assert.equal(copied.length,before); // no clipboard or external request on selection/rendering
    const fix=findClass(nodes['candidate-cards'].children[0],'fix-link');
    const actual=new URL(fix.href),expected=new URL(url);
    assert.equal(actual.origin,expected.origin);assert.equal(actual.pathname,expected.pathname);
    assert.deepEqual([...actual.searchParams.keys()],['q']);assert.equal(fix.target,'_blank');
    const prompt=actual.searchParams.get('q');
    assert.match(prompt,/Reply in Italian/);assert.match(prompt,/1 candidate source pages/);
    assert(prompt.includes('https://vec.wikipedia.org/wiki/Local_source'));
    assert(prompt.includes('https://lmo.wikipedia.org/wiki/Reference_source → Y_B https://lmo.wikipedia.org/wiki/Reference_target'));
    assert(!fix.listeners.click);assert.equal(copied.length,before);
    assert(!nodes['llm-prompt'].open);
  }
  assert.equal(getFetchCount(),fetches);
  let next=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{storage});
  assert.equal(next.nodes['llm-provider'].value,'deepseek');
  let link=findClass(next.nodes['connection-candidates'].children[0],'fix-link');
  assert.equal(new URL(link.href).origin,'https://chat.deepseek.com');
  assert.match(new URL(link.href).searchParams.get('q'),/Reply in Italian/);assert(!link.listeners.click);
  // Full-copy fallback is reserved for oversized URLs on every provider.
  const long=JSON.parse(JSON.stringify(raw));long.pages[0].title='à'.repeat(1000);
  for(const preferences of [{clipboardUnavailable:true},{clipboardFailure:true},{blockStorage:true}]) {
    next=await harness('deorphanize',long,undefined,null,preferences);
    next.nodes['llm-provider'].value='mistral';next.nodes['llm-provider'].listeners.change();
    link=descendants(next.nodes['candidate-cards']).find(node=>node.className==='fix-link'&&node.listeners.click);
    assert(link);assert.equal(link.href,'https://chat.mistral.ai/chat');link.listeners.click();
    await new Promise(resolve=>setImmediate(resolve));
    assert.equal(next.nodes['llm-prompt'].open,true);
    assert(next.nodes['llm-prompt-text'].value.includes('https://lmo.wikipedia.org/wiki/Reference_source'));
    if(!preferences.blockStorage)assert.match(next.nodes['llm-prompt-status'].textContent,/copialo manualmente/);
  }
  next=await harness('deorphanize',raw,undefined,null,{storage:new Map([['orphanwiki-llm','javascript:evil']])});
  assert.equal(next.nodes['llm-provider'].value,'qwen');
  next.nodes['llm-provider'].value='__proto__';next.nodes['llm-provider'].listeners.change();
  assert.equal(next.nodes['llm-provider'].value,'qwen');
  assert.equal(new URL(findClass(next.nodes['candidate-cards'].children[0],'fix-link').href).hostname,'chat.qwen.ai');
  next.nodes['llm-provider'].value='mistral';next.nodes['llm-provider'].listeners.change();
  next.nodes['ui-language-toggle'].listeners.click();
  link=findClass(next.nodes['candidate-cards'].children[0],'fix-link');
  assert.match(new URL(link.href).searchParams.get('q'),/Reply in English/);
  assert(!link.listeners.click);assert.equal(next.copied.length,0);
  next=await harness('deorphanize',long,undefined,null,{storage:new Map([['orphanwiki-language','en'],['orphanwiki-llm','mistral']])});
  link=descendants(next.nodes['candidate-cards']).find(node=>node.className==='fix-link'&&node.listeners.click);
  link.listeners.click();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(next.nodes['llm-prompt-title'].textContent,'Prompt for Mistral');assert.match(next.nodes['llm-prompt-text'].value,/Reply in English/);
  assert.match(next.nodes['llm-prompt-status'].textContent,/Prompt copied/);
  await next.nodes.dataset.listeners.change();
  assert.equal(next.nodes['llm-prompt'].open,false);assert.equal(next.nodes['llm-provider'].value,'mistral');
  console.log('LLM selection: all four native prefill URLs, no normal clipboard/dialog, validated preference, unchanged pagination/filters, oversized URL fallback, clipboard failures and localized dialog passed.');
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
    assert.match(nodes.source.textContent,/Collection period:/);
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
      assert.match(nodes.age.children[4].textContent,/Denominators: all/);
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
    assert.equal(result.requests.length,4);
    assert(result.requests.every(request=>request.cache==='no-store'));
    assert(!nodes.source.textContent.includes('Ogni versione linguistica'));
    assert.equal(vm.runInContext('data.metadata.creators.collected_languages',context),2);
    assert.equal(vm.runInContext('data.pages.length',context),4);
    assert.equal(vm.runInContext('data.pages.filter(p=>p.orphan).length',context),3);
    assert.equal(vm.runInContext('new Set(data.pages.map(creatorKey)).size',context),2);
    assert.equal(vm.runInContext("label(data.pages[0])",context),'VEC · Shared category');
    if(page==='articles') {
      assert.match(nodes['age-note'].textContent,/criteri diversi/);
      assert.equal(nodes['article-rows'].children.length,3);
      assert.match(nodes['article-rows'].children[0].children[0].children[0].href,/vec\.wikipedia/);
      assert.equal(nodes.download.href,'data/all.json');
      assert.equal(nodes.dataset.children[0].textContent,'Tutte le lingue · 2026-09-29');
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
      const prompt=new URL(findClass(nodes['connection-candidates'].children[0],'fix-link').href).searchParams.get('text');
      assert(prompt.includes('https://vec.wikipedia.org/wiki/'));assert(!prompt.includes('https://all.wikipedia.org'));
    }
    nodes['ui-language-toggle'].listeners.click();
    if(nodes.dataset)assert.equal(nodes.dataset.children[0].textContent,'All languages · 2026-09-29');
    assert(!nodes.source.textContent.includes('Creators collected'));
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
    assert.match(result.nodes.source.textContent,/Lingue aggregate: VEC, LMO/);
    const full=await harness(page,index,'?dataset=all.json',entries.map(({observations_file,...entry})=>entry),{},files);
    for(const id of page==='articles'?['topics-full','topics-paper','features-paper','categories']:['origins','creator-bots']) {
      const values=nodes=>nodes[id].children.filter(node=>node.className==='bar-row').map(node=>[node.children[0].textContent,node.children[1].children[0].style.width,node.children[2].textContent]);
      assert.deepEqual(values(result.nodes),values(full.nodes));
    }
    assert.equal(result.nodes[page==='articles'?'article-info':'creator-info'].textContent,full.nodes[page==='articles'?'article-info':'creator-info'].textContent);
  }
  const deorphanize=await harness('deorphanize',a,'?dataset=vec.json',entries,{},files);
  assert.deepEqual(deorphanize.requests.map(r=>r.url),['data/manifest.json','data/vec.json']);
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
  nodes['article-category'].value='STEM';nodes['article-category'].listeners.input();
  assert.equal(nodes['article-rows'].children.length,16);
  assert(nodes['article-rows'].children.every(row=>row.children[1].textContent==='STEM'));
  nodes['article-classification'].value='topics';nodes['article-classification'].listeners.change();
  assert.equal(nodes['article-category'].value,'');
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
  assert.equal(all.requests.length,13); // manifest, index, eleven compact card files
  assert(all.requests.slice(2).every(request=>request.url.endsWith('-cards.json')));
  assert(!all.nodes.status.textContent.includes('Impossibile'));
  for(const card of all.nodes['candidate-cards'].children)assert.equal(findClass(card,'candidate-badge').textContent,'70');
  const pms=all.nodes['candidate-cards'].children.find(card=>titleLink(card).textContent==='Target pms');
  assert.match(findClass(pms,'connect-button').href,/dataset=all.json&page=1&language=pms/);
  const normal=await harness('deorphanize',index,'?dataset=pms.json',entries,{},files);
  assert.deepEqual(normal.requests.map(request=>request.url),['data/manifest.json','data/pms-cards.json']);
  const overviewPrompt=new URL(findClass(normal.nodes['candidate-cards'].children[0],'fix-link').href).searchParams.get('text');
  const detail=await harness('connections',index,'?dataset=all.json&page=1&language=pms',entries,{},files);
  assert.equal(detail.document.body.dataset.loading,'false',detail.nodes.status.textContent);
  assert.deepEqual(detail.requests.map(request=>request.url),['data/manifest.json','data/all.json','data/pms-connections-v1/1.json']);
  assert.equal(detail.nodes['connection-candidates'].children.length,50);
  detail.nodes['connection-more'].listeners.click();
  assert.equal(detail.nodes['connection-candidates'].children.length,70);
  assert.equal(descendants(detail.nodes['connection-candidates']).filter(node=>node.className==='connection-evidence').length,140);
  const last=detail.nodes['connection-candidates'].children.at(-1);
  const prompt=new URL(findClass(last,'fix-link').href).searchParams.get('text');
  assert(prompt.includes('Source 69'));assert(!prompt.includes('Source 00'));
  assert(prompt.includes('Italian_69'));assert(prompt.includes('Reference_69'));
  assert.match(prompt,/Evaluate only this single proposed link/);
  await detail.nodes['connection-copy-prompt'].listeners.click();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(detail.copied[0],overviewPrompt);
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
  for(const preferences of [{},{clipboardUnavailable:true},{clipboardFailure:true}]) {
    const result=await harness('connections',raw,`?dataset=fixture.json&page=${key}`,null,preferences);
    const {nodes}=result;
    assert.equal(result.copied.length,0);assert.equal(nodes['llm-prompt'].open,undefined);
    assert.equal(nodes['connection-copy-prompt'].disabled,false);
    await nodes['connection-copy-prompt'].listeners.click();await new Promise(resolve=>setImmediate(resolve));
    const prompt=result.copied[0]||nodes['llm-prompt-text'].value;
    assert(prompt.includes('Local 6'));assert.equal((prompt.match(/Local X =/g)||[]).length,5);
    assert(prompt.includes('Wikipedia'));assert(!prompt.includes('creator_key'));
    if(preferences.clipboardUnavailable||preferences.clipboardFailure){assert.equal(nodes['llm-prompt'].open,true);assert.match(nodes['llm-prompt-status'].textContent,/manualmente/);}
    else {assert.equal(nodes['llm-prompt'].open,undefined);assert.equal(nodes['connection-copy-prompt'].textContent,'Prompt copiato');}
    const single=new URL(findClass(nodes['connection-candidates'].children[0],'fix-link').href).searchParams.get('text');
    assert.match(single,/Evaluate only this single proposed link/);
    if(!preferences.clipboardUnavailable&&!preferences.clipboardFailure) {
      nodes['ui-language-toggle'].listeners.click();
      assert.equal(result.copied.length,1);
      await nodes['connection-copy-prompt'].listeners.click();await new Promise(resolve=>setImmediate(resolve));
      assert(result.copied[1].includes('Reply in English'));
    }
  }
  const large=JSON.parse(JSON.stringify(raw));large.pages.find(p=>p.page_id===target.page_id).title='Long '+ 'ł'.repeat(5000);
  const oversized=await harness('connections',large,`?dataset=fixture.json&page=${key}`);
  await oversized.nodes['connection-copy-prompt'].listeners.click();await new Promise(resolve=>setImmediate(resolve));
  assert(oversized.copied[0].length>7500);
  assert.equal(oversized.nodes['llm-prompt'].open,undefined);
  raw.link_candidates[key].candidate_count=0;raw.link_candidates[key].candidates=[];
  const disabled=await harness('connections',raw,`?dataset=fixture.json&page=${key}`);
  assert.equal(disabled.nodes['connection-copy-prompt'].disabled,true);
  await disabled.nodes['connection-copy-prompt'].listeners.click();assert.equal(disabled.copied.length,0);
  console.log('Connection copy: explicit click only, five priority candidates, unchanged single-card scope, direct clipboard, manual fallback and disabled empty state passed.');
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
(async()=>{try{await checkFreshResources();await checkPage(false);await checkPage(true);await checkPaperProfiles();await checkDeorphanize();await checkCandidateSorting();await checkConnections();await checkFixPrompts();await checkSingleConnectionPrompts();await checkLlmSelection();await checkDatasetSelection();await checkCompactLists();await checkCreatorBots();await checkPreferences();await checkAggregation();await checkCompactTransport();await checkEditorialTransports();await checkTopicExplorer();await checkConnectionCopy();checkThemeContrast();}finally{fs.rmSync(fixtureDir,{recursive:true});}})().catch(error=>{console.error(error);process.exitCode=1;});
