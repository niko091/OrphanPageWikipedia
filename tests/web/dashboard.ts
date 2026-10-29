import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {test} from 'node:test';
import {pathToFileURL} from 'node:url';
import type {CandidateRecord,Dataset,ManifestEntry,PromptRequest} from '../../web/src/types.js';
import {prepareSite,record,root,title} from './fixture.ts';

// Exercise production ES modules directly. Real layout, navigation, SQL filtering
// and keyboard operation are covered by the database-only browser suite.
class Element {
  children: Element[]=[];
  dataset: Record<string,string>={};
  style: Record<string,string>={};
  value='';textContent='';className='';href='';hidden=false;disabled=false;checked=false;open=false;focused=false;
  attributes=new Map<string,string>();
  clientWidth=900;
  listeners=new Map<string,()=>unknown>();
  append(...nodes: Element[]): void {this.children.push(...nodes);}
  replaceChildren(...nodes: Element[]): void {this.children=nodes;}
  setAttribute(key: string,value: string): void {this.attributes.set(key,value);}
  removeAttribute(key: string): void {this.attributes.delete(key);}
  addEventListener(key: string,listener: ()=>unknown): void {this.listeners.set(key,listener);}
  showModal(): void {this.open=true;}
  close(): void {this.open=false;}
  focus(): void {this.focused=true;}
}
const nodes=new Map<string,Element>();
function node(id: string): Element {let value=nodes.get(id);if(!value){value=new Element();nodes.set(id,value);}return value;}
const storage=new Map<string,string>(),copied:string[]=[];
const location={search:'?page=1'};
Object.assign(globalThis,{
  window:{location,localStorage:{getItem:(key:string)=>storage.get(key)??null,setItem:(key:string,value:string)=>storage.set(key,value)},
    navigator:{clipboard:{writeText:async(value:string)=>{copied.push(value);}}}},
  document:{body:{dataset:{page:'connections'}},getElementById:node,createElement:()=>new Element(),createElementNS:()=>new Element()},
});
const directory=fs.mkdtempSync(path.join(os.tmpdir(),'orphanwiki-modules-'));
process.on('exit',()=>fs.rmSync(directory,{recursive:true,force:true}));
const fixture=prepareSite(directory),dataRoot=path.join(fixture.web,'data');
const dataset=fixture.datasets.find(wiki=>wiki.metadata.language==='vec')!;
const entry: ManifestEntry={...fixture.manifest.find(entry=>entry.file==='fixture.json')!,storage:'relational_census_v1'};
assert(entry.summary_file&&entry.profile_topics_dir);
const moduleURL=(name:string)=>{
  const filename=path.join(root,'web/assets',name+'.js');
  return pathToFileURL(filename).href+'?v='+createHash('sha256').update(fs.readFileSync(filename)).digest('hex').slice(0,12);
};
const datasets=await import(moduleURL('datasets')) as typeof import('../../web/src/datasets.js');
const editorial=await import(moduleURL('editorial')) as typeof import('../../web/src/editorial.js');
const profile=await import(moduleURL('profile')) as typeof import('../../web/src/profile.js');
const requests:{url:string;signal?:AbortSignal|null}[]=[];
let respond: (url:string,init?:RequestInit)=>Promise<Response>=async url=>{
  const filename=path.join(dataRoot,url.replace(/^data\//,''));
  return fs.existsSync(filename)?new Response(fs.readFileSync(filename,'utf8')):new Response('missing',{status:404});
};
globalThis.fetch=async(input,init)=>{
  const url=String(input);requests.push({url,signal:init?.signal});return respond(url,init);
};
const localResponse=respond;
const recordFor=(pageID:number): CandidateRecord=>{const value=dataset.link_candidates?.[String(pageID)];assert(value);return value;};
let corrupt=false,contentCalls=0;let pendingSource: Promise<void> | null=null;
const articleText="Testo integrale à ł. [[Voce sintetica 001]]\n== Relazioni ==\nContenuto sintetico.";
editorial.configureEditorial({data:dataset,datasetFile:entry.file,loadVersion:1,authEnabled:false,authUsername:null,
  llmProvider:'qwen',llmProviders:{qwen:{name:'Qwen',url:'https://chat.qwen.ai/'}},
  pageLanguage:()=>dataset.metadata.language,pageKey:page=>String(page.page_id),hiddenSuggestedLink:()=>false,
  userPage:(name,language)=>`https://${language}.wikipedia.org/wiki/User:${encodeURIComponent(name)}`,
  validUsername:(value):value is string=>typeof value==='string'&&value.length>0,
  loadAuth:async()=>{},renderDataset:()=>{},fail:error=>{throw error;},
  wikimediaAPI:async(url,signal)=>{
    contentCalls++;if(pendingSource)await pendingSource;signal?.throwIfAborted();
    assert.equal(url.searchParams.get('action'),'query');assert.equal(url.searchParams.get('prop'),'revisions');
    assert.equal(url.searchParams.get('rvslots'),'main');
    const id=Number(url.searchParams.get('pageids'));assert(id>0);
    return {query:{pages:[{pageid:id,ns:0,title:title(id),revisions:[{revid:id+1000,timestamp:'2026-10-01T00:00:00Z',
      size:Buffer.byteLength(articleText),sha1:corrupt?'0'.repeat(40):createHash('sha1').update(articleText).digest('hex'),
      slots:{main:{contentmodel:'wikitext',content:articleText}}}]}]}};
  }});

await test('static imports and page references fingerprint every production asset',()=>{
  for(const filename of ['index','creators','deorphanize','connections']) {
    const html=fs.readFileSync(path.join(root,'web',filename+'.html'),'utf8');
    for(const asset of ['app.js','style.css']) {
      const digest=createHash('sha256').update(fs.readFileSync(path.join(root,'web/assets',asset))).digest('hex').slice(0,12);
      assert(html.includes(`assets/${asset}?v=${digest}`));
    }
  }
  for(const file of fs.readdirSync(path.join(root,'web/src')).filter(file=>file.endsWith('.ts'))) {
    const compiled=fs.readFileSync(path.join(root,'web/assets',file.replace(/\.ts$/,'.js')),'utf8');
    for(const match of compiled.matchAll(/from '\.\/([a-z-]+\.js)\?v=([a-f0-9]+)'/g)) {
      const digest=createHash('sha256').update(fs.readFileSync(path.join(root,'web/assets',match[1]))).digest('hex').slice(0,12);
      assert.equal(match[2],digest,match[1]);
    }
  }
});
await test('manifest requires relational storage and every operational resource',()=>{
  assert.equal(datasets.validateManifest([entry])[0].file,entry.file);
  for(const field of ['storage','summary_file','connections_dir','connections_metadata_file','profile_topics_dir','cards_dir']) {
    const invalid={...entry};delete invalid[field as keyof ManifestEntry];
    assert.throws(()=>datasets.validateManifest([invalid]),{name:'Error'},field);
  }
  assert.throws(()=>datasets.validateManifest([entry,entry]));
  assert.throws(()=>datasets.validateManifest([{...entry,file:'../private.json'}]));
});
await test('prepared summaries retain NULL and fail rather than recalculate absent reports',async()=>{
  const summary=await datasets.readSummary(entry);
  assert.equal(summary.census_page_count,64);
  assert.equal(summary.reports.numeric.length_bytes.groups[0].missing,1);
  const invalid=structuredClone(summary);delete invalid.reports.numeric.length_bytes;
  respond=async()=>new Response(JSON.stringify(invalid));
  await assert.rejects(datasets.readSummary(entry),/Riepilogo non valido/);
  respond=async()=>new Response('missing',{status:404});
  const start=requests.length;await assert.rejects(datasets.readSummary(entry),/Riepilogo non disponibile/);
  assert.deepEqual(requests.slice(start).map(request=>request.url),['data/'+entry.summary_file]);
  respond=localResponse;
});
await test('creator axes use round references at their true positions without changing prepared observations',async()=>{
  const {createCharts}=await import(moduleURL('charts')) as typeof import('../../web/src/charts.js');
  const summary=await datasets.readSummary(entry);
  const report=summary.reports.numeric.creator_prior_edits_main;
  report.ticks=[0,9,99,999,10000];report.cumulative_ticks=[0,9,99,999,10000];
  const original=structuredClone(report);
  const charts=createCharts({data:dataset,precomputed:summary,topicDisplayNames:new Map(),creatorMode:()=> 'observed'});
  const chart=node('creator_prior_edits_main-chart');
  for(const width of [900,300])for(const mode of ['percent','count','cumulative']) {
    chart.clientWidth=width;node('chart-type').value=mode;
    charts.distribution('creator_prior_edits_main-chart','creator_prior_edits_main','modifiche');
    const svg=chart.children.find(child=>child.attributes.has('viewBox'));assert(svg);
    const bottom=(width<600?380:520)-76;
    const labels=svg.children.filter(child=>child.attributes.get('y')===String(bottom+30));
    assert(labels.length>=2);
    const values=labels.map(label=>Number(label.textContent.replaceAll('.','')));
    assert(values.every(value=>[0,1,10,100,1000,10000].includes(value)));
    assert.equal(new Set(values).size,values.length);
    for(const [index,label] of labels.entries()) {
      const fraction=Math.log1p(values[index])/Math.log1p(10000);
      const expected=64+(mode==='cumulative'?fraction:(1+fraction*18)/20)*(width-88);
      assert(Math.abs(Number(label.attributes.get('x'))-expected)<1e-8);
    }
    assert.deepEqual(report,original);
  }
  // A constant distribution has one reference, and small ranges retain useful ticks.
  for(const [minimum,maximum] of [[10,10],[12,18],[0,0.5]]) {
    report.ticks=[minimum,minimum,minimum,minimum,maximum];
    chart.clientWidth=900;node('chart-type').value='percent';
    charts.distribution('creator_prior_edits_main-chart','creator_prior_edits_main','modifiche');
    const svg=chart.children.find(child=>child.attributes.has('viewBox'));assert(svg);
    const labels=svg.children.filter(child=>child.attributes.get('y')===String(520-76+30));
    assert(labels.length>0);assert.equal(new Set(labels.map(label=>label.textContent)).size,labels.length);
  }
});
await test('article axes use round years and bytes at their true linear and logarithmic positions',async()=>{
  const {createCharts}=await import(moduleURL('charts')) as typeof import('../../web/src/charts.js');
  const summary=await datasets.readSummary(entry);
  const charts=createCharts({data:dataset,precomputed:summary,topicDisplayNames:new Map(),creatorMode:()=> 'observed'});
  for(const [id,field,maximum,unit] of [['age','age_days',24,'anni'],['length','length_bytes',100000,'byte']] as const) {
    const report=summary.reports.numeric[field];
    report.ticks=[0,maximum/4,maximum/2,maximum*3/4,maximum];
    report.cumulative_ticks=[...report.ticks];
    const original=structuredClone(report),chart=node(id);
    for(const width of [900,300])for(const mode of ['percent','count','cumulative']) {
      chart.clientWidth=width;node('chart-type').value=mode;
      charts.distribution(id,field,unit);
      const svg=chart.children.find(child=>child.attributes.has('viewBox'));assert(svg);
      const bottom=(width<600?380:520)-76;
      const labels=svg.children.filter(child=>child.attributes.get('y')===String(bottom+30));
      const values=labels.map(label=>Number(label.textContent.replaceAll('.','')));
      assert(values.length>=2);assert.equal(new Set(values).size,values.length);
      for(const [index,value] of values.entries()) {
        assert((field==='age_days'?[0,10,20]:[0,1,10,100,1000,10000,100000]).includes(value));
        const fraction=field==='age_days'?value/maximum:Math.log1p(value)/Math.log1p(maximum);
        const expected=64+(mode==='cumulative'?fraction:(1+fraction*18)/20)*(width-88);
        assert(Math.abs(Number(labels[index].attributes.get('x'))-expected)<1e-8);
      }
      if(mode!=='cumulative') {
        assert.equal(svg.children.filter(child=>child.attributes.has('width')).length,40);
        assert(chart.children.some(child=>child.textContent.includes('2°')));
      }
      assert.deepEqual(report,original);
    }
  }
  const narrow=summary.reports.numeric.length_bytes;
  narrow.ticks=[103,118,133,148,163];node('length').clientWidth=300;node('chart-type').value='percent';
  charts.distribution('length','length_bytes','byte');
  const svg=node('length').children.find(child=>child.attributes.has('viewBox'));assert(svg);
  assert.deepEqual(svg.children.filter(child=>child.attributes.get('y')===String(380-76+30)).map(label=>label.textContent),['120','140','160']);
});
await test('continuity separates zero, one and two months and retains overflow in counts and percentages',async()=>{
  const {createCharts}=await import(moduleURL('charts')) as typeof import('../../web/src/charts.js');
  const summary=await datasets.readSummary(entry),original=structuredClone(summary);
  const report=summary.reports.numeric.creator_continuity_months_before_creation;
  assert(report.month_histogram);
  const charts=createCharts({data:dataset,precomputed:summary,topicDisplayNames:new Map(),creatorMode:()=> 'observed'});
  const chart=node('creator_continuity_months_before_creation-chart');
  for(const width of [900,300])for(const mode of ['percent','count','cumulative']) {
    chart.clientWidth=width;node('chart-type').value=mode;
    charts.historyCharts();
    const svg=chart.children.find(child=>child.attributes.has('viewBox'));assert(svg);
    const bottom=(width<600?380:520)-76;
    const labels=svg.children.filter(child=>child.attributes.get('y')===String(bottom+30)).map(label=>label.textContent);
    assert(svg.children.some(child=>child.textContent==='Mesi consecutivi'));
    if(mode!=='cumulative') {
      if(width===900)assert.deepEqual(labels,Array.from({length:13},(_,index)=>String(index)).concat('>12'));
      else assert.deepEqual(labels,['0','4','8','>12']);
      const bars=svg.children.filter(child=>child.attributes.has('width'));
      assert.equal(bars.length,28);
      assert(bars.some(bar=>bar.attributes.get('aria-label')?.includes('>12 Mesi consecutivi')));
      for(const bin of [0,1,2])assert(bars.some(bar=>bar.attributes.get('aria-label')?.includes(`· ${bin} Mesi consecutivi`)));
    } else assert.equal(svg.children.filter(child=>child.attributes.has('d')).length,2);
    assert.deepEqual(summary,original);
  }
  const invalid=structuredClone(summary);delete invalid.reports.numeric.creator_continuity_months_before_creation.month_histogram;
  respond=async()=>new Response(JSON.stringify(invalid));
  const older=await datasets.readSummary(entry);
  const olderCharts=createCharts({data:dataset,precomputed:older,topicDisplayNames:new Map(),creatorMode:()=> 'observed'});
  node('chart-type').value='percent';olderCharts.historyCharts();
  assert.equal(chart.children.length,1);
  assert(chart.children[0].textContent.includes('aggiornare e riavviare il servizio dashboard'));
  olderCharts.creatorCharts();
  assert(node('creator_tenure_days-chart').children.some(child=>child.attributes.has('viewBox')));
  invalid.reports.numeric.creator_continuity_months_before_creation.month_histogram=structuredClone(report.month_histogram);
  invalid.reports.numeric.creator_continuity_months_before_creation.month_histogram!.groups[0].counts=[];
  await assert.rejects(datasets.readSummary(entry),/Riepilogo della continuità non valido/);
  respond=localResponse;
});
await test('connection v2 requires matching metadata and never loads a canonical census',async()=>{
  const start=requests.length;const next=await datasets.readConnections(entry);
  assert.equal(next.pages.length,1);assert.equal(next.pages[0].page_id,1);
  assert(next.link_candidates?.['1'].candidates?.[0].evidence?.length);
  assert.equal(requests.slice(start).length,2);
  assert(!requests.slice(start).some(request=>request.url==='data/'+entry.file));
  const sharedName='data/'+entry.connections_metadata_file;
  respond=async(url,init)=>{
    const response=await localResponse(url,init);
    if(url===sharedName){const value:unknown=await response.json();assert(record(value));value.connection_identity='f'.repeat(64);return new Response(JSON.stringify(value));}
    return response;
  };
  await assert.rejects(datasets.readConnections(entry),/Metadati dei collegamenti non validi/);
  respond=async(url,init)=>url===sharedName?new Response('missing',{status:404}):localResponse(url,init);
  await assert.rejects(datasets.readConnections(entry),/Dataset non disponibile/);
  respond=localResponse;
});
await test('invalid pages fail validation while measured missing values remain NULL',()=>{
  datasets.validateDataset(dataset);
  assert.equal(dataset.pages[0].length_bytes,null);
  assert.throws(()=>datasets.validateDataset({...dataset,pages:[{...dataset.pages[0],page_id:0}]}));
  assert.throws(()=>datasets.validateDataset({...dataset,pages:[{...dataset.pages[0],title:undefined}]} as unknown as Dataset));
});
await test('profile topics fetch only selected shards and preserve empty predictions',async()=>{
  profile.configureProfile({datasetEntries:[entry],topicDisplayNames:new Map([['Culture.Media.Music','Music']])});
  const start=requests.length;const found=await profile.readProfileTopics(entry,new Set([1,2,999999]),new AbortController().signal);
  assert.deepEqual(found.get(1),[]);assert.deepEqual(found.get(2),['Culture.Media.Music']);assert.equal(found.get(999999),null);
  assert.equal(requests.slice(start).length,2);
  assert(requests.slice(start).every(request=>request.url.startsWith('data/'+entry.profile_topics_dir)));
  profile.clearProfileCache();respond=async url=>url.endsWith('/0.json')?new Response('missing',{status:404}):localResponse(url);
  await assert.rejects(profile.readProfileTopics(entry,new Set([2]),new AbortController().signal),/Trasporto osservazioni non disponibile/);
  respond=localResponse;
});
await test('profile submission queries only attached, collected Wikipedia editions',async()=>{
  profile.clearProfileCache();
  const start=requests.length;
  respond=async url=>{
    const api=new URL(url);
    if(api.searchParams.get('meta')==='globaluserinfo')return new Response(JSON.stringify({query:{globaluserinfo:{id:9,name:'Example',merged:[
      {wiki:'vecwiki',id:42,url:'https://vec.wikipedia.org'}, {wiki:'enwiki',id:99,url:'https://en.wikipedia.org'}]}}}));
    assert.equal(api.hostname,'vec.wikipedia.org');assert.equal(api.searchParams.get('list'),'usercontribs');
    assert.equal(api.searchParams.get('uclimit'),'100');assert.equal(api.searchParams.get('ucuserids'),'42');
    return new Response(JSON.stringify({query:{usercontribs:[{userid:42,pageid:2,ns:0,timestamp:'2026-10-01T00:00:00Z'}]}}));
  };
  const edits=await profile.recentWikipediaEdits('Example',new AbortController().signal);
  assert.equal(edits.length,1);assert.equal(edits[0].language,'vec');assert.equal(requests.length-start,2);
  assert(!requests.slice(start).some(request=>request.url.includes('en.wikipedia')));
  respond=localResponse;
});
function request(): PromptRequest {const result=editorial.fixRequest(dataset.pages[0],recordFor(1));assert(result);return result;}
await test('prompt preparation is click-only, deduplicated, Wikipedia-only and bounded',async()=>{
  const prepared=request();assert.equal(contentCalls,0);assert.equal(prepared.review?.sources.length,1);
  assert.equal(prepared.sources.length,4);assert.match(prepared.outputRules,/ADD LINK/);assert.match(prepared.prompt,/No execution, authentication or live verification/);
  editorial.setConnectionCopyRequest(prepared);await editorial.copyConnectionPrompt();
  assert.equal(contentCalls,4);assert.equal(node('llm-prompt').open,true);assert.equal(node('llm-prompt-copy').disabled,false);
  const text=node('llm-prompt-text').value;assert(text.includes(articleText));assert(!text.includes('UNREADABLE'));
  assert.equal((text.match(/^DOCUMENT D\d+/gm)||[]).length,4);assert.equal(node('llm-prompt-open').href,'https://chat.qwen.ai/');
  await editorial.copyPrompt();assert.equal(copied.at(-1),text);assert.equal(storage.size,0);
  editorial.closePrompt();assert.equal(node('llm-prompt-text').value,'');assert.equal(node('llm-prompt').open,false);
});
await test('invalid source checksums reject preparation without inserting error-text documents',async()=>{
  corrupt=true;editorial.setConnectionCopyRequest(request());
  await assert.rejects(editorial.copyConnectionPrompt(),/Fonte Wikipedia non verificabile/);
  assert.equal(node('llm-prompt-text').value,'');assert.equal(node('llm-prompt-copy').disabled,true);
  corrupt=false;
});
await test('explicit review snapshot excludes closed pairs and can reopen them',async()=>{
  const source=recordFor(1).candidates?.[0];assert(source);
  respond=async()=>new Response(JSON.stringify({format:'editorial_reviews_v1',reviews:[{language:'vec',source_id:source.page_id,target_id:1,status:'fixed',version:1,updated_at:'2026-10-01T00:00:00Z'}]}));
  await editorial.loadSharedReviews();assert.equal(editorial.fixRequest(dataset.pages[0],recordFor(1)),null);
  respond=async()=>new Response(JSON.stringify({format:'editorial_reviews_v1',reviews:[{language:'vec',source_id:source.page_id,target_id:1,status:'not_fixed',version:2,updated_at:'2026-10-01T00:00:00Z'}]}));
  await editorial.loadSharedReviews();assert(editorial.fixRequest(dataset.pages[0],recordFor(1)));
  respond=localResponse;editorial.resetReviews();
});
await test('missing shared review state is an error, not an anonymous empty snapshot',async()=>{
  respond=async()=>new Response('missing',{status:503});
  await assert.rejects(editorial.loadSharedReviews(),/Valutazioni condivise non disponibili/);
  assert.equal(editorial.reviewsAvailable,false);respond=localResponse;
});
await test('review buttons toggle either selection off and preserve shared versions and prompt eligibility',async()=>{
  const prepared=request(),context=prepared.review;assert(context);
  const source=context.sources[0];let version=0;
  respond=async()=>new Response(JSON.stringify({format:'editorial_reviews_v1',reviews:[]}));
  await editorial.loadSharedReviews();editorial.showReview(context);
  const buttons=()=>node('review-rows').children[0].children.at(-1)!.children;
  assert.deepEqual(buttons().map(button=>button.textContent),['Fixato','Bad link']);
  const sequence=['fixed','fixed','bad_link','fixed','fixed','bad_link','bad_link'] as const;
  const expected=['fixed','not_fixed','bad_link','fixed','not_fixed','bad_link','not_fixed'];
  for(const [index,status] of sequence.entries()) {
    respond=async(url,init)=>{
      assert.equal(url,'api/reviews');assert.equal(init?.method,'POST');
      const body:unknown=JSON.parse(String(init.body));assert(record(body));
      assert.equal(body.status,expected[index]);assert.equal(body.version,version);
      assert.equal(body.source_id,source.page_id);assert.equal(body.target_id,context.target_id);
      return new Response(JSON.stringify({review:{...body,version:++version,updated_at:'2026-10-01T00:00:00Z'}}));
    };
    buttons()[status==='fixed'?0:1].listeners.get('click')!();
    await new Promise<void>(resolve=>setImmediate(resolve));
    assert.equal(version,index+1);
    const closed=expected[index]!=='not_fixed';
    assert.equal(editorial.closedEdge(dataset.pages[0],source.page_id),closed);
    assert.equal(editorial.fixRequest(dataset.pages[0],recordFor(1))===null,closed);
    assert.deepEqual(buttons().map(button=>button.attributes.get('aria-pressed')),
      [String(expected[index]==='fixed'),String(expected[index]==='bad_link')]);
  }
  editorial.closeReview();editorial.resetReviews();respond=localResponse;
});
await test('malformed predicted topic labels are errors, not missing observations',async()=>{
  profile.clearProfileCache();
  respond=async(url,init)=>{
    const response=await localResponse(url,init);
    if(!url.endsWith('/0.json'))return response;
    const payload: unknown=await response.json();assert(record(payload)&&Array.isArray(payload.rows));
    payload.rows=[[2,'predicted',['invented.topic']]];return new Response(JSON.stringify(payload));
  };
  await assert.rejects(profile.readProfileTopics(entry,new Set([2]),new AbortController().signal),/Trasporto osservazioni non valido/);
  respond=localResponse;
});
await test('profile merge retains the newest 100 across all collected attached editions',async()=>{
  profile.clearProfileCache();
  const lmo: ManifestEntry={...entry,language:'lmo',file:'lmo.json'};
  profile.configureProfile({datasetEntries:[entry,lmo],topicDisplayNames:new Map([['Culture.Media.Music','Music']])});
  const start=requests.length;
  respond=async url=>{
    const api=new URL(url);
    if(api.searchParams.get('meta')==='globaluserinfo')return new Response(JSON.stringify({query:{globaluserinfo:{id:9,name:'Example',merged:[
      {wiki:'vecwiki',id:42,url:'https://vec.wikipedia.org'},{wiki:'lmowiki',id:43,url:'https://lmo.wikipedia.org'},
      {wiki:'enwiki',id:99,url:'https://en.wikipedia.org'}]}}}));
    const local=api.hostname==='vec.wikipedia.org'?42:43;
    return new Response(JSON.stringify({query:{usercontribs:Array.from({length:100},(_,index)=>({userid:local,pageid:index+1,ns:0,
      timestamp:new Date(Date.UTC(2026,9,1,0,0,index*2+(local===43?1:0))).toISOString().replace('.000','')}))}}));
  };
  const edits=await profile.recentWikipediaEdits('Example',new AbortController().signal);
  assert.equal(edits.length,100);assert.equal(edits.filter(edit=>edit.language==='vec').length,50);
  assert.equal(edits.filter(edit=>edit.language==='lmo').length,50);assert.equal(edits[0].page_id,100);
  assert.equal(requests.length-start,3);assert(edits.every((edit,index)=>!index||edits[index-1].timestamp>=edit.timestamp));
  respond=localResponse;profile.configureProfile({datasetEntries:[entry],topicDisplayNames:new Map([['Culture.Media.Music','Music']])});
});
await test('profile shard workers share four slots and retain only requested lookups',async()=>{
  profile.clearProfileCache();let active=0,peak=0;
  const wanted=new Set(Array.from({length:8},(_,bucket)=>bucket*1024+1));const start=requests.length;
  respond=async url=>{
    if(url.endsWith('/index.json'))return new Response(JSON.stringify({format:'profile_topics_index_v1',metadata:dataset.metadata,census_page_count:8192,
      shard_size:1024,shards:[0,1,2,3,4,5,6,7]}));
    active++;peak=Math.max(peak,active);await new Promise(resolve=>setTimeout(resolve,3));active--;
    const bucket=Number(url.split('/').at(-1)?.replace('.json',''));
    return new Response(JSON.stringify({format:'profile_topics_shard_v1',metadata:dataset.metadata,bucket,rows:[[bucket*1024+1,'predicted',[]]]}));
  };
  const found=await profile.readProfileTopics(entry,wanted,new AbortController().signal);
  assert.equal(peak,4);assert.equal(found.size,8);assert.equal(requests.length-start,9);
  for(const labels of found.values())assert.deepEqual(labels,[]);
  respond=localResponse;profile.clearProfileCache();
});
await test('closing a preparation aborts pending documents and never restores stale text',async()=>{
  let release: ()=>void=()=>{};
  pendingSource=new Promise<void>(resolve=>{release=resolve;});
  editorial.setConnectionCopyRequest(request());const preparation=editorial.copyConnectionPrompt();
  assert.equal(node('llm-prompt').open,true);editorial.closePrompt();release();await preparation;pendingSource=null;
  assert.equal(node('llm-prompt-text').value,'');assert.equal(node('llm-prompt').open,false);
  assert.equal(node('llm-prompt-copy').disabled,true);assert.equal(storage.size,0);
});
console.log('Current relational modules verified; layout and SQL interactions are covered by Playwright.');
