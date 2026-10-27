// Dependency-free DOM smoke test. Does not substitute for browser layout review.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import {pathToFileURL} from 'node:url';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash,webcrypto} from 'node:crypto';


function evaluate(source: string,context: vm.Context,options?: Parameters<typeof vm.runInContext>[2]): unknown {
  return vm.runInContext(source,context,options);
}
async function evaluateAsync(source: string,context: vm.Context): Promise<unknown> {
  return evaluate(source,context);
}

interface FetchOptions {
  method?: string;
  body?: string;
  headers?: Record<string,string>;
  cache?: string;
  credentials?: string;
  signal?: AbortSignal;
}
interface RecordedRequest extends FetchOptions { url: string; method: string; }
interface MockResponse { ok: boolean; status?: number; headers?: {get: (name: string) => string | null}; json: () => Promise<unknown>; }
interface HarnessPreferences {
  chooseLanguage?: string;
  storage?: Map<string,string>;
  systemDark?: boolean;
  blockStorage?: boolean;
  clipboardUnavailable?: boolean;
  clipboardFailure?: boolean;
  authSession?: {enabled: boolean; username: string | null};
  authAPI?: (url: string,options: FetchOptions) => MockResponse | Promise<MockResponse>;
  reviewStore?: Map<string,EditorialReview>;
  reviewAPI?: (url: string,options: FetchOptions) => MockResponse | Promise<MockResponse>;
  pagesAPI?: (options: FetchOptions) => MockResponse | Promise<MockResponse>;
  reviewsUnavailable?: boolean;
  beforeFetch?: (url: string,options: FetchOptions) => unknown;
  apiResponse?: (url: URL,options: FetchOptions) => unknown;
  apiHTTP?: (url: URL,options: FetchOptions) => MockResponse | Promise<MockResponse>;
  contentLengths?: Record<string,string | number>;
  cachedFiles?: Record<string,unknown>;
}

const appCode=fs.readFileSync('web/assets/app.js','utf8');
const nativeReference=appCode.match(/const nativeModuleUrl='([^']+)';/);
assert(nativeReference, 'The generated app must declare its Wasm module');
const nativePath=nativeReference[1];
const nativeReady=import(new URL(nativePath,pathToFileURL(path.resolve('web/assets/app.js'))).href).then(async (value: unknown)=>{
  assert(isRecord(value) && typeof value.default==='function' && typeof value.calculate==='function');
  const wasmFile=fs.readdirSync('web/assets/native').find(name=>name.endsWith('.wasm'));
  assert(wasmFile, 'The generated Wasm file must exist');
  await value.default({module_or_path:fs.readFileSync(path.join('web/assets/native',wasmFile))});
  const calculate=value.calculate;
  return {calculate(kind: string,input: string): string {
    const result: unknown=calculate(kind,input);assert(typeof result==='string');return result;
  }};
});
const fixtureDir=fs.mkdtempSync(path.join(os.tmpdir(),'orphanwiki-dashboard-'));
const fixtureFile=path.join(fixtureDir,'fixture.json');
// Frozen synthetic inputs from seed 42; these are test data, never research inputs.
const fixtureInputs: [number, number[]][]=[
  [4001,[63,58]],
  [8864,[109,9,8,24]],
  [924,[]],
  [4647,[108,57,115,151]],
  [5619,[88,72,40]],
  [250,[]],
  [18128,[]],
  [8264,[21,142,76]],
  [17634,[148,50]],
  [4634,[75]],
  [10067,[163,94,42]],
  [2125,[166,19]],
  [3909,[42]],
  [2680,[57,84,15,59]],
  [2397,[81,55,168,128]],
  [241,[37,68,36]],
  [907,[150,110]],
  [11656,[36]],
  [3045,[]],
  [7168,[109]],
  [4115,[120,136,65,142]],
  [12426,[69,165,88,29]],
  [1799,[]],
  [1124,[130]],
  [944,[51,40,96,42]],
  [4299,[1,154,83,126]],
  [1957,[21,22,125,18]],
  [309,[43,68,136,156]],
  [2438,[52,80,103,167]],
  [222,[31,64,58]],
  [3903,[]],
  [272,[18]],
  [935,[55,139,34]],
  [3667,[105,49,25]],
  [693,[106,120,14]],
  [747,[28,64]],
  [758,[109]],
  [886,[]],
  [1281,[26,13,167,139]],
  [784,[103]],
  [8036,[]],
  [8717,[143,125,40]],
  [1318,[]],
  [4255,[15,13]],
  [4224,[41,15,131,21]],
  [100,[64,149,153,11]],
  [4551,[145,134,81,67]],
  [348,[68]],
  [1550,[118,81]],
  [3442,[55,130,68,34]],
  [10284,[113]],
  [30258,[168,136,3,142]],
  [873,[68]],
  [3465,[54,88,53,163]],
  [12950,[14,24]],
  [15061,[]],
  [4369,[142,110,144]],
  [5257,[142,38,111,33]],
  [675,[]],
  [8430,[27]],
  [1503,[159,40,61]],
  [7133,[]],
  [2317,[57,52,118]],
  [531,[58]],
  [220,[90,165]],
  [3166,[85,8,30,67]],
  [1734,[89,81,112]],
  [4663,[148,49,66]],
  [564,[138,51,94,111]],
  [1790,[]],
  [5690,[105,84]],
  [2241,[50]],
  [191,[158]],
  [3787,[]],
  [1787,[156,168,83,120]],
  [2060,[122,44,169,22]],
  [793,[86,24,61,80]],
  [1468,[]],
  [8018,[107,162,148]],
  [375,[103,63,38]],
  [28190,[]],
  [6208,[119,13,143,64]],
  [727,[144,153,82,114]],
  [6809,[110,141,115,41]],
  [7745,[64,164]],
  [379,[161,62,71]],
  [1935,[70]],
  [126,[]],
  [1131,[55]],
  [655,[120,107,16,53]],
  [9158,[6,148,98,123]],
  [620,[108,138,140]],
  [2883,[8,100,87]],
  [16684,[120]],
  [6340,[145,7,22,165]],
  [988,[13]],
  [1430,[84,87,98]],
  [1120,[5,139,14]],
  [12245,[]],
  [1314,[]],
  [3527,[95,43]],
  [22537,[80]],
  [898,[102,51,20]],
  [9127,[27]],
  [5330,[31,145,11,89]],
  [3637,[]],
  [3037,[126,28,111]],
  [30090,[46,134,167]],
  [252,[124,120,112,152]],
  [119,[]],
  [1301,[146,157,98]],
  [1531,[47,125]],
  [158,[72,153]],
  [5283,[]],
  [3109,[105]],
  [2704,[166,126,115]],
  [5795,[104]],
  [5266,[95,122,142,136]],
  [3498,[70,79,65]],
  [407,[31,138]],
  [262,[151,135]],
  [7887,[46,78]],
  [4285,[164]],
  [13225,[]],
  [3973,[88,48,14]],
  [2863,[]],
  [922,[156,153,159]],
  [913,[114,77,151]],
  [5643,[]],
  [517,[62]],
  [3914,[74]],
  [12511,[19,60,68]],
  [14477,[109]],
  [1071,[69]],
  [1104,[74,113,32,120]],
  [10868,[129,139]],
  [2939,[]],
  [3711,[148]],
  [3580,[148,11]],
  [5748,[167,114,72,47]],
  [18279,[24,121,90]],
  [2257,[]],
  [8944,[74,103,141]],
  [1674,[132,1,169]],
  [12250,[]],
  [1088,[161,114,14]],
  [769,[113,125]],
  [4002,[80]],
  [3765,[]],
  [4270,[131,70]],
  [2129,[63,117,142]],
  [1207,[131,35,18,71]],
  [630,[1,73]],
  [52224,[169,126,39,115]],
  [3365,[140,97,117,83]],
  [1247,[12,82,122]],
  [28007,[167,39,127]],
  [12507,[]],
  [11805,[117,4,37,105]],
  [825,[167,21,85]],
  [2076,[139,10,159]],
  [7313,[]],
  [18217,[]],
  [2040,[]],
  [1509,[63]],
  [3455,[44]],
  [537,[159,62,128]],
  [2677,[3,120]],
  [844,[151,77]],
  [4899,[117,78]],
  [1089,[28,61,98]],
  [4222,[6,169]],
  [2401,[]],
  [6231,[57,163]],
  [1272,[161]],
  [321,[]],
  [1475,[94,34,24,76]],
  [1619,[34]],
  [1712,[43,66]],
  [11673,[30,120]],
  [796,[]],
  [1714,[68,150]],
  [1513,[28,60]],
  [2554,[84,157,57,166]],
  [9974,[30,36,12]],
  [11166,[30,25,61]],
  [2231,[108,151,40,107]],
  [20286,[105,72,9,95]],
  [1045,[93]],
  [185588,[16,102]],
  [1025,[6,13,86,63]],
  [3904,[85]],
  [43147,[1,71,38,34]],
  [4495,[]],
  [4189,[]],
  [750,[83,5,45,68]],
  [3681,[115,93,132]],
  [4238,[158]],
  [14453,[165,8,16]],
  [2514,[19,21,83]],
  [3784,[160,163]],
  [5489,[153,136,76]],
  [2341,[]],
  [5891,[56,111,116,59]],
  [1066,[107,25,81]],
  [1900,[40,122]],
  [40605,[16,151,144,144]],
  [1999,[109,14]],
  [1445,[40]],
  [4518,[]],
  [1487,[]],
  [10607,[144,160,158]],
  [6718,[169,69,8,47]],
  [417,[90,2]],
  [1386,[18,37,163]],
  [3296,[88,41,95]],
  [1679,[153,22,14,40]],
  [470,[]],
  [744,[156,114,107]],
  [443,[]],
  [2082,[125,135,79,12]],
  [6408,[]],
  [1015,[129]],
  [4290,[19,102]],
  [17277,[104,164,107,75]],
  [485,[118,93,23,112]],
  [7609,[103,135,21,102]],
  [222,[]],
  [3054,[50,90,90,166]],
  [6968,[66]],
  [621,[]],
  [2837,[115,145,165,163]],
  [13094,[39,113]],
  [605,[71,152]],
  [466,[119,116]],
  [659,[20,166]],
  [4818,[149,142,11]],
  [1730,[83]],
  [4598,[]],
  [1739,[22,130]],
];
function runNative(args: string[]): void {
  const result=spawnSync(path.resolve('target/debug/orphanwiki'),args,{encoding:'utf8'});
  if(result.error)throw result.error;
  if(result.status!==0)throw new Error(result.stderr||'Native fixture export failed');
}
function createFixture(): void {
  const categories=['Comuni','Scritori','Montagne','Fiumi','Storia','Musica'];
  const raw={metadata:{language:'vec',source:'Fixture sintetica, nessuna misura reale di Wikipedia',
    creation_date_rule:'creation_event_v1',item_mapping:{source:'page_props.wikibase_item',complete:true},
    complete:true,demo:true,started_at:'2026-09-29T00:00:00+00:00',finished_at:'2026-09-29T00:00:00+00:00'},
    pages:fixtureInputs.map(([length,links],index)=>{
      const at=`${2005+index%20}-01-01T00:00:00+00:00`;
      return {page_id:index+1,title:`Voce dimostrativa ${index+1}`,
        categories:[categories[index%6],[1,5].includes(index%6)?'Cultura':'Mondo'],length_bytes:length,
        created_at:at,page_created_at:at,first_public_revision_at:at,wikibase_item:`Q${index+1}`,
        creation_date_source:'synthetic',creation_date_status:'observed',links:links.map(id=>`Voce dimostrativa ${id}`)};
    }),redirects:{},category_parents:{Scritori:['Cultura'],Musica:['Cultura'],Montagne:['Mondo'],Fiumi:['Mondo'],Comuni:['Mondo'],Storia:['Mondo']}};
  const reference=structuredClone(raw);reference.metadata.language='lmo';
  reference.pages.forEach((page,index)=>{page.links=index%2===0?[reference.pages[(index+1)%reference.pages.length].title]:[];});
  const inputs=[raw,reference].map(graph=>{
    const file=path.join(fixtureDir,graph.metadata.language+'-raw.json');fs.writeFileSync(file,JSON.stringify(graph));return file;
  });
  const output=path.join(fixtureDir,'comparison');
  runNative(['compare',...inputs,'--output-dir',output,'--work-dir',path.join(fixtureDir,'staging')]);
  const manifest=readManifest(path.join(output,'manifest.json')).filter(entry=>(entry.kind||'wiki')==='wiki');
  const entries=[];
  for(const entry of manifest) {
    const result=readFixture(path.join(output,entry.file)),language=result.metadata.language;
    result.pages.forEach((page,index)=>{
      const group=result.pages.filter((_,position)=>position%15===index%15);
      const prior=group.filter(other=>required(other.created_at)<required(page.created_at)).length;
      const orphans=group.filter(other=>other.orphan).length;
      Object.assign(page,{creator_key:`demo:${language}:creator:${index%15}`,creator_attribution:'synthetic',
        creator_account_type:['registered','bot','temporary'][index%3],creator_is_bot_now:index%3===1,
        creator_tenure_days:prior*100,creator_prior_edits_main:prior*12,creator_prior_edits_other:prior*3,
        creator_prior_articles:prior,creator_articles_created_total:group.length,creator_articles_in_census:group.length,
        creator_orphans_current:orphans,creator_orphans_current_share:orphans/group.length,
        creator_languages_created:[language],creator_languages_before:prior?[language]:[],creator_data_status:'matched',
        origin:index%4===0?'translation_tagged':'new_page_unclassified'});
    });
    result.metadata.creators={status:'collected',language,languages_scanned:[language],matched_pages:result.pages.length,
      unmatched_pages:0,started_at:result.metadata.started_at,finished_at:result.metadata.finished_at};
    const file=language==='vec'?'fixture.json':'lmo-demo.json';
    fs.writeFileSync(path.join(fixtureDir,file),JSON.stringify(result));
    entries.push({file,language,date:result.metadata.finished_at,demo:true});
  }
  fs.writeFileSync(path.join(fixtureDir,'manifest.json'),JSON.stringify(entries));
  runNative(['dashboard','--data-dir',fixtureDir]);
}
try {createFixture();}
catch(error) {fs.rmSync(fixtureDir,{recursive:true});throw error;}
const fixture=(): LoadedFixture=>readFixture(fixtureFile);
// Reuse the browser's transport interfaces; tsconfig.node includes app.ts for types only.
interface FixtureCreatorMetadata extends CreatorMetadata {matched_pages?: number; unmatched_pages?: number;}
interface FixtureMetadata extends DatasetMetadata { creators?: FixtureCreatorMetadata; [key: string]: unknown; }
interface FixturePage extends PageObservation { [key: string]: unknown; }
type FixtureReference=Omit<ReferenceEdge,'source_page_id' | 'target_page_id'> & {source_page_id?: number | string | null; target_page_id?: number | string | null};
// Malformed titles and IDs are deliberate inputs to browser validation tests.
interface FixtureCandidate extends Omit<Candidate,'title' | 'evidence'> {title: string | null; evidence?: FixtureReference[];}
interface FixtureCandidateRecord extends Omit<CandidateRecord,'candidates'> {candidates?: FixtureCandidate[];}
interface FixtureDataset extends Omit<Dataset,'metadata' | 'pages' | 'link_candidates'> {
 metadata: FixtureMetadata; pages: FixturePage[]; link_candidates?: Record<string,FixtureCandidateRecord | undefined>;
}
interface LoadedFixture extends FixtureDataset {
  metadata: FixtureMetadata & { creators: FixtureCreatorMetadata };
  link_candidates: Record<string,FixtureCandidateRecord | undefined>;
}

function numberValue(value: unknown): number {assert(typeof value==='number');return value;}
function required<T>(value: T | null | undefined): T {
  assert(value!==null && value!==undefined, 'Missing test fixture value');
  return value;
}
function reviewBody(text: string | undefined): Pick<EditorialReview,'language' | 'source_id' | 'target_id' | 'status' | 'version'> & {dataset:string} {
  assert(typeof text==='string');
  const value: unknown=JSON.parse(text);
  assert(isRecord(value) && typeof value.language==='string' && typeof value.dataset==='string' &&
    typeof value.source_id==='number' && typeof value.target_id==='number' && typeof value.version==='number' &&
    (value.status==='fixed'||value.status==='bad_link'||value.status==='not_fixed'));
  return {dataset:value.dataset,language:value.language,source_id:value.source_id,target_id:value.target_id,status:value.status,version:value.version};
}

interface EditorialRecord {
  format: string;
  census_page_count: number;
  connection_identity?: string;
  card_identity?: string;
  metadata?: DatasetMetadata;
  pages: FixturePage[];
  link_candidates: Record<string,FixtureCandidateRecord>;
}
interface CardIndexFixture extends Omit<CompactObservations,'link_candidates'> {
  card_identity: string;
  census_page_count: number;
  link_candidates: Record<string,FixtureCandidateRecord>;
}
function isEditorialRecord(value: unknown): value is EditorialRecord {
  return isRecord(value) && typeof value.format==='string' && typeof value.census_page_count==='number' &&
    isList(value.pages) && value.pages.every(page=>isRecord(page)&&typeof page.page_id==='number'&&typeof page.title==='string'&&typeof page.orphan==='boolean') &&
    isRecord(value.link_candidates);
}
function editorialRecord(value: unknown): EditorialRecord {
  assert(isEditorialRecord(value), 'Invalid editorial test record');return value;
}

interface OrphanCardRecord {
  format: string;
  card_identity: string;
  census_page_count: number;
  page: FixturePage;
  candidate_record: FixtureCandidateRecord;
}
function cardRecord(value: unknown): OrphanCardRecord {
  assert(isOrphanCardRecord(value), 'Invalid rich card test record');return value;
}
function isOrphanCardRecord(value: unknown): value is OrphanCardRecord {
  return isRecord(value) && typeof value.format==='string' && typeof value.card_identity==='string' &&
    typeof value.census_page_count==='number' && isRecord(value.page) &&
    typeof value.page.page_id==='number' && typeof value.page.title==='string' &&
    typeof value.page.orphan==='boolean' && isRecord(value.candidate_record);
}
function sharedMetadata(value: unknown): ConnectionMetadata {
  assert(isSharedMetadata(value), 'Invalid shared connection test metadata');return value;
}
function isSharedMetadata(value: unknown): value is ConnectionMetadata {
  return isRecord(value) && typeof value.format==='string' && isMetadata(value.metadata) &&
    typeof value.census_page_count==='number' && typeof value.connection_identity==='string';
}
type TopicRow=[number,string,string[] | null,...unknown[]];
interface TopicFile {
  metadata: DatasetMetadata;
  format?: string;
  rows?: TopicRow[];
  columns?: string[];
  shard_size?: number;
  shards?: number[];
  bucket?: number;
  census_page_count?: number;
}

function isRecord(value: unknown): value is Record<string,unknown> {
  return typeof value==='object' && value!==null && !Array.isArray(value);
}
function isList(value: unknown): value is unknown[] {return Array.isArray(value);}
function isMetadata(value: unknown): value is FixtureMetadata {
  return isRecord(value) && typeof value.language==='string' &&
    typeof value.started_at==='string' && typeof value.finished_at==='string';
}
function isFixture(value: unknown): value is LoadedFixture {
  return isRecord(value) && isMetadata(value.metadata) && isRecord(value.metadata.creators) &&
    typeof value.metadata.creators.status==='string' && isRecord(value.link_candidates) && isList(value.pages) && value.pages.every(page=>
      isRecord(page) && typeof page.page_id==='number' && typeof page.title==='string' && typeof page.orphan==='boolean');
}
function readManifest(file: string): ManifestEntry[] {
  const value: unknown=JSON.parse(fs.readFileSync(file,'utf8'));
  assert(isList(value) && value.every((entry): entry is ManifestEntry=>isRecord(entry) &&
    typeof entry.file==='string' && typeof entry.language==='string' && typeof entry.date==='string'));
  return value;
}
function readFixture(file: string): LoadedFixture {
  const value: unknown=JSON.parse(fs.readFileSync(file,'utf8'));
  assert(isFixture(value), `Invalid generated dataset: ${file}`);
  return value;
}
type ProjectionKey='articles_file' | 'observations_file';
function fixtureTransport(key: 'summary_file',file?: string): DashboardSummaryData;
function fixtureTransport(key: 'creator_index_file',file?: string): CreatorIndexData;
function fixtureTransport(key: 'connections_metadata_file',file?: string): ConnectionMetadata;
function fixtureTransport(key: ProjectionKey,file?: string): CompactObservations;
function fixtureTransport(key: ProjectionKey | 'summary_file' | 'creator_index_file' | 'connections_metadata_file',file='fixture.json'): unknown {
  const entry=readManifest(path.join(fixtureDir,'manifest.json')).find(entry=>entry.file===file);
  assert(entry, `Missing manifest entry: ${file}`);
  const name=entry[key];assert(typeof name==='string');
  const value: unknown=JSON.parse(fs.readFileSync(path.join(fixtureDir,name),'utf8'));
  assert(isRecord(value) && isMetadata(value.metadata) && typeof value.format==='string');
  if(key==='summary_file')assert(isRecord(value.reports) && isRecord(value.reports.overview));
  else if(key==='creator_index_file')assert(isList(value.creators));
  else if(key==='connections_metadata_file')assert.equal(typeof value.connection_identity,'string');
  else assert(isList(value.columns) && value.columns.every(v=>typeof v==='string') &&
    isList(value.rows) && value.rows.every(isList));
  return value;
}
class Element {
  tag: string;
  children: Element[] = [];
  style: Record<string, string> = {};
  listeners: Record<string, (event?: unknown) => unknown> = {};
  dataset: Record<string, string> = {};
  value = '';
  textContent = '';
  className = '';
  href = '';
  rel = '';
  target = '';
  title = '';
  hidden = false;
  disabled = false;
  checked = false;
  open = false;
  focused = false;
  focus(): void {this.focused=true;}
  clientWidth = 0;
  viewBox = '';
  height = '';
  d = '';
  ['aria-label'] = '';
  ['aria-disabled'] = '';
  download = '';
  type = '';
  placeholder = '';
  fill = '';
  private attributes: Record<string, string> = {};
  constructor(tag: string) {this.tag=tag;}
  append(...nodes: Element[]): void {this.children.push(...nodes);if(this.tag==='select' && !this.value && this.children.length)this.value=this.children[0].value;}
  replaceChildren(...nodes: Element[]): void {this.children=[];this.append(...nodes);}
  setAttribute(k: string,v: string): void {this.attributes[k]=v;Reflect.set(this,k,v);}
  getAttribute(k: string): string | undefined {return this.attributes[k];}
  removeAttribute(k: string): void {delete this.attributes[k];if(k==='href')this.href='';}
  addEventListener(type: string,handler: (event?: unknown) => unknown): void {this.listeners[type]=handler;}
  showModal(): void {this.open=true;}
  close(): void {this.open=false;}
}
const descendants=(node: Element): Element[]=>[node,...node.children.flatMap(descendants)];
function findClass(node: Element | undefined,name: string): Element {
  assert(node, `Missing DOM node for ${name}`);
  const found=descendants(node).find(n=>n.className===name);
  assert(found, `Missing DOM class ${name}`);
  return found;
}
const titleLink=(card: Element | undefined): Element=>findClass(card,'candidate-title').children[0];
function changeCheckbox(nodes: Record<string,Element>,id: string,checked: boolean): void {
  nodes[id].checked=checked;nodes[id].listeners.change();
}
function revisionFixture(url: URL,roots: unknown[]) {
  const language=url.hostname.split('.')[0],pageId=Number(url.searchParams.get('pageids'))||null;
  let title=url.searchParams.get('titles'),id=pageId;
  const identities: {language: string; id: unknown; title: string}[]=[];
  for(const root of roots) {
    if(!isRecord(root)||!isRecord(root.metadata))continue;
    const local=root.metadata.language;
    if(typeof local!=='string')continue;
    if(isList(root.pages))for(const page of root.pages) {
      if(isRecord(page)&&typeof page.title==='string')identities.push({language:typeof page.language==='string'?page.language:local,id:page.page_id,title:page.title});
    }
    if(!isRecord(root.link_candidates))continue;
    for(const [key,record] of Object.entries(root.link_candidates)) {
      const code=key.includes(':')?key.split(':')[0]:local;
      if(!isRecord(record)||!isList(record.candidates))continue;
      for(const candidate of record.candidates) {
        if(!isRecord(candidate)||typeof candidate.title!=='string')continue;
        identities.push({language:code,id:candidate.page_id,title:candidate.title});
        if(!isList(candidate.evidence))continue;
        for(const edge of candidate.evidence) {
          if(!isRecord(edge)||typeof edge.language!=='string'||typeof edge.source_title!=='string'||typeof edge.target_title!=='string')continue;
          identities.push({language:edge.language,id:edge.source_page_id,title:edge.source_title});
          identities.push({language:edge.language,id:edge.target_page_id,title:edge.target_title});
        }
      }
    }
  }
  if(pageId)title=identities.find(row=>row.language===language&&row.id===pageId)?.title??null;
  else {
    const found=identities.find(row=>row.language===language&&row.title===title&&typeof row.id==='number'&&Number.isSafeInteger(row.id)&&row.id>0)?.id;
    id=typeof found==='number'?found:100000;
  }
  assert(typeof title==='string',`No synthetic identity for ${url}`);
  assert(typeof id==='number');
  const content=`'''${title}''' (${language})\n== Filmografia ==\nTesto integrale à ł.\n`;
  return revisionContent(id,title,content);
}
interface RevisionPayload {
 query: {pages: {pageid: number; ns: number; title: string; revisions: {
 revid: number; timestamp: string; size: number; sha1: string;
 slots: {main: {contentmodel: string; content: string; texthidden?: boolean}};
 }[]}[]};
 continue?: {rvcontinue: string};
}
function revisionContent(id: number,title: string,content: string): RevisionPayload {
  return {query:{pages:[{pageid:id,ns:0,title,revisions:[{revid:id+1000,timestamp:'2026-10-15T12:00:00Z',size:Buffer.byteLength(content),
    sha1:createHash('sha1').update(content).digest('hex'),slots:{main:{contentmodel:'wikitext',content}}}]}]}};
}
async function preparePrompt(result: Awaited<ReturnType<typeof harness>>,control: Element) {
  assert.equal(typeof control.listeners.click,'function');
  await control.listeners.click();
  assert.equal(result.nodes['llm-prompt'].open,true);
  return result.nodes['llm-prompt-text'].value;
}
interface SourceFixtureMetadata {
  title: string;
  language: string;
  status: string;
  page_id?: number | null;
  revision_id?: number;
  sha1?: string;
  utf8_bytes?: number;
  revision_url?: string;
}
function isSourceMetadata(value: unknown): value is SourceFixtureMetadata {
  return isRecord(value)&&typeof value.title==='string'&&typeof value.language==='string'&&typeof value.status==='string'&&
    (value.page_id==null||typeof value.page_id==='number')&&
    (value.revision_id===undefined||typeof value.revision_id==='number')&&
    (value.sha1===undefined||typeof value.sha1==='string')&&
    (value.utf8_bytes===undefined||typeof value.utf8_bytes==='number')&&
    (value.revision_url===undefined||typeof value.revision_url==='string');
}
function sourceMetadata(prompt: string): SourceFixtureMetadata[] {
  return [...prompt.matchAll(/^DOCUMENT D\d+\n(\{[\s\S]*?^\})/gm)].map(match=>{
    const value: unknown=JSON.parse(match[1]);assert(isSourceMetadata(value));return value;
  });
}
async function harness(page: string,raw: unknown,search='?dataset=fixture.json',entries: ManifestEntry[] | null=null,preferences: HarnessPreferences={},files: Record<string, unknown> | null=null) {
  const file=page==='articles'?'index.html':page+'.html';
  const html=fs.readFileSync('web/'+file,'utf8');
  const nodes: Record<string,Element>={};
  for(const match of html.matchAll(/<(\w+)[^>]*\bid="([^"]+)"/g))nodes[match[2]]=new Element(match[1]);
  const staticNodes: Element[]=[];
  for(const match of html.matchAll(/<(\w+)([^<>]*data-i18n[^<>]*)>/g)) {
    const id=match[2].match(/\bid="([^"]+)"/),node=id?nodes[id[1]]:new Element(match[1]);
    node.dataset={};
    for(const attr of match[2].matchAll(/(data-i18n(?:-[\w-]+)?)="([^"]+)"/g)) {
      node.setAttribute(attr[1],attr[2].replaceAll('&amp;','&').replaceAll('&quot;','"').replaceAll('&#x27;',"'"));
      if(attr[1]==='data-i18n')node.dataset.i18n=attr[2];
    }
    staticNodes.push(node);
  }
  if(nodes['category-metric'])nodes['category-metric'].value='rate';
  const rootDataset: Record<string,string>={};
  const document={documentElement:{dataset: rootDataset,lang:'it'},body:{dataset: Object.assign<Record<string,string>,{page:string}>({},{page})},getElementById:(id: string)=>nodes[id]||null,querySelectorAll:(selector: string)=>staticNodes.filter(node=>node.getAttribute(selector.slice(1,-1))!=null),createElement:(t: string)=>new Element(t),createElementNS:(_ns: string,t: string)=>new Element(t)};
  const storage=preferences.storage||new Map<string,string>(),media={matches:preferences.systemDark||false,listener: (_event?: {matches: boolean})=>{},addEventListener(_type: string,fn: (event?: {matches: boolean})=>void){this.listener=fn;}};
  const localStorage={getItem:(key: string)=>{if(preferences.blockStorage)throw new Error('storage unavailable');return storage.get(key);},setItem:(key: string,value: string)=>{if(preferences.blockStorage)throw new Error('storage unavailable');storage.set(key,value);}};
  const copied: string[]=[];
  const navigator=preferences.clipboardUnavailable?{}:{clipboard:{writeText:async (text: string)=>{if(preferences.clipboardFailure)throw new Error('clipboard denied');copied.push(text);}}};
  // Keep authentication requests separate from transport/merge benchmark counters.
  let authSession=preferences.authSession??{enabled:false,username:null};
  const authRequests: RecordedRequest[]=[],navigations: string[]=[];
  let fetchCount=0;const requests: RecordedRequest[]=[],reviewStore=preferences.reviewStore||new Map<string,EditorialReview>();
  const context: Record<string,unknown>=vm.createContext({window:{navigator,localStorage,matchMedia:()=>media,addEventListener(){},location:{search,assign:(url: string)=>{navigations.push(url);}}},URL,URLSearchParams,TextEncoder,AbortController,crypto:webcrypto,setTimeout,clearTimeout,document,Intl,Map,Set,Math,Number,Option:function(text: string,value: string){const n=new Element('option');n.textContent=text;n.value=value;return n;},fetch:async (url: string,options: FetchOptions={})=>{
    if(String(url).startsWith('api/auth/')) {
      authRequests.push({url,method:options.method??'GET',...options});
      if(preferences.authAPI)return preferences.authAPI(url,options);
      if(url.endsWith('/logout'))authSession={...authSession,username:null};
      return {ok:true,status:200,json:async()=>authSession};
    }
    requests.push({url,method:options?.method||'GET',body:options?.body,headers:options?.headers,cache:options?.cache,credentials:options?.credentials,signal:options?.signal});fetchCount++;if(preferences.beforeFetch)await preferences.beforeFetch(url,options);
    if(url==='api/pages'&&preferences.pagesAPI)return preferences.pagesAPI(options);
    if(String(url).startsWith('api/reviews')) {
      if(preferences.reviewAPI)return preferences.reviewAPI(url,options);
      if(preferences.reviewsUnavailable)return {ok:false,status:503,json:async()=>({error:'unavailable'})};
      if(options?.method==='POST') {
        const body=reviewBody(options.body),key=[body.language,body.source_id,body.target_id].join(':');
        const current=reviewStore.get(key);
        if((current?.version??0)!==body.version)return {ok:false,status:409,json:async()=>({current})};
        const review={language:body.language,source_id:body.source_id,target_id:body.target_id,status:body.status,
          version:body.version+1,updated_at:'2026-10-20T12:00:00Z',reviewer:authSession.username??''};reviewStore.set(key,review);
        return {ok:true,status:200,json:async()=>({review})};
      }
      return {ok:true,status:200,json:async()=>({format:'editorial_reviews_v1',reviews:[...reviewStore.values()]})};
    }
    if(String(url).startsWith('https://')&&preferences.apiHTTP)return preferences.apiHTTP(new URL(url),options);
    return {ok:true,headers:{get:()=>preferences.contentLengths?.[url]??null},json:async()=>String(url).startsWith('https://')?(preferences.apiResponse?preferences.apiResponse(new URL(url),options):revisionFixture(new URL(url),[raw,...Object.values(files||{})])):options?.cache!=='no-store'&&preferences.cachedFiles?.[url]?preferences.cachedFiles[url]:url.endsWith('manifest.json')?(entries||[{language:'vec',file:'demo.json',date:'2026-10-01',demo:true},{language:'vec',file:'unused.json',date:'2026-09-28',demo:false},{language:'vec',file:'fixture.json',date:'2026-09-29',demo:false}]):files?structuredClone(files[url.slice(5)]):raw};}});
  await nativeReady;
  evaluate(appCode,context,{filename:path.resolve('web/assets/app.js'),importModuleDynamically:vm.constants.USE_MAIN_CONTEXT_DEFAULT_LOADER});
  await evaluate('dashboardReady',context);
  if(preferences.chooseLanguage) {
    nodes.dataset.value=preferences.chooseLanguage;await nodes.dataset.listeners.change();
  }
  return {nodes,context,html,staticNodes,storage,media,document,copied,requests,authRequests,navigations,getFetchCount:()=>fetchCount};
}
async function checkFreshResources() {
  const pages=['index.html','creators.html','deorphanize.html','connections.html'];
  assert.deepEqual(fs.readdirSync('web').filter(name=>name.endsWith('.html')).sort(), [...pages,'about.html'].sort());
  const bindings=fs.readFileSync('web/assets/native/dashboard.js');
  assert.equal(nativePath,`./native/dashboard.js?v=${createHash('sha256').update(bindings).digest('hex').slice(0,12)}`);
  const wasmFile=fs.readdirSync('web/assets/native').find(name=>name.endsWith('.wasm'));
  const wasm=fs.readFileSync(path.join('web/assets/native',required(wasmFile)));
  assert(wasmFile);
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
async function checkProjectFooter() {
  for(const page of ['articles','creators','deorphanize','connections']) {
    const raw=fixture();
    const orphan=required(raw.pages.find(record=>record.orphan));
    const {html,nodes,staticNodes}=await harness(page,raw,`?dataset=fixture.json&page=${orphan.page_id}`);
    const footers=[...html.matchAll(/<footer\b[^>]*>([\s\S]*?)<\/footer>/g)];
    assert.equal(footers.length,1,`${page}: one project footer`);
    assert(html.indexOf('<footer>')>html.indexOf('</main>'),`${page}: footer outside the main content`);
    const footer=footers[0][1];
    if(page==='connections')assert(footer.includes('Verifica il contenuto prima di aggiungere il collegamento.'));
    assert.match(footer,/<nav\b[^>]*aria-label="Informazioni sul progetto"/);
    const navigation=required(staticNodes.find(node=>node.getAttribute('data-i18n-aria-label')==='Informazioni sul progetto'));
    assert.equal(navigation.getAttribute('aria-label'),'Informazioni sul progetto');
    const links=[...footer.matchAll(/<a\b[^>]*href="([^"]+)"[^>]*>([^<]+)<\/a>/g)];
    assert.deepEqual(links.map(link=>[link[1],link[2]]),[
      ['https://github.com/niko091/OrphanPageWikipedia','Repository GitHub'],
      ['https://github.com/niko091/OrphanPageWikipedia/blob/main/LICENSE','Licenza MIT'],
      ['about.html','About'],
    ]);
    const labels=()=>['footer-repository','footer-license','footer-about'].map(id=>nodes[id].textContent);
    assert.deepEqual(labels(),['Repository GitHub','Licenza MIT','About']);
    nodes['ui-language-toggle'].listeners.click();
    assert.deepEqual(labels(),['GitHub repository','MIT license','About']);
    assert.equal(navigation.getAttribute('aria-label'),'Project information');
    nodes['ui-language-toggle'].listeners.click();
    assert.deepEqual(labels(),['Repository GitHub','Licenza MIT','About']);
  }
  console.log('Project footer: repository, MIT license, blank About destination and Italian/English labels on all four pages passed.');
}
async function checkPage(creatorsPage: boolean) {
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
    for(const [order,field,direction] of [['length-desc','length_bytes',-1],['length-asc','length_bytes',1],['date-desc','created_at',-1],['date-asc','created_at',1]] as const) {
      nodes['article-sort'].value=order;nodes['article-sort'].listeners.input();
      const expected=raw.pages.filter(p=>p[field]!=null).sort((a,b)=>(field==='created_at'?Date.parse(required(a.created_at))-Date.parse(required(b.created_at)):required(a.length_bytes)-required(b.length_bytes))*direction||a.title.localeCompare(b.title));
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
    raw.metadata.creators.status='not_collected';evaluate('prepareCreators(); creatorList(); creatorCharts()',context);
    assert.equal(nodes['creator-section'].hidden,true);
    assert.match(nodes['creator-info'].textContent,/non disponibili/);
    assert.equal(nodes['creator-rows'].children.length,0);
    raw.metadata.creators.status='collected';evaluate('prepareCreators(); creatorCharts()',context);
  }
  const charts=creatorsPage?creatorFields.map(field=>field+'-chart'):['age','length'];
  for(const chart of charts)assert(nodes[chart].children[0].children.some(n=>n.tag==='rect'));
  nodes['chart-type'].value='cumulative';nodes['chart-type'].listeners.change();
  for(const chart of charts)assert(nodes[chart].children[0].children.some(n=>n.tag==='path'));
  nodes['chart-type'].value='count';nodes['chart-type'].listeners.change();
  for(const width of [300,900,1600]) {
    for(const chart of charts)nodes[chart].clientWidth=width;
    evaluate('renderCharts()',context);
    for(const chart of charts) {
      assert.equal(nodes[chart].children[0].viewBox,`0 0 ${width} ${width<600?380:520}`);
      assert.equal(nodes[chart].children[2].children.length,2);
    }
  }
  const chart=charts[0];nodes[chart].clientWidth=900;
  const chartFixture: {metadata: FixtureMetadata; pages: {orphan: boolean; fixture: number | null}[]}={metadata:raw.metadata,pages:[]};
  context.chartFixture=chartFixture;
  const draw=()=>evaluate(`data=chartFixture;distribution('${chart}','fixture','unità','Valore')`,context);
  chartFixture.pages=[{orphan:true,fixture:0},{orphan:true,fixture:1},{orphan:false,fixture:1},{orphan:false,fixture:20},{orphan:true,fixture:null}];
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
  chartFixture.pages=[{orphan:true,fixture:0},{orphan:true,fixture:20},
    ...Array.from({length:20},(_,i)=>({orphan:false,fixture:i<10?0:20}))];
  draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.equal(Number(bars[1].height),206);
  assert.equal(Number(bars[21].height),206); // 1/2 and 10/20 must be equally tall
  assert.match(bars[21]['aria-label'],/50% \(10\/20 pagine del gruppo\)/);
  chartFixture.pages=[{orphan:false,fixture:0},{orphan:false,fixture:20}];draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.equal(bars.length,20); // no invented percentages for an empty group
  assert(bars.every(bar=>Number.isFinite(Number(bar.height))));
  assert.equal(nodes[chart].children.length,4);
  nodes['chart-type'].value='count';
  chartFixture.pages=[{orphan:true,fixture:0},{orphan:true,fixture:0},{orphan:false,fixture:0}];
  draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.equal(Number(bars[10].height),206);
  assert.equal(Number(bars[30].height),103);
  nodes['chart-type'].value='cumulative';draw();
  const paths=nodes[chart].children[0].children.filter(n=>n.tag==='path');
  assert.equal(paths.length,2);
  assert(paths.every(path=>path.d.endsWith('V32'))); // original normalization: both reach 100%
  nodes['chart-type'].value='count';
  chartFixture.pages=Array.from({length:101},(_,i)=>({orphan:i%2===0,fixture:i}));draw();
  bars=nodes[chart].children[0].children.filter(n=>n.tag==='rect');
  assert.match(bars[0]['aria-label'],/<2 unità: 1 pagine/);
  assert.match(bars[39]['aria-label'],/>98 unità: 1 pagine/);
  assert.equal(bars.reduce((sum,bar)=>sum+Number(required(bar['aria-label'].match(/: (\d+) pagine/))[1]),0),101);
  chartFixture.pages=[];draw();
  assert.equal(nodes[chart].children[0].textContent,'Dati non disponibili.');
  raw.pages=[];
  raw.metadata.age_rule='first_public_revision';
  await evaluate("load({file:'fixture.json'})",context);
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
  assert.equal(evaluate('Object.values(topicTaxonomy).flat().length',context),64);
  assert.equal(evaluate('Object.keys(topicTaxonomy).length',context),4);
  const topicConfig: unknown=JSON.parse(fs.readFileSync('config/topics.json','utf8'));
  assert(isRecord(topicConfig)&&isList(topicConfig.labels)&&topicConfig.labels.every(label=>typeof label==='string'));
  const expectedTopics=topicConfig.labels;
  const actualTopics=evaluate('Object.entries(topicTaxonomy).flatMap(([parent,rows])=>rows.map(row=>parent+"."+row))',context);
  assert(isList(actualTopics));
  assert.deepEqual(Array.from(actualTopics).sort(),expectedTopics.slice().sort());
  const rows=(id: string)=>nodes[id].children.filter(node=>node.className==='bar-row');
  const row=(id: string,name: string)=>rows(id).find(node=>node.children[0].textContent===name);
  const value=(node: Element | undefined)=>required(node).children[2].textContent;
  const width=(node: Element | undefined)=>required(node).children[1].children[0].style.width;
  for(const [id,count] of [['topics-full',64],['topics-paper',4],['features-paper',6]] as const)assert.equal(rows(id).length,count);
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
  assert.match(required(row('features-paper','Voci nuove (< mediana)')).title,/copertura 6\/7/);
  assert.equal(value(row('features-paper','Qualità strutturale alta (Johnson V2, ≥ mediana)')),'— (0/0)');
  assert(!row('features-paper','Biografie di donne'));
  assert(!html.includes('id="features-thresholds"'));
  assert(!descendants(nodes['features-paper']).some(node=>/mediana età/.test(node.textContent)));
  assert.equal(JSON.stringify(raw),before);
  assert(descendants(nodes['features-paper']).some(node=>/revisioni diverse dal grafo SQL/.test(node.textContent)));
  evaluate("data.metadata.measures_rule='creation_quality_creators_v2';paperProfiles()",context);
  assert(descendants(nodes['features-paper']).some(node=>/La copertura esclude revisioni diverse/.test(node.textContent)));
  evaluate("data.metadata.measures_rule='creation_quality_creators_v3';paperProfiles()",context);
  assert.equal(JSON.stringify(raw),before);
  raw.pages[0].topic_status='predicted';raw.pages[0].topic_labels=['Culture.Media.Music','Culture.Media.Films','Culture.Biography.Women','STEM.Physics'];
  raw.pages[1].topic_status='predicted';raw.pages[1].topic_labels=[];
  raw.pages[3].topic_status='predicted';raw.pages[3].topic_labels=['Culture.Media.Music','Geography.Geographical'];
  const withTopics=JSON.stringify(raw);
  evaluate('paperProfiles()',context);
  assert.equal(rows('topics-full').length,64);
  assert.equal(rows('topics-paper').length,4);
  assert.equal(value(row('topics-full','Culture · Media.Music')),'50% (1/2)');
  assert.equal(value(row('topics-full','Culture · Media.Films')),'100% (1/1)');
  assert.equal(value(row('topics-paper','Culture')),'50% (1/2)'); // OR children, not 3 duplicate Culture observations.
  assert.equal(value(row('topics-paper','Geography')),'0% (0/1)');
  assert.equal(width(row('topics-paper','Geography')),'0%');
  assert.equal(value(row('topics-paper','History and Society')),'— (0/0)');
  assert.match(required(row('topics-paper','History and Society')).title,/Nessuna voce soddisfa/);
  assert.match(required(row('topics-paper','Culture')).title,/copertura 3\/7/); // Empty predicted labels are known false.
  assert(!row('features-paper','Biografie di donne'));
  assert.equal(rows('topics-full')[0].children[0].textContent,'Culture · Biography.Women');
  const bars=()=>rows('features-paper').map(node=>[value(node),width(node)]);
  const values=bars();
  nodes['ui-language-toggle'].listeners.click();
  assert.deepEqual(bars(),values);
  assert.match(required(row('features-paper','New articles (< median)')).title,/coverage 6\/7/);
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
  raw.pages.forEach(page=>page.page_created_at=null);evaluate('paperProfiles()',context);
  assert.equal(value(row('features-paper','New articles (< median)')),'— (0/0)');
  // Bot membership does not add article characteristic rows.
  const botFixture={metadata:raw.metadata,pages:[{orphan:true,creator_key:'actor:1',creator_is_bot_now:true},{orphan:true,creator_key:'actor:2',creator_is_bot_now:false}]};
  context.botFixture=botFixture;
  evaluate('data=botFixture',context);
  evaluate('paperProfiles()',context);
  assert(!row('features-paper','Created by a bot'));
  assert(!row('features-paper','Created by a non-bot'));
  botFixture.pages=[];evaluate('paperProfiles()',context);
  assert(rows('features-paper').every(node=>value(node)==='— (0/0)'));
  assert(rows('features-paper').every(node=>!width(node).includes('NaN')));
  console.log('Paper bars: all taxonomy rows, orphan/requirement denominators, counts, multilabel OR, missing/empty states, medians, translations and unchanged inputs passed.');
}

async function checkDeorphanize() {
  const raw=fixture();
  const eligible=raw.pages.filter(page=>page.orphan&&(raw.link_candidates[page.page_id]?.candidate_count??0)>0);
  const {nodes,html}=await harness('deorphanize',raw);
  assert(html.includes('<h1 data-i18n="De-orfanizzare le pagine">De-orfanizzare le pagine</h1>'));
  assert(!html.includes('candidate-coverage'));
  assert(!html.includes('chart-type'));
  assert.equal(nodes['candidate-cards'].children.length,25);
  const card=nodes['candidate-cards'].children[0],footer=findClass(card,'candidate-footer');
  assert.equal(footer.children[0].tag,'span'); // synthetic titles never open Wikipedia
  assert.equal(footer.children[1].textContent,'Connetti');
  assert.match(footer.children[1].href,/^connections\.html\?dataset=fixture\.json&page=\d+$/);
  nodes['candidate-more'].listeners.click();assert.equal(nodes['candidate-cards'].children.length,Math.min(50,eligible.length));
  assert(nodes['candidate-cards'].children.every(card=>eligible.some(page=>page.title===titleLink(card).textContent)));
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
async function checkProgressiveCandidates() {
  const raw=fixture(),summary=fixtureTransport('summary_file');
  raw.metadata.demo=false;summary.metadata=structuredClone(raw.metadata);
  const lmo=structuredClone(raw);lmo.metadata.language='lmo';
  lmo.metadata.creators.language='lmo';lmo.metadata.creators.languages_scanned=['lmo'];
  const lmoSummary=structuredClone(summary);lmoSummary.metadata=structuredClone(lmo.metadata);
  const index={metadata:{kind:'aggregate',language:'all',languages:['vec','lmo'],complete:true,demo:false,
    started_at:raw.metadata.started_at,finished_at:raw.metadata.finished_at},members:[
      {language:'vec',file:'vec.json',page_count:raw.pages.length,metadata:raw.metadata},
      {language:'lmo',file:'lmo.json',page_count:lmo.pages.length,metadata:lmo.metadata}]};
  const entries: ManifestEntry[]=[{language:'all',file:'all.json',kind:'aggregate',date:raw.metadata.finished_at},
    {language:'vec',file:'vec.json',summary_file:'vec-summary.json',date:raw.metadata.finished_at},
    {language:'lmo',file:'lmo.json',summary_file:'lmo-summary.json',date:lmo.metadata.finished_at}];
  const files={'all.json':index,'vec.json':raw,'lmo.json':lmo,'vec-summary.json':summary,'lmo-summary.json':lmoSummary};
  const original=JSON.stringify(files),hold=new Set(['data/vec.json','data/lmo.json']);
  const pending=new Map<string,{signal:AbortSignal;release:()=>void}>();
  const beforeFetch=async(url: string,options: FetchOptions)=>{
    if(!hold.has(url))return;
    await new Promise<void>((resolve,reject)=>{
      const signal=required(options.signal);
      pending.set(url,{signal,release:()=>{hold.delete(url);resolve();}});
      signal.addEventListener('abort',()=>{hold.delete(url);reject(new Error('aborted'));},{once:true});
    });
  };
  const result=await harness('deorphanize',index,'?dataset=all.json',entries,{beforeFetch},files),{nodes,context,document}=result;
  const cards=()=>nodes['candidate-cards'].children.filter(card=>card.className==='candidate-card');
  assert.equal(nodes.dataset.value,'');assert.equal(nodes.dataset.children[0].textContent,'Seleziona');
  assert.match(nodes.status.textContent,/Seleziona una lingua/);
  assert.equal(nodes['dataset-date'].disabled,true);assert.equal(nodes['profile-apply'].disabled,true);
  assert.equal(cards().length,0);
  await evaluate('Promise.all(candidatePreviews.values())',context);
  assert.deepEqual(result.requests.filter(row=>row.url.startsWith('data/')).map(row=>row.url).sort(),
    ['data/manifest.json','data/lmo-summary.json','data/vec-summary.json'].sort());
  nodes.dataset.value='vec';const first=nodes.dataset.listeners.change();
  await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(document.body.dataset.loading,'false');assert.equal(cards().length,0);
  assert.match(nodes['candidate-info'].textContent,/Caricamento dei collegamenti possibili/);
  changeCheckbox(nodes,'candidate-show-without-connections',true);
  assert.equal(cards().length,25);assert.match(nodes['candidate-info'].textContent,/Anteprima: 25/);
  assert.match(nodes.status.textContent,/Caricamento dell’elenco completo/);
  assert.equal(nodes['candidate-more'].hidden,true);
  assert(cards().every(card=>findClass(card,'candidate-badge').textContent==='…'));
  assert(cards().every(card=>findClass(card,'fix-link').getAttribute('aria-disabled')==='true'));
  assert.equal(result.requests.filter(row=>row.url==='data/vec-summary.json').length,1);
  assert(!result.requests.some(row=>row.url.startsWith('https://')));
  const target=required(raw.pages.find(page=>page.orphan&&(raw.link_candidates[page.page_id]?.candidate_count??0)>0&&
    !summary.article_preview?.some(preview=>preview.page_id===page.page_id)));
  changeCheckbox(nodes,'candidate-show-without-connections',false);assert.equal(cards().length,0);
  nodes['candidate-search'].value=target.title;nodes['candidate-search'].listeners.input();
  assert.match(nodes['candidate-info'].textContent,/Caricamento dei collegamenti possibili/);
  required(pending.get('data/vec.json')).release();await first;
  assert.equal(evaluate('candidatePreview',context),false);
  assert.equal(nodes['candidate-search'].value,target.title);
  assert(cards().some(card=>titleLink(card).textContent===target.title));
  assert(!nodes['candidate-info'].textContent.includes('Anteprima'));
  assert.equal(nodes['profile-apply'].disabled,false);
  nodes.dataset.value='lmo';const stale=nodes.dataset.listeners.change();
  await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(cards().length,0);
  changeCheckbox(nodes,'candidate-show-without-connections',true);
  assert.equal(cards().length,25);assert.equal(evaluate('data.metadata.language',context),'lmo');
  assert(cards().every(card=>findClass(card,'connect-button').href.includes('dataset=lmo.json')));
  changeCheckbox(nodes,'candidate-show-without-connections',false);assert.equal(cards().length,0);
  const previous=required(pending.get('data/lmo.json'));
  nodes.dataset.value='vec';await nodes.dataset.listeners.change();await stale;
  assert(previous.signal.aborted);assert.equal(evaluate('data.metadata.language',context),'vec');
  assert(!nodes.status.textContent.includes('Impossibile'));
  assert.equal(result.requests.filter(row=>row.url.endsWith('-summary.json')).length,2);
  nodes.dataset.value='';await nodes.dataset.listeners.change();
  assert.equal(cards().length,0);assert.equal(document.body.dataset.loading,'true');
  nodes['ui-language-toggle'].listeners.click();
  assert.equal(nodes.dataset.children[0].textContent,'Select');assert.match(nodes.status.textContent,/Select a language/);
  nodes.dataset.value='all';await nodes.dataset.listeners.change();
  assert.equal(evaluate('data.pages.length',context),raw.pages.length*2);
  assert.equal(document.body.dataset.loading,'false');
  assert(cards().every(card=>findClass(card,'connect-button').href.includes('language=')));
  const staleSummary=structuredClone(summary);staleSummary.metadata.finished_at='stale';
  const fallback=await harness('deorphanize',raw,'?dataset=vec.json',[entries[1]],{}, {...files,'vec-summary.json':staleSummary});
  assert.equal(fallback.document.body.dataset.loading,'false');
  assert.equal(evaluate('candidatePreview',fallback.context),false);
  assert.equal(JSON.stringify(files),original);
  console.log('Progressive orphan cards: unverified preview counts hidden by default, opt-in previews, bounded preloads, preserved search, replacement, cancellation, aggregate and stale-preview fallback passed.');
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
    assert.equal(required(rows.at(-1)).children[2].textContent,'—');
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
  evaluate('creatorCharts()',context);
  assert.equal(nodes['creator-bots'].children[0].children[2].textContent,'— (0/0)');
  assert.equal(nodes['creator-bots'].children[2].children[2].textContent,'100% (1/1)');
  nodes['ui-language-toggle'].listeners.click();
  assert.match(nodes['creator-bots-note'].textContent,/Current bot status/);
  assert.equal(nodes['creator-bots'].children[0].children[0].textContent,'Bot (current status)');
  console.log('Creator bot chart: current membership, attribution, missing data, empty groups and translations passed.');
}
async function checkCandidateSorting() {
  const raw=fixture();
  const template=required(raw.pages.find(page=>page.orphan));
  raw.pages=['Zero','Beta','Alfa','Unknown','Absent','Highest'].map((title,i)=>({...template,page_id:i+1,title}));
  raw.link_candidates={
    1:{candidate_count:0},2:{candidate_count:2},3:{candidate_count:2},
    4:{candidate_count:null},6:{candidate_count:5}
  };
  const {nodes,html,context}=await harness('deorphanize',raw);
  for(const order of ['candidates-desc','candidates-asc'])assert(html.includes(`value="${order}"`));
  const titles=()=>nodes['candidate-cards'].children.filter(card=>card.className==='candidate-card').map(card=>titleLink(card).textContent);
  assert.deepEqual(titles(),['Highest','Alfa','Beta']);
  for(const [order,expected] of [
    ['candidates-asc',['Alfa','Beta','Highest']],
    ['candidates-desc',['Highest','Alfa','Beta']]
  ] as const) {
    nodes['candidate-sort'].value=order;nodes['candidate-sort'].listeners.input();
    assert.deepEqual(titles(),expected);
  }
  changeCheckbox(nodes,'candidate-show-without-connections',true);
  assert.deepEqual(titles(),['Highest','Alfa','Beta','Zero','Absent','Unknown']);
  nodes['candidate-sort'].value='candidates-asc';nodes['candidate-sort'].listeners.input();
  assert.deepEqual(titles(),['Zero','Alfa','Beta','Highest','Absent','Unknown']);
  changeCheckbox(nodes,'candidate-show-without-connections',false);
  assert.deepEqual(titles(),['Alfa','Beta','Highest']);
  evaluate(`profileScores=new Map(data.pages.map(page=>[pageKey(page),{topic:page.page_id===1?100:1,language:0}]));
    $('profile-sort-option').disabled=false;$('candidate-sort').value='personalized';candidateCards();`,context);
  assert.deepEqual(titles(),['Highest','Alfa','Beta']);
  changeCheckbox(nodes,'candidate-show-without-connections',true);assert.equal(titles()[0],'Zero');
  changeCheckbox(nodes,'candidate-show-without-connections',false);assert.deepEqual(titles(),['Highest','Alfa','Beta']);
  console.log('Candidate sorting: positive counts by default, reversible zero/missing visibility, personalized exclusions, ascending/descending counts and ties passed.');
}
async function checkHiddenPages() {
  const raw=fixture(),template=required(raw.pages.find(page=>page.orphan));
  raw.metadata.demo=false;
  const excluded=['Disambigua','Disanbìgue','Pagine di disambiguazione','  INSIEMI_DI_CARATTERI  ',
    'Disambiguation pages','Homonymie','Pàgines de desambiguació','Páginas de desambiguação','Jeux de caractères',
    'Regístar da pàgin da desambiguazziun'];
  const visible=Array.from({length:30},(_,i)=>({...template,page_id:i+1,title:`Regular ${String(i+1).padStart(2,'0')}`,
    category:i===0?null:i===1?'Disambiguazione linguistica':'Musica'}));
  // Titles and model topics must never substitute for the displayed category.
  visible[2].title='Disambigua';visible[3].title='Insiemi di caratteri';
  visible[4].topic_labels=['STEM.Computing'];visible[5].title='Redirect';
  const hidden=excluded.map((category,i)=>({...template,page_id:100+i,title:`Excluded ${i+1}`,category}));
  const redirects=['  REDIRECT  ','Redirects','Pagine_di_redirect','Redirezioni','Redireccions','Redirecionamentos','Redirections']
    .map((category,i)=>({...template,page_id:300+i,title:`Redirect category ${i+1}`,category}));
  const unavailable=[
    {...template,page_id:201,title:'No connections',category:'Musica'},
    {...template,page_id:202,title:'Unknown connections',category:'Musica'},
    {...template,page_id:203,title:'Absent connections',category:'Musica'}];
  const both={...template,page_id:204,title:'Both hidden',category:'Disambigua'};
  const redirectWithoutConnections={...template,page_id:400,title:'Redirect without connections',category:'Redirect'};
  raw.pages=[...visible,...hidden,...redirects,...unavailable,both,redirectWithoutConnections,{...template,page_id:200,title:'Not orphan',orphan:false,category:'Musica'}];
  raw.link_candidates=Object.fromEntries([...visible,...hidden,...redirects].map(page=>[page.page_id,{status:'compared',candidate_count:1,candidates:[]}]));
  raw.link_candidates[201]={status:'compared',candidate_count:0,candidates:[]};
  raw.link_candidates[202]={status:'missing_item',candidate_count:null,candidates:[]};
  raw.link_candidates[204]={status:'compared',candidate_count:0,candidates:[]};
  raw.link_candidates[400]={status:'compared',candidate_count:0,candidates:[]};
  const original=JSON.stringify(raw),columns=['page_id','title','orphan','category','topic_status','topic_labels','length_bytes','created_at','in_degree'];
  const index: CardIndexFixture={format:'orphan_cards_v2',metadata:raw.metadata,census_page_count:raw.pages.length,
    card_identity:'a'.repeat(64),columns,rows:raw.pages.filter(page=>page.orphan).map(page=>columns.map(key=>page[key]??null)),
    link_candidates:Object.fromEntries(Object.entries(raw.link_candidates).map(([id,record])=>[id,{...required(record),candidate_ids:[]}]))};
  const legacy={format:'orphan_cards_v1',metadata:raw.metadata,census_page_count:raw.pages.length,
    pages:raw.pages.filter(page=>page.orphan),link_candidates:raw.link_candidates};
  const entry: ManifestEntry={language:'vec',file:'fixture.json',date:raw.metadata.finished_at,demo:raw.metadata.demo};
  const files={'fixture.json':raw,'cards.json':index,'legacy-cards.json':legacy};
  for(const transport of [entry,{...entry,cards_file:'legacy-cards.json'},
    {...entry,cards_file:'cards.json',cards_dir:'card-records'}]) {
    const result=await harness('deorphanize',raw,'?dataset=fixture.json',[transport],{},files),{nodes,html}=result;
    const toggle=nodes['candidate-hidden'],options=nodes['candidate-hidden-options'];
    const disambiguation=nodes['candidate-show-disambiguation'],connections=nodes['candidate-show-without-connections'];
    const showRedirects=nodes['candidate-show-redirects'];
    const titles=()=>nodes['candidate-cards'].children.filter(card=>card.className==='candidate-card').map(card=>titleLink(card).textContent);
    assert.match(html,/<div class="list-controls">[\s\S]*?id="candidate-sort"[\s\S]*?id="candidate-hidden"[^>]*aria-expanded="false"[^>]*aria-controls="candidate-hidden-options"/);
    assert(!html.includes('Gli interessi usano'));
    assert.equal(nodes['review-service-status'].textContent,'');
    assert.equal(toggle.getAttribute('aria-expanded'),'false');assert.equal(options.hidden,true);
    assert.equal(disambiguation.checked,false);assert.equal(connections.checked,false);
    assert.equal(showRedirects.checked,false);
    assert.match(nodes['candidate-info'].textContent,/30 orfane/);
    assert.equal(titles().length,25);assert(!titles().some(title=>title.startsWith('Excluded')));
    nodes['candidate-more'].listeners.click();assert.equal(titles().length,30);
    assert(titles().includes('Disambigua'));assert(titles().includes('Insiemi di caratteri'));assert(titles().includes('Redirect'));
    const beforeOpen=titles(),fetches=result.getFetchCount();
    toggle.listeners.click();assert.equal(toggle.getAttribute('aria-expanded'),'true');assert.equal(options.hidden,false);
    assert.deepEqual(titles(),beforeOpen);assert.equal(result.getFetchCount(),fetches);
    toggle.listeners.click();assert.equal(options.hidden,true);assert.deepEqual(titles(),beforeOpen);
    toggle.listeners.click();
    // Independent controls: the overlap requires both; checkbox changes keep the list open.
    for(const [showDisambiguation,redirectsVisible,showWithoutConnections,total] of [
      [true,false,false,40],[false,false,true,33],[true,false,true,44],
      [false,true,false,37],[true,true,false,47],[false,true,true,41],[true,true,true,52],
      [false,false,false,30]
    ] as const) {
      changeCheckbox(nodes,'candidate-show-disambiguation',showDisambiguation);
      changeCheckbox(nodes,'candidate-show-redirects',redirectsVisible);
      changeCheckbox(nodes,'candidate-show-without-connections',showWithoutConnections);
      assert.equal(options.hidden,false);assert.equal(titles().length,25);
      assert.match(nodes['candidate-info'].textContent,new RegExp('^'+total+' orfane'));
      nodes['candidate-more'].listeners.click();assert.equal(titles().length,Math.min(50,total));
      if(total>50)nodes['candidate-more'].listeners.click();
      assert.equal(titles().length,total);
      assert.equal(titles().includes('Excluded 1'),showDisambiguation);
      for(const page of unavailable)assert.equal(titles().includes(page.title),showWithoutConnections);
      assert.equal(titles().includes(both.title),showDisambiguation&&showWithoutConnections);
      for(const page of redirects)assert.equal(titles().includes(page.title),redirectsVisible);
      assert.equal(titles().includes(redirectWithoutConnections.title),redirectsVisible&&showWithoutConnections);
    }
    changeCheckbox(nodes,'candidate-show-disambiguation',true);
    changeCheckbox(nodes,'candidate-show-without-connections',true);
    nodes['candidate-search'].value='Excluded';nodes['candidate-search'].listeners.input();
    assert.equal(titles().length,excluded.length);
    changeCheckbox(nodes,'candidate-show-disambiguation',false);
    assert.equal(titles().length,0);assert.equal(nodes['candidate-more'].hidden,true);
    assert.equal(nodes['candidate-cards'].children[0].textContent,'Nessuna voce orfana corrispondente.');
    nodes['candidate-search'].value='';nodes['candidate-search'].listeners.input();
    nodes['candidate-category'].value='insiemi';nodes['candidate-category'].listeners.input();assert.equal(titles().length,0);
    changeCheckbox(nodes,'candidate-show-disambiguation',true);assert.deepEqual(titles(),['Excluded 4']);
    nodes['candidate-sort'].value='candidates-asc';nodes['candidate-sort'].listeners.input();
    assert.equal(toggle.getAttribute('aria-expanded'),'true');assert.deepEqual(titles(),['Excluded 4']);
    changeCheckbox(nodes,'candidate-show-redirects',true);
    nodes['ui-language-toggle'].listeners.click();
    assert.equal(toggle.textContent,'Hidden pages');
    assert.equal(toggle.title,'Choose which hidden pages to show.');
    assert(result.staticNodes.some(node=>node.textContent==='Show disambiguation pages'));
    assert(result.staticNodes.some(node=>node.textContent==='Show redirect pages'));
    assert(result.staticNodes.some(node=>node.textContent==='Show pages without possible connections'));
    assert.equal(disambiguation.checked,true);assert.equal(connections.checked,true);
    assert.equal(showRedirects.checked,true);
    let prevented=false;
    nodes['candidate-hidden-control'].listeners.keydown({key:'Escape',preventDefault(){prevented=true;}});
    assert(prevented);assert.equal(options.hidden,true);assert.equal(toggle.getAttribute('aria-expanded'),'false');
    assert.equal(toggle.focused,true);assert.deepEqual(titles(),['Excluded 4']);
    toggle.listeners.click();assert.equal(options.hidden,false);
    assert.equal(disambiguation.checked,true);assert.equal(connections.checked,true);
    assert.equal(showRedirects.checked,true);
    assert(!result.requests.some(request=>request.url.startsWith('https://')||request.url.startsWith('data/card-records/')));
  }
  const articles=await harness('articles',raw);
  articles.nodes['article-search'].value='Excluded';articles.nodes['article-search'].listeners.input();
  assert.equal(articles.nodes['article-rows'].children.length,excluded.length);
  assert.equal(JSON.stringify(raw),original);
  console.log('Hidden pages: disclosure without filtering, independent checkboxes/overlap, unchecked defaults, Escape/focus, canonical/legacy/index parity, pagination, translations and unchanged data passed.');
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
  assert(!descendants(nodes['connection-candidates'].children[1]).some(node=>node.className==='dead-end-badge'));
  raw.metadata.demo=true;evaluate('renderConnections()',context);
  assert(!descendants(nodes['connection-candidates']).some(n=>n.tag==='a')); // synthetic links never open Wikipedia
  raw.link_candidates['1']={status:'compared',candidate_count:0,candidates:[]};evaluate('renderConnections()',context);
  assert.match(nodes['connection-info'].textContent,/Nessuna pagina candidata/);
  raw.link_candidates['1']={status:'missing_item',candidate_count:null,candidates:[]};evaluate('renderConnections()',context);
  assert.match(nodes['connection-info'].textContent,/non disponibile/);
  raw.link_candidates['1']={candidate_count:60,candidates:Array.from({length:60},(_,i)=>({page_id:i+2,title:`Fonte ${i}`,evidence:[]}))};evaluate('renderConnections()',context);
  assert.equal(nodes['connection-candidates'].children.length,50);
  nodes['connection-more'].listeners.click();assert.equal(nodes['connection-candidates'].children.length,60);
  assert(isRecord(context.window)&&isRecord(context.window.location));
  context.window.location.search='?dataset=fixture.json&page=999';
  assert.throws(()=>evaluate('renderConnections()',context),/non presente/);
  const invalid=await harness('connections',raw,'?dataset=..%2Fsecret.json&page=1');
  assert.match(invalid.nodes.status.textContent,/Raccolta richiesta non disponibile/);
  console.log('Connections page: proposed/existing links, dead-end priority, pagination, safety and invalid routes passed.');
}
function assertAnchorInstructions(prompt: string) {
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
function assertAccessInstructions(prompt: string) {
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
  const candidates: FixtureCandidate[]=Array.from({length:40},(_,i)=>({page_id:i+2,title:`Fonte ${String(i).padStart(2,'0')}`,dead_end:i>=35,evidence:[{language:'lmo',source_title:`Reference ${i}`,target_title:'Target B'}]}));
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
    assert(fix().title.includes(required(({qwen:'Qwen',kimi:'Kimi',mistral:'Mistral',glm:'GLM'})[provider])));
    assert.equal(new URL(url).search,'');
  }
  nodes['ui-language-toggle'].listeners.click();assert.equal(result.getFetchCount(),fetches);
  prompt=await preparePrompt(result,fix());assert.match(prompt,/Reply in English/);
  assert.match(prompt,/Page link \(language to edit\) \| Page link \(reference language\)/);
  assert.equal(nodes['llm-prompt-open'].href,'https://chat.qwen.ai/');
  nodes['ui-language-toggle'].listeners.click();
  raw.link_candidates[1]={candidate_count:2,candidates:[candidates[0],candidates[35],candidates[35],{page_id:1,title:page.title},{page_id:0,title:'Invalid'},{page_id:99,title:null}]};
  evaluate('candidateCards()',context);
  prompt=await preparePrompt(result,fix());
  const entries=prompt.split('Candidate source pages X:\n')[1].split('\n').filter(line=>/^\d+\. LOCAL_X/.test(line));
  assert.equal(entries.length,2);assert.match(entries[0],/Dead-end page/);assert.match(entries[1],/dead_end=false/);
  const unknown: FixtureCandidate={page_id:90,title:'Unknown à & #',evidence:[{language:'lmo',source_title:'Reference unknown',source_page_id:501,target_title:'Target B',target_page_id:601}]};
  raw.link_candidates[1]={candidate_count:1,candidates:[unknown]};evaluate('candidateCards()',context);
  prompt=await preparePrompt(result,fix());assert.match(prompt,/dead_end=unknown/);
  assert(sourceMetadata(prompt).some(row=>row.language==='lmo'&&row.page_id===501));
  assert(sourceMetadata(prompt).some(row=>row.language==='lmo'&&row.page_id===601));
  required(unknown.evidence)[0].source_page_id='501&action=submit';required(unknown.evidence)[0].target_page_id=-1;
  const before=result.requests.length;
  evaluate('candidateCards()',context);await preparePrompt(result,fix());
  assert(result.requests.slice(before).some(row=>new URL(row.url).searchParams.get('titles')==='Reference unknown'));
  assert(result.requests.slice(before).every(row=>new URL(row.url).searchParams.get('action')==='query'));
  changeCheckbox(nodes,'candidate-show-without-connections',true);
  for(const record of [undefined,{candidate_count:null,candidates},{candidate_count:0,candidates:[]},{candidate_count:1,candidates:[{page_id:2,title:'No evidence',evidence:[{language:'lmo.evil',source_title:'X',target_title:'Y'}]}]}]) {
    raw.link_candidates[1]=record;evaluate('candidateCards()',context);assert.equal(fix().tag,'span');assert.equal(fix()['aria-disabled'],'true');
  }
  raw.link_candidates[1]={candidate_count:40,candidates};raw.metadata.demo=true;evaluate('candidateCards()',context);
  assert.equal(fix().textContent,'Fix this · DEMO');assert(!fix().listeners.click);assert(!titleLink(card()).href);
  console.log('Fix this: click-only revisions, twelve deduplicated complete sources, scope/role instructions, explicit clipboard, safe provider URLs, dead-end ranking and unchanged data passed.');
}
async function checkSingleConnectionPrompts() {
  const raw=fixture();raw.metadata.demo=false;raw.metadata.language='roa-tara';
  raw.pages=[{page_id:1,title:'Orphan à & #',orphan:true,category:'Tema'}];
  const chosen: FixtureCandidate={page_id:90,title:'Chosen à & #',dead_end:false,evidence:['lmo','it','fur'].map((language,i)=>({language,source_title:`Ref ${language}`,source_page_id:100+i,target_title:`Target ${language}`,target_page_id:200+i}))};
  const others=Array.from({length:6},(_,i)=>({page_id:i+2,title:`Other ${i}`,dead_end:true,evidence:[{language:'lmo',source_title:`Unrelated ${i}`,target_title:'Other target'}]}));
  raw.link_candidates={1:{candidate_count:7,candidates:[...others,chosen]}};
  const before=JSON.stringify(raw),result=await harness('connections',raw,'?dataset=fixture.json&page=1'),{nodes,context}=result;
  const card=()=>nodes['connection-candidates'].children.find(row=>titleLink(row).textContent===chosen.title),fix=()=>findClass(card(),'fix-link');
  for(const [provider,url] of [['qwen','https://chat.qwen.ai/'],['kimi','https://www.kimi.ai/'],['mistral','https://chat.mistral.ai/chat'],['glm','https://chat.z.ai/'],['chatgpt','https://chatgpt.com/'],['claude','https://claude.ai/new'],['gemini','https://gemini.google.com/app']]) {
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
  required(chosen.evidence).push(...['nap','sc','en','fr'].map((language,i)=>({language,source_title:`Ref ${language}`,source_page_id:300+i,target_title:`Target ${language}`,target_page_id:400+i})));
  evaluate('renderConnections()',context);
  const prompt=await preparePrompt(result,fix());
  assert.equal((prompt.match(/Observed reference edge/g)||[]).length,7);assert.equal(sourceMetadata(prompt).length,16);
  assert(!prompt.includes('Other_'));assert(!prompt.includes('up to 5'));
  for(const evidence of [[],[{language:'lmo.evil',source_title:'Bad',target_title:'Bad'}]]) {
    chosen.evidence=evidence;evaluate('renderConnections()',context);assert.equal(fix().tag,'span');assert.equal(fix()['aria-disabled'],'true');
    assert(nodes['connection-candidates'].children.some(row=>findClass(row,'fix-link').tag==='button'));
  }
  console.log('Single connection: isolated candidate beyond top five, all seven reference pairs without truncation, seven plain chat URLs and unchanged data passed.');
}
async function checkLlmSelection() {
  const raw=fixture();raw.metadata.demo=false;
  raw.pages=Array.from({length:30},(_,i)=>({page_id:i+1,title:`Target ${i}`,orphan:true,category:'Tema'}));
  const record={candidate_count:1,candidates:[{page_id:99,title:'Local source',dead_end:true,evidence:[{language:'lmo',source_title:'Reference source',target_title:'Reference target'}]}]};
  raw.link_candidates=Object.fromEntries(raw.pages.map(page=>[page.page_id,record]));
  const result=await harness('deorphanize',raw),{nodes,storage}=result;
  assert.deepEqual(nodes['llm-provider'].children.map(option=>option.textContent),['ChatGPT','Gemini','Claude','Qwen','Kimi','GLM','Mistral','Altro']);
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
  // Every added provider retains the same complete prompt and explicit copy workflow.
  const selection=await harness('connections',raw,'?dataset=fixture.json&page=1');
  const initialFetches=selection.getFetchCount();
  let completePrompt: string | undefined;
  for(const [provider,url] of [['chatgpt','https://chatgpt.com/'],['claude','https://claude.ai/new'],['gemini','https://gemini.google.com/app'],['other',null]] as const) {
    const before=selection.getFetchCount(),copies=selection.copied.length;
    selection.nodes['llm-provider'].value=provider;selection.nodes['llm-provider'].listeners.change();
    assert.equal(selection.getFetchCount(),before);assert.equal(selection.copied.length,copies);
    const prompt=await preparePrompt(selection,findClass(selection.nodes['connection-candidates'].children[0],'fix-link'));
    // Each explicit preparation retrieves documents again; acquisition times can differ.
    const comparable=prompt.replace(/"retrieved_at": "[^"]+"/g,'"retrieved_at": "TIMESTAMP"');
    if(completePrompt===undefined)completePrompt=comparable;else assert.equal(comparable,completePrompt);
    assert.equal(selection.nodes['llm-prompt-open'].hidden,url===null);
    assert.equal(selection.nodes['llm-prompt-open'].href,url??'');
    if(url)assert.equal(new URL(selection.nodes['llm-prompt-open'].href).search,'');
    await selection.nodes['llm-prompt-copy'].listeners.click();assert.equal(selection.copied.at(-1),prompt);
    assert.equal(selection.storage.get('orphanwiki-llm'),provider);
  }
  assert(selection.getFetchCount()>initialFetches);
  const restored=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{storage:selection.storage});
  assert.equal(restored.nodes['llm-provider'].value,'other');
  assert.equal(required(restored.nodes['llm-provider'].children.at(-1)).textContent,'Altro');
  restored.nodes['ui-language-toggle'].listeners.click();
  assert.equal(restored.nodes['llm-provider'].value,'other');
  assert.equal(required(restored.nodes['llm-provider'].children.at(-1)).textContent,'Other');
  await preparePrompt(restored,findClass(restored.nodes['connection-candidates'].children[0],'fix-link'));
  assert.equal(restored.nodes['llm-prompt-title'].textContent,'Prompt for Other');assert.equal(restored.nodes['llm-prompt-open'].hidden,true);
  restored.nodes['llm-provider'].value='chatgpt';restored.nodes['llm-provider'].listeners.change();
  await preparePrompt(restored,findClass(restored.nodes['connection-candidates'].children[0],'fix-link'));
  assert.equal(restored.nodes['llm-prompt-open'].hidden,false);assert.equal(restored.nodes['llm-prompt-open'].href,'https://chatgpt.com/');
  console.log('Provider selection: preferences and pagination retained, no background sources/clipboard, explicit manual copy fallback, translations and clearing on dataset change passed.');
}
async function checkWikitextPreparation() {
  const raw=fixture();raw.metadata.demo=false;
  raw.pages=[{page_id:1,title:'Michael Douglas',orphan:true}];
  raw.link_candidates={1:{candidate_count:1,candidates:[{page_id:2,title:'Michael J. Fox',dead_end:false,evidence:[{language:'co',source_page_id:2,source_title:'Michael J. Fox',target_page_id:1,target_title:'Michael Douglas'}]}]}};
  const full='{{Infobox}}\n{| class="wikitable"\n| 1995\n| '+"''[[The American President]]''"+'\n| Lewis Rothschild\n|\n|}\n'+('Testo ł à.\n'.repeat(4000))+'\n````\nFINE INTEGRALE';
  const sources: Record<string,ReturnType<typeof revisionContent>>={
    'vec:1':revisionContent(1,'Michael Douglas','Michael Douglas, ator.'),
    'vec:2':revisionContent(2,'Michael J. Fox',full),
    'co:1':revisionContent(1,'Michael Douglas','Michael Douglas, attore.'),
    'co:2':revisionContent(2,'Michael J. Fox',"* ''[[The American President]]'' (1995), cù [[Michael Douglas]]")
  };
  const key=(url: URL)=>url.hostname.split('.')[0]+':'+url.searchParams.get('pageids');
  const control=(result: Awaited<ReturnType<typeof harness>>)=>findClass(result.nodes['connection-candidates'].children[0],'fix-link');
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
    assert(required(metadata.revision_url).endsWith('oldid='+expected.revid));
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
  const mutations: ((payload: ReturnType<typeof revisionContent>)=>unknown)[]=[
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
    const broken=structuredClone(sources['vec:2']);mutate(broken);
    const test=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:url=>key(url)==='vec:2'?broken:sources[key(url)]});
    const partial=await preparePrompt(test,control(test)),metadata=sourceMetadata(partial);
    assert.equal(metadata.filter(row=>row.status==='READABLE').length,3);
    assert.equal(required(metadata.find(row=>row.language==='vec'&&row.title==='Michael J. Fox')).status,'UNREADABLE');
    assert(!partial.includes(full));assert.match(test.nodes['llm-prompt-status'].textContent,/3\/4/);
  }
  const empty=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:url=>key(url)==='vec:2'?revisionContent(2,'Michael J. Fox',''):sources[key(url)]});
  const emptyPrompt=await preparePrompt(empty,control(empty));
  assert.equal(required(sourceMetadata(emptyPrompt).find(row=>row.language==='vec'&&row.page_id===2)).utf8_bytes,0);
  const failed=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:()=>({error:{code:'blocked'}})});
  await preparePrompt(failed,control(failed));assert.equal(failed.nodes['llm-prompt-copy'].disabled,true);
  assert.match(failed.nodes['llm-prompt-status'].textContent,/Nessuna fonte disponibile/);
  const gates: (()=>void)[]=[];
  const pending=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:url=>new Promise<RevisionPayload>(resolve=>gates.push(()=>resolve(sources[key(url)])))});
  const work=control(pending).listeners.click();await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(gates.length,3);assert.equal(pending.nodes['llm-prompt-copy'].disabled,true);
  pending.nodes['llm-prompt-debug'].checked=true;pending.nodes['llm-prompt-debug'].listeners.change();
  assert.equal(pending.nodes['llm-prompt-text'].value,'');
  pending.nodes['llm-prompt-close'].listeners.click();
  assert(pending.requests.filter(row=>row.url.startsWith('https://')).every(row=>required(row.signal).aborted));
  gates.forEach(resolve=>resolve());await work;
  assert.equal(pending.nodes['llm-prompt-text'].value,'');assert.equal(pending.nodes['llm-prompt'].open,false);
  assert.equal(pending.requests.filter(row=>row.url.startsWith('https://')).length,3);
  // A superseded preparation must not overwrite a newer source package.
  let release: (()=>void) | undefined;let hold=true;
  const racing=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{apiResponse:url=>hold&&key(url)==='vec:2'?
    new Promise<RevisionPayload>(resolve=>{release=()=>resolve(sources[key(url)]);}):sources[key(url)]});
  const old=control(racing).listeners.click();
  await new Promise<void>(resolve=>setImmediate(resolve));assert.equal(typeof release,'function');hold=false;
  const latest=await preparePrompt(racing,control(racing));required(release)();await old;
  assert.equal(racing.nodes['llm-prompt-text'].value,latest);
  racing.nodes['llm-prompt'].listeners.cancel({preventDefault(){}});
  assert.equal(racing.nodes['llm-prompt-text'].value,'');assert.equal(racing.nodes['llm-prompt'].open,false);
  // Missing and known IDs for the same reference are one document; different wikis stay separate.
  required(required(required(raw.link_candidates[1]).candidates)[0].evidence).push({language:'co',source_title:'Michael J. Fox',target_title:'Michael Douglas'});
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
        assert.equal(node.textContent,evaluate(`t(${key})`,context));
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
      assert.equal(evaluate('number(1234.5)',context),'1,234.5');
      nodes['article-group'].value='all';nodes['article-group'].listeners.input();
      nodes['article-more'].listeners.click();
      nodes['ui-language-toggle'].listeners.click();
      assert.equal(nodes['article-rows'].children.length,50);
      assert.equal(nodes['article-group'].value,'all');
      assert.deepEqual(nodes.age.children[0].children.filter(node=>node.tag==='rect').map(node=>node.height),heights);
      assert.equal(evaluate('number(1234.5)',context),new Intl.NumberFormat('it-IT',{maximumFractionDigits:1}).format(1234.5));
    } else if(page==='creators') {
      assert.match(nodes['creator_prior_edits_main-note'].textContent,/Prior article edits.*Scanned languages: VEC/);
      assert.match(required(nodes['creator_prior_edits_main-chart'].children[0].children.at(-1)).textContent,/edits \(logarithmic scale\)/);
      assert(nodes['creator-rows'].children.some(row=>row.children[1].textContent==='Registered'));
      assert(descendants(nodes.origins).some(node=>node.textContent==='Tagged translation'));
    } else if(page==='deorphanize') {
      const footer=findClass(nodes['candidate-cards'].children[0],'candidate-footer');
      assert.equal(footer.children[1].textContent,'Connect');
      assert.match(footer.children[1]['aria-label'],/^Connect Voce dimostrativa/);
      assert(nodes['candidate-cards'].children.every(card=>raw.pages.some(p=>p.title===titleLink(card).textContent)));
      nodes['candidate-more'].listeners.click();
      nodes['ui-language-toggle'].listeners.click();
      assert.equal(nodes['candidate-cards'].children.length,Math.min(50,raw.pages.filter(p=>p.orphan&&(raw.link_candidates[p.page_id]?.candidate_count??0)>0).length));
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
  evaluate("theme='system';applyTheme()",result.context);
  result.media.matches=true;result.media.listener();assert.equal(result.document.documentElement.dataset.theme,'dark');
  result.media.matches=false;result.media.listener();assert.equal(result.document.documentElement.dataset.theme,'light');
  result=await harness('articles',fixture(),undefined,null,{storage:new Map([['orphanwiki-language','bad'],['orphanwiki-theme','bad']]),systemDark:true});
  assert.equal(result.document.documentElement.lang,'it');assert.equal(evaluate('theme',result.context),'system');
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
    const result=await harness(page,index,route,entries,page==='deorphanize'?{chooseLanguage:'all'}:{},files),{nodes,context}=result;
    assert(!nodes.status.textContent.includes('Impossibile caricare'),nodes.status.textContent);
    assert.equal(result.requests.filter(request=>request.url.startsWith('data/')).length,4);
    assert(result.requests.every(request=>request.cache==='no-store'));
    assert(!nodes.source);
    assert.equal(evaluate('data.metadata.creators.collected_languages',context),2);
    assert.equal(evaluate('data.pages.length',context),4);
    assert.equal(evaluate('data.pages.filter(p=>p.orphan).length',context),3);
    assert.equal(evaluate('new Set(data.pages.map(attributedCreatorKey)).size',context),2);
    assert.equal(evaluate("label(data.pages[0])",context),'VEC · Shared category');
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
      let cards=nodes['candidate-cards'].children;
      assert.equal(cards.length,1);
      assert.match(findClass(cards[0],'connect-button').href,/language=vec/);
      assert.equal(findClass(cards[0],'candidate-badge').textContent,'1');
      changeCheckbox(nodes,'candidate-show-without-connections',true);cards=nodes['candidate-cards'].children;
      assert.equal(cards.length,3);
      assert.equal(findClass(cards[2],'candidate-badge').textContent,'—');
    }
    if(page==='connections') {
      assert.match(titleLink(nodes['connection-candidates'].children[0]).href,/vec\.wikipedia/);
      assert(descendants(nodes['connection-candidates']).some(n=>n.href?.includes('lmo.wikipedia')));
      const prompt=await preparePrompt(result,findClass(nodes['connection-candidates'].children[0],'fix-link'));
      assert(prompt.includes('https://vec.wikipedia.org/wiki/'));assert(!prompt.includes('https://all.wikipedia.org'));
    }
    nodes['ui-language-toggle'].listeners.click();
    if(nodes.dataset)assert.equal(nodes.dataset.children[0].textContent,page==='deorphanize'?'Select':'All languages');
    assert(!nodes.source);
  }
  let result=await harness('connections',index,'?dataset=all.json&page=1',entries,{},files);
  assert.match(result.nodes.status.textContent,/Pagina orfana non presente/);
  result=await harness('connections',index,'?dataset=all.json&page=1&language=en',entries,{},files);
  assert.match(result.nodes.status.textContent,/Pagina orfana non presente/);
  const bad=structuredClone(index);bad.members[0].file='outside.json';
  result=await harness('articles',bad,'?dataset=all.json',entries,{}, {...files,'all.json':bad});
  assert.match(result.nodes.status.textContent,/Indice aggregato non valido/);
  const changed=structuredClone(b);changed.metadata.input_sha256='stale';
  result=await harness('articles',index,'?dataset=all.json',entries,{}, {...files,'lmo.json':changed});
  assert.match(result.nodes.status.textContent,/Indice aggregato non valido/);
  const partial=structuredClone(b);partial.metadata.creators={status:'not_collected'};
  for(const p of partial.pages) {p.creator_key=null;p.creator_prior_articles=null;}
  const partialIndex=structuredClone(index);partialIndex.members[1].metadata=partial.metadata;
  result=await harness('creators',partialIndex,'?dataset=all.json',entries,{}, {...files,'all.json':partialIndex,'lmo.json':partial});
  assert.equal(evaluate('data.metadata.creators.collected_languages',result.context),1);
  assert.equal(result.nodes['creator-rows'].children.length,1);
  assert.equal(result.nodes['creators-empty'].hidden,true);
  console.log('Aggregation: weighted page populations, local identities, all four pages, evidence, missing creators, translations and invalid/stale routes passed.');
}
async function checkCompactTransport() {
  const a=fixture(),b=fixture();a.metadata.demo=b.metadata.demo=false;
  b.metadata.language='lmo';b.metadata.creators.language='lmo';
  a.pages=a.pages.slice(0,3);b.pages=b.pages.slice(0,5);
  for(const raw of [a,b])for(const page of raw.pages){page.topic_status='predicted';page.topic_labels=['Culture.Media.Music'];}
  const pack=(raw: LoadedFixture)=>{const columns=[...new Set<string>(raw.pages.flatMap(Object.keys))];return {format:'observations_v1',metadata:raw.metadata,columns,
    rows:raw.pages.map(page=>columns.map(key=>page[key]??null)),link_candidates:Object.fromEntries(Object.entries(raw.link_candidates).map(([key,record])=>[key,{status:required(record).status,candidate_count:required(record).candidate_count}]))};};
  const index={metadata:{kind:'aggregate',language:'all',languages:['vec','lmo'],complete:true,demo:false,started_at:a.metadata.started_at,finished_at:a.metadata.finished_at},members:[{language:'vec',file:'vec.json',page_count:3,metadata:a.metadata},{language:'lmo',file:'lmo.json',page_count:5,metadata:b.metadata}]};
  const entries: ManifestEntry[]=[{language:'all',file:'all.json',kind:'aggregate',date:a.metadata.finished_at},...['vec','lmo'].map(language=>({language,file:language+'.json',kind:'wiki',observations_file:language+'-observations.json',date:a.metadata.finished_at}))];
  const files={'all.json':index,'vec-observations.json':pack(a),'lmo-observations.json':pack(b),'vec.json':a,'lmo.json':b};
  const before=JSON.stringify(files);
  for(const page of ['articles','creators']) {
    const result=await harness(page,index,'?dataset=all.json',entries,{},files);
    assert.equal(result.document.body.dataset.loading,'false');
    assert(!result.nodes.status.textContent.includes('Impossibile'),result.nodes.status.textContent);
    assert.deepEqual(result.requests.map(r=>r.url),['data/manifest.json','data/all.json','data/vec-observations.json','data/lmo-observations.json']);
    assert.equal(evaluate('data.pages.length',result.context),8);
    assert(!result.nodes.source);
    const full=await harness(page,index,'?dataset=all.json',entries.map(({observations_file,...entry})=>entry),{},files);
    for(const id of page==='articles'?['topics-full','topics-paper','features-paper','categories']:['origins','creator-bots']) {
      const values=(nodes: Record<string,Element>)=>nodes[id].children.filter(node=>node.className==='bar-row').map(node=>[node.children[0].textContent,node.children[1].children[0].style.width,node.children[2].textContent]);
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
  assert.equal(evaluate('articleLabels(data.pages[32])[0]',context),'Topics unavailable');
  assert.equal(evaluate('articleLabels(data.pages[33])[0]',context),'No topic above threshold');
  nodes['article-classification'].value='categories';nodes['article-classification'].listeners.change();
  assert.equal(nodes['article-rows'].children[0].children[1].textContent,'Music category');
  assert.equal(JSON.stringify(raw),before);
  console.log('Article classification: Wikipedia categories, every macro/topic choice, multilabel filtering/sorting, missing versus empty labels, pagination, safe translations and unchanged data passed.');
}

async function checkEditorialTransports() {
  // Aggregate card loading must use compact transports for every member.
  // Canonical files are absent, so accidentally loading them fails this test.
  const languages=['co','eml','fur','lij','lmo','nap','pms','roa-tara','sc','scn','vec'];
  const entries: ManifestEntry[]=[],members: AggregateMember[]=[],files: Record<string,unknown>={};
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
  const all=await harness('deorphanize',index,'?dataset=all.json',entries,{chooseLanguage:'all'},files);
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
    (card: FixtureDataset)=>card.metadata={...card.metadata,input_sha256:'stale'},
    (card: FixtureDataset)=>card.census_page_count=3,
    (card: FixtureDataset)=>card.pages[0].orphan=false,
    (card: FixtureDataset)=>required(card.link_candidates)['999']={},
    (card: FixtureDataset)=>card.format='observations_v1'
  ]) {
    const bad=structuredClone(files),card=bad['pms-cards.json'];
    assert(isFixture(card));mutation(card);
    const invalid=await harness('deorphanize',index,'?dataset=all.json',entries,{chooseLanguage:'all'},bad);
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
  await checkCardIndex();
  await checkSharedConnectionMetadata();
}
async function checkSharedConnectionMetadata() {
  const raw=fixture();raw.metadata.demo=false;
  const entries=readManifest(path.join(fixtureDir,'manifest.json'));
  const entry={...required(entries.find(entry=>entry.file==='fixture.json')),demo:false};
  const shared=fixtureTransport('connections_metadata_file');shared.metadata.demo=false;
  const key=Object.keys(raw.link_candidates).find(key=>raw.link_candidates[key]?.candidates?.length);
  assert(key);
  assert(entry.connections_dir);
  const detail=editorialRecord(JSON.parse(fs.readFileSync(path.join(fixtureDir,entry.connections_dir,key+'.json'),'utf8')));
  assert.equal(detail.format,'connections_v2');assert.equal(detail.metadata,undefined);
  const {connection_identity,...body}=detail;
  const legacyDetail={...body,format:'connections_v1',metadata:shared.metadata};
  const oldEntry={...entry,connections_dir:'legacy-details'};delete oldEntry.connections_metadata_file;
  const lmoMetadata=fixtureTransport('connections_metadata_file','lmo-demo.json').metadata;lmoMetadata.demo=false;
  const lmo={...required(entries.find(entry=>entry.file==='lmo-demo.json')),demo:false};
  const index={metadata:{language:'all',kind:'aggregate',complete:true,demo:false,languages:['vec','lmo'],
    started_at:raw.metadata.started_at,finished_at:raw.metadata.finished_at},members:[
      {language:'vec',file:entry.file,page_count:shared.census_page_count,metadata:shared.metadata},
      {language:'lmo',file:lmo.file,page_count:240,metadata:lmoMetadata}]};
  const all={language:'all',file:'all.json',date:index.metadata.finished_at,kind:'aggregate'};
  const detailFile=entry.connections_dir+'/'+key+'.json';
  const sharedName=required(entry.connections_metadata_file);
  const files: Record<string,unknown>={'all.json':index,[detailFile]:detail,[sharedName]:shared,
    ['legacy-details/'+key+'.json']:legacyDetail};
  const search=`?dataset=all.json&page=${key}&language=vec`;
  const old=await harness('connections',raw,search,[all,oldEntry,lmo],{},files);
  const next=await harness('connections',raw,search,[all,entry,lmo],{},files);
  const logical=(result: Awaited<ReturnType<typeof harness>>)=>evaluate('JSON.stringify({metadata:data.metadata,pages:data.pages,link_candidates:data.link_candidates})',result.context);
  assert.equal(logical(next),logical(old));
  assert.deepEqual(next.requests.filter(request=>request.url.startsWith('data/')).map(request=>request.url),
    ['data/manifest.json','data/all.json','data/'+detailFile,'data/'+entry.connections_metadata_file]);
  assert(!next.requests.some(request=>request.url.startsWith('https://')));
  const oldPrompt=await preparePrompt(old,old.nodes['connection-copy-prompt']);
  const nextPrompt=await preparePrompt(next,next.nodes['connection-copy-prompt']);
  const stable=(prompt: string)=>prompt.replace(/"retrieved_at": "[^"]+"/g,'"retrieved_at": "<retrieval>"');
  assert.equal(stable(nextPrompt),stable(oldPrompt));assert.equal(next.copied.length,0);
  for(const mutate of [
    (documents: Record<string,unknown>)=>sharedMetadata(documents[sharedName]).connection_identity='b'.repeat(64),
    (documents: Record<string,unknown>)=>sharedMetadata(documents[sharedName]).census_page_count++,
    (documents: Record<string,unknown>)=>sharedMetadata(documents[sharedName]).metadata.language='lmo',
    (documents: Record<string,unknown>)=>sharedMetadata(documents[sharedName]).metadata.finished_at='old',
    (documents: Record<string,unknown>)=>sharedMetadata(documents[sharedName]).format='wrong',
    (documents: Record<string,unknown>)=>editorialRecord(documents[detailFile]).connection_identity='b'.repeat(64),
    (documents: Record<string,unknown>)=>editorialRecord(documents[detailFile]).metadata=shared.metadata,
    (documents: Record<string,unknown>)=>editorialRecord(documents[detailFile]).census_page_count++,
    (documents: Record<string,unknown>)=>editorialRecord(documents[detailFile]).format='connections_v1',
    (documents: Record<string,unknown>)=>delete documents[sharedName]
  ]) {
    const bad=structuredClone(files);mutate(bad);
    const rejected=await harness('connections',raw,search,[all,entry,lmo],{},bad);
    assert.match(rejected.nodes.status.textContent,/Impossibile caricare/);
    assert.equal(rejected.nodes['connection-copy-prompt'].disabled,true);
    assert(!rejected.requests.some(request=>request.url.startsWith('https://')));
  }
  const unsafe=await harness('connections',raw,search,[all,{...entry,connections_metadata_file:'../outside.json'},lmo],{},files);
  assert.match(unsafe.nodes.status.textContent,/Nome dataset non valido/);
  assert(!unsafe.requests.some(request=>request.url.includes('outside')));
  const undeclared={...entry};delete undeclared.connections_metadata_file;
  const rejected=await harness('connections',raw,search,[all,undeclared,lmo],{},files);
  assert.match(rejected.nodes.status.textContent,/Impossibile caricare/);
  console.log('Shared connection metadata: generated v2 details reconstruct v1 pages/evidence and prompts, focused aggregate loads only its own metadata, stale/mixed/unsafe sources rejected.');
}
async function checkCardIndex() {
  const raw=fixture(),template=raw.pages[0],columns=['page_id','title','orphan','category','topic_status','topic_labels','length_bytes','created_at','in_degree'];
  raw.metadata.demo=false;
  const targets: FixturePage[]=Array.from({length:300},(_,i)=>({...template,page_id:i+1,title:`Target ${String(i+1).padStart(3,'0')}`,
    orphan:true,in_degree:0,category:i%2?'Scritori':'Musica',length_bytes:i%7?i*50:null,created_at:i%11?'2020-01-01T00:00:00Z':null,
    topic_status:i%3?'predicted':'missing',topic_labels:i%3===1?['Culture.Media.Music']:i%3===2?[]:null}));
  const sources: FixturePage[]=Array.from({length:12},(_,i)=>({...template,page_id:1000+i,title:`Source ${i}`,orphan:false}));
  raw.pages=[...targets,...sources];raw.link_candidates={};
  const index: CardIndexFixture={format:'orphan_cards_v2',metadata:raw.metadata,census_page_count:raw.pages.length,card_identity:'a'.repeat(64),
    columns,rows:targets.map(page=>columns.map(key=>page[key]??null)),link_candidates:{}};
  const legacy: EditorialRecord & {metadata: FixtureMetadata}={format:'orphan_cards_v1',metadata:raw.metadata,census_page_count:raw.pages.length,pages:targets,link_candidates:{}};
  const entry={language:'vec',file:'fixture.json',date:raw.metadata.finished_at,demo:false,cards_file:'card-index.json',cards_dir:'card-records',connections_dir:'details'};
  const legacyEntry: ManifestEntry={...entry,cards_file:'legacy-cards.json'};delete legacyEntry.cards_dir;
  const files: Record<string,unknown>={'fixture.json':raw,'card-index.json':index,'legacy-cards.json':legacy};
  for(const target of targets) {
    const candidates=sources.map((source,i)=>({page_id:source.page_id,title:source.title,out_degree:i<5?0:2,dead_end:i<5,
      evidence:[{language:'lmo',source_page_id:3000+i,source_title:`Reference ${i}`,target_page_id:2000+target.page_id,target_title:`Reference ${target.page_id}`},
        {language:'pms',source_page_id:4000+i,source_title:`Other ${i}`,target_page_id:5000+target.page_id,target_title:`Other ${target.page_id}`}]}));
    const full={status:'compared',candidate_count:12,candidates};
    const shortlist={...full,candidates:candidates.slice(0,10).map(candidate=>({...candidate,evidence:candidate.evidence.slice(0,1)}))};
    raw.link_candidates[target.page_id]=full;legacy.link_candidates[target.page_id]=shortlist;
    index.link_candidates[target.page_id]={status:'compared',candidate_count:12,candidate_ids:shortlist.candidates.map(candidate=>candidate.page_id)};
    files[`card-records/${target.page_id}.json`]={format:'orphan_card_v1',card_identity:index.card_identity,
      census_page_count:raw.pages.length,page:target,candidate_record:shortlist};
    files[`details/${target.page_id}.json`]={format:'connections_v1',metadata:raw.metadata,
      census_page_count:raw.pages.length,pages:[target],link_candidates:{[target.page_id]:full}};
  }
  for(const [id,count] of [[299,0],[300,null]] as const) {
    const record={status:count===null?'missing_item':'compared',candidate_count:count,candidates:[]};
    legacy.link_candidates[id]=record;raw.link_candidates[id]=record;
    index.link_candidates[id]={status:record.status,candidate_count:count,candidate_ids:[]};
    cardRecord(files[`card-records/${id}.json`]).candidate_record=record;
    editorialRecord(files[`details/${id}.json`]).link_candidates[id]=record;
  }
  const reviews=new Map<string,EditorialReview>();
  for(const source of sources.slice(0,10))reviews.set(`vec:${source.page_id}:1`,{language:'vec',source_id:source.page_id,target_id:1,status:'fixed',version:1,updated_at:'2026-10-20T00:00:00Z'});
  reviews.set('vec:1000:2',{language:'vec',source_id:1000,target_id:2,status:'bad_link',version:1,updated_at:'2026-10-20T00:00:00Z'});
  let active=0,maximum=0,hold=false;const held: {url:string;signal:AbortSignal;resolve:()=>void}[]=[];
  const beforeFetch=async(url: string,options: FetchOptions)=>{
    if(!/^data\/(?:lmo-)?card-records\//.test(String(url)))return;
    active++;maximum=Math.max(maximum,active);
    try {
      if(hold)await new Promise<void>((resolve,reject)=>{held.push({url,signal:required(options.signal),resolve});required(options.signal).addEventListener('abort',()=>reject(new Error('aborted')),{once:true});});
      else await new Promise<void>(resolve=>setImmediate(resolve));
    } finally {active--;}
  };
  const old=await harness('deorphanize',raw,'?dataset=fixture.json',[legacyEntry],{reviewStore:reviews},files);
  const next=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{reviewStore:reviews,beforeFetch},files);
  const cards=(result: Awaited<ReturnType<typeof harness>>)=>result.nodes['candidate-cards'].children.filter(node=>node.className==='candidate-card');
  const snapshot=(result: Awaited<ReturnType<typeof harness>>)=>cards(result).map(card=>[titleLink(card).textContent,findClass(card,'candidate-badge').textContent,
    findClass(card,'connect-button').href,findClass(card,'fix-link').getAttribute('aria-disabled')??false]);
  assert.deepEqual(snapshot(next),snapshot(old));assert.equal(maximum,4);
  const missingTopics=structuredClone(files);
  const missingCard=cardRecord(missingTopics['card-records/2.json']);
  delete missingCard.page.topic_status;delete missingCard.page.topic_labels;
  const missingIndex=missingTopics['card-index.json'];assert(isRecord(missingIndex)&&Array.isArray(missingIndex.rows));
  const missingRow: unknown=missingIndex.rows[1];assert(Array.isArray(missingRow));
  missingRow[columns.indexOf('topic_status')]=null;missingRow[columns.indexOf('topic_labels')]=null;
  const unavailable=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{reviewStore:reviews},missingTopics);
  assert.equal(findClass(cards(unavailable)[0],'fix-link').getAttribute('aria-disabled'),undefined);
  assert(!unavailable.requests.some(request=>request.url.startsWith('https://')));
  const firstRequests=next.requests.filter(request=>request.url.startsWith('data/card-records/'));
  assert.equal(firstRequests.length,25);
  assert.deepEqual(firstRequests.map(request=>Number(required(request.url.match(/(\d+)\.json$/))[1])).sort((a,b)=>a-b),Array.from({length:25},(_,i)=>i+2));
  assert(!next.requests.some(request=>request.url.startsWith('https://')||request.url==='data/fixture.json'));
  const bytes=(document: unknown)=>Buffer.byteLength(JSON.stringify(document));
  const transferred=bytes(index)+firstRequests.reduce((sum,request)=>sum+bytes(files[request.url.slice(5)]),0);
  assert(transferred<=bytes(legacy)*.25);
  const promptOld=await preparePrompt(old,findClass(cards(old)[0],'fix-link'));
  const promptNext=await preparePrompt(next,findClass(cards(next)[0],'fix-link'));
  const stablePrompt=(prompt: string)=>prompt.replace(/"retrieved_at": "[^"]+"/g,'"retrieved_at": "<retrieval>"');
  assert.equal(stablePrompt(promptNext),stablePrompt(promptOld));
  for(const result of [old,next])result.nodes['llm-prompt-close'].listeners.click();
  for(const result of [old,next])evaluate(`{
    const report=calculateReport('interests',{edits:[{language:'vec',topic_labels:['Culture.Media.Music']}],
      candidates:data.pages.map(page=>({key:pageKey(page),language:pageLanguage(page),topic_labels:page.topic_status==='predicted'?page.topic_labels:null})),aggregate:false});
    profileScores=new Map(report.scores.map(row=>[row.key,{topic:row.topic,language:row.language}]));
    $('profile-sort-option').disabled=false;$('candidate-sort').value='personalized';candidateCards();
  }`,result.context);
  await evaluate('cardHydration',next.context);assert.deepEqual(snapshot(next),snapshot(old));
  assert(cards(next).every(card=>Number(titleLink(card).textContent.slice(-3))%3===2));
  const change=async(id: string,value: string)=>{
    for(const result of [old,next]){result.nodes[id].value=value;result.nodes[id].listeners.input();}
    await evaluate('cardHydration',next.context);assert.deepEqual(snapshot(next),snapshot(old));
  };
  for(const order of ['title-asc','category-asc','length-desc','length-asc','date-desc','date-asc','candidates-asc','candidates-desc'])await change('candidate-sort',order);
  await change('candidate-category','scri');await change('candidate-category','');
  for(const result of [old,next])result.nodes['candidate-more'].listeners.click();
  await evaluate('cardHydration',next.context);assert.deepEqual(snapshot(next),snapshot(old));assert.equal(cards(next).length,50);
  await change('candidate-search','Target 001');
  assert.equal(required(cards(next)[0].children[0].children.filter(node=>node.className==='candidate-hint').at(-1)).textContent,
    required(cards(old)[0].children[0].children.filter(node=>node.className==='candidate-hint').at(-1)).textContent);
  assert.equal(evaluate('Object.values(data.link_candidates).filter(record=>record.candidates).length',next.context),0);
  hold=true;next.nodes['candidate-search'].value='Target 1';next.nodes['candidate-search'].listeners.input();
  await new Promise<void>(resolve=>setImmediate(resolve));assert.equal(held.length,4);
  hold=false;await change('candidate-search','Target 2');
  assert(held.every(request=>request.signal.aborted));assert(maximum<=4);
  assert(cards(next).every(card=>titleLink(card).textContent.startsWith('Target 2')));
  assert.equal(evaluate('Object.values(data.link_candidates).filter(record=>record.candidates).length',next.context),25);
  for(const [search,badge] of [['Target 299','0'],['Target 300','—']]) {
    const before=next.requests.length;await change('candidate-search',search);
    assert.equal(cards(next).length,0);assert.equal(next.requests.length,before);
    for(const result of [old,next])changeCheckbox(result.nodes,'candidate-show-without-connections',true);
    await evaluate('cardHydration',next.context);assert.deepEqual(snapshot(next),snapshot(old));
    assert.equal(findClass(cards(next)[0],'candidate-badge').textContent,badge);
    assert.equal(next.requests.length,before);
    for(const result of [old,next])changeCheckbox(result.nodes,'candidate-show-without-connections',false);
  }
  const lmoMetadata={...raw.metadata,language:'lmo',creators:{...raw.metadata.creators,language:'lmo',languages_scanned:['lmo']}};
  const lmoIndex=structuredClone(index);lmoIndex.metadata=lmoMetadata;lmoIndex.card_identity='b'.repeat(64);
  const lmoLegacy: EditorialRecord & {metadata: FixtureMetadata}={...legacy,metadata:lmoMetadata,pages:structuredClone(legacy.pages),link_candidates:{}};
  for(const target of targets) {
    const record=structuredClone(cardRecord(files[`card-records/${target.page_id}.json`]));record.card_identity=lmoIndex.card_identity;
    for(const candidate of required(record.candidate_record.candidates))required(candidate.evidence)[0].language='vec';
    files[`lmo-card-records/${target.page_id}.json`]=record;lmoLegacy.link_candidates[target.page_id]=record.candidate_record;
  }
  files['lmo-card-index.json']=lmoIndex;files['lmo-legacy-cards.json']=lmoLegacy;
  const lmoEntry: ManifestEntry={...entry,language:'lmo',file:'lmo.json',date:lmoMetadata.finished_at,cards_file:'lmo-card-index.json',cards_dir:'lmo-card-records',connections_dir:'lmo-details'};
  const lmoLegacyEntry={...lmoEntry,cards_file:'lmo-legacy-cards.json'};delete lmoLegacyEntry.cards_dir;
  const aggregate={metadata:{language:'all',kind:'aggregate',languages:['vec','lmo'],demo:false,complete:true,
    started_at:raw.metadata.started_at,finished_at:raw.metadata.finished_at},members:[
      {language:'vec',file:'fixture.json',page_count:raw.pages.length,metadata:raw.metadata},
      {language:'lmo',file:'lmo.json',page_count:raw.pages.length,metadata:lmoMetadata}]};
  files['all.json']=aggregate;
  const allEntry={language:'all',file:'all.json',date:raw.metadata.finished_at,kind:'aggregate'};
  const oldAll=await harness('deorphanize',aggregate,'?dataset=all.json',[allEntry,legacyEntry,lmoLegacyEntry],{reviewStore:reviews,chooseLanguage:'all'},files);
  const newAll=await harness('deorphanize',aggregate,'?dataset=all.json',[allEntry,entry,lmoEntry],{reviewStore:reviews,beforeFetch,chooseLanguage:'all'},files);
  assert.deepEqual(snapshot(newAll),snapshot(oldAll));
  assert.equal(newAll.requests.filter(request=>/^data\/(?:lmo-)?card-records\//.test(request.url)).length,25);assert(maximum<=4);
  assert(!newAll.requests.some(request=>request.url==='data/fixture.json'||request.url==='data/lmo.json'));
  hold=true;next.nodes['candidate-search'].value='Target 1';next.nodes['candidate-search'].listeners.input();
  await new Promise<void>(resolve=>setImmediate(resolve));const switched=held.slice(-4);assert(switched.every(request=>!request.signal.aborted));
  hold=false;next.context.switchEntry=lmoEntry;await evaluate('load(switchEntry)',next.context);
  assert(switched.every(request=>request.signal.aborted));assert(maximum<=4);
  assert.equal(evaluate('data.metadata.language',next.context),'lmo');
  assert(cards(next).every(card=>findClass(card,'connect-button').href.includes('dataset=lmo.json')));
  for(const mutate of [(card: OrphanCardRecord)=>card.card_identity='b'.repeat(64),(card: OrphanCardRecord)=>card.page.title='wrong',
    (card: OrphanCardRecord)=>{delete card.page.topic_status;delete card.page.topic_labels;},
    (card: OrphanCardRecord)=>required(card.candidate_record.candidates).pop(),(card: OrphanCardRecord)=>required(required(card.candidate_record.candidates)[0].evidence)[0].language='vec']) {
    const bad=structuredClone(cardRecord(files['card-records/2.json']));mutate(bad);
    const result=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{reviewStore:reviews}, {...files,'card-records/2.json':bad});
    assert.equal(findClass(cards(result)[0],'fix-link').getAttribute('aria-disabled'),'true');
    assert.equal(result.copied.length,0);
  }
  for(const mutate of [(card: CardIndexFixture)=>required(card.link_candidates[2].candidate_ids).push(1000),(card: CardIndexFixture)=>card.rows[0][2]=false,
    (card: CardIndexFixture)=>card.link_candidates[2].candidate_count=0,(card: CardIndexFixture)=>card.link_candidates[999]={candidate_count:null,candidate_ids:[]}]) {
    const bad=structuredClone(index);mutate(bad);
    const result=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{}, {...files,'card-index.json':bad});
    assert.match(result.nodes.status.textContent,/Impossibile caricare/);
    assert(!result.requests.some(request=>request.url.startsWith('data/card-records/')));
  }
  const unsafe=await harness('deorphanize',raw,'?dataset=fixture.json',[{...entry,cards_dir:'../outside'}],{},files);
  assert.match(unsafe.nodes.status.textContent,/Nome dataset non valido/);assert(!unsafe.requests.some(request=>request.url.includes('outside')));
  const empty=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{}, {...files,'card-index.json':{...index,rows:[],link_candidates:{}}});
  assert.equal(empty.document.body.dataset.loading,'false');assert.equal(cards(empty).length,0);
  assert(!empty.requests.some(request=>request.url.startsWith('data/card-records/')));
  console.log('Editorial card index: '+JSON.stringify({orphans:targets.length,visible:25,legacy_bytes:bytes(legacy),index_and_visible_bytes:transferred,
    maximum_concurrent:maximum,retained_rich_records:25,global_order_and_prompt_parity:true}));
}
async function checkConnectionCopy() {
  const raw=fixture();raw.metadata.demo=false;
  const target=raw.pages.find(page=>page.orphan),key=String(required(target).page_id);
  raw.link_candidates[key]={status:'compared',candidate_count:7,candidates:Array.from({length:7},(_,i)=>({page_id:100+i,title:`Local ${i}`,dead_end:i===6,evidence:[{language:'lmo',source_title:`Reference ${i}`,target_title:required(target).title}]}))};
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
  const before=JSON.stringify(raw),reviewStore=new Map<string,EditorialReview>();
  let overview=await harness('deorphanize',raw,undefined,null,{reviewStore});
  assert.equal(overview.requests.filter(request=>request.url.startsWith('api/reviews')).length,1);
  assert.equal(overview.requests.filter(request=>request.method==='POST').length,0);
  assert.equal(overview.nodes['llm-prompt-finished'].disabled,true);
  const card=()=>overview.nodes['candidate-cards'].children.find(card=>titleLink(card).textContent==='Target one');
  await preparePrompt(overview,findClass(card(),'fix-link'));
  assert.equal(overview.nodes['llm-prompt-finished'].disabled,false);
  overview.nodes['llm-prompt-debug'].checked=true;
  await overview.nodes['llm-prompt-finished'].listeners.click();
  await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(overview.nodes['llm-prompt'].open,false);
  assert.equal(overview.nodes['llm-prompt-text'].value,'');assert.equal(overview.nodes['llm-prompt-debug'].checked,false);
  assert.equal(overview.nodes['review-dialog'].open,true);
  assert.equal(overview.nodes['review-rows'].children.length,3);
  assert.equal(overview.requests.filter(request=>request.url.startsWith('api/reviews')&&request.method==='GET').length,1);
  assert(!overview.html.includes('review-refresh'));
  const choose=async(result: Awaited<ReturnType<typeof harness>>,index: number,statusIndex: number)=>{
    const row=result.nodes['review-rows'].children[index],button=findClass(row,'review-actions').children[statusIndex];
    assert.equal(button.disabled,false);button.listeners.click();
    await new Promise<void>(resolve=>setImmediate(resolve));
  };
  await choose(overview,0,0);await choose(overview,1,2);await choose(overview,2,1);
  assert.equal(reviewStore.size,3);
  assert.deepEqual([...reviewStore.values()].map(review=>review.status),['fixed','bad_link','not_fixed']);
  assert.match(overview.nodes['review-dialog-status'].textContent,/salvata.*tutti/);
  const posts=overview.requests.filter(request=>request.method==='POST');
  assert.equal(posts.length,3);
  for(const request of posts) {
    assert.equal(request.url,'api/reviews');assert.equal(required(request.headers)['Content-Type'],'application/json');
    const body=reviewBody(request.body);
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
  connection.nodes['llm-prompt-finished'].listeners.click();await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(connection.nodes['review-rows'].children.length,1);
  await choose(connection,0,0);
  assert.equal(evaluate("editorialReviews.get('vec:4:1').status",overview.context),'not_fixed');
  // Reloading the page reads the latest shared statuses before rendering its cards.
  overview=await harness('deorphanize',raw,undefined,null,{reviewStore});
  const reads=overview.requests.filter(request=>request.url.startsWith('api/reviews'));
  assert.equal(reads.length,1);assert.equal(reads[0].method,'GET');assert.equal(reads[0].cache,'no-store');
  assert.equal(titleLink(overview.nodes['candidate-cards'].children.at(-1)).textContent,'Target one');
  assert.equal(findClass(card(),'fix-link').getAttribute('aria-disabled'),'true');
  assert.equal(connection.nodes['connection-candidates'].children.length,3);
  assert.equal(connection.requests.filter(request=>request.url.startsWith('api/reviews')&&request.method==='GET').length,1);
  assert(!connection.html.includes('review-refresh'));

  // Closed pairs can be reopened from Connect without preparing another prompt.
  connection.nodes['review-close'].listeners.click();
  const source2=cards().find(card=>titleLink(card).textContent==='Source 2');
  findClass(source2,'review-edge').listeners.click();await new Promise<void>(resolve=>setImmediate(resolve));
  await choose(connection,0,1);
  assert.equal(titleLink(cards()[0]).textContent,'Source 2');
  assert.equal(findClass(cards()[0],'fix-link').tag,'button');
  assert.equal(required(reviewStore.get('vec:2:1')).status,'not_fixed');

  // Optimistic version checks surface a concurrent review without overwriting it.
  reviewStore.set('vec:2:1',{...required(reviewStore.get('vec:2:1')),status:'bad_link',version:3});
  await choose(connection,0,0);
  assert.equal(required(reviewStore.get('vec:2:1')).status,'bad_link');
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
  await new Promise<void>(resolve=>setImmediate(resolve));
  assert.match(offline.nodes['review-dialog-status'].textContent,/non disponibili/);
  assert(findClass(offline.nodes['review-rows'].children[0],'review-actions').children.every(button=>button.disabled));
  assert.equal(offline.requests.filter(request=>request.method==='POST').length,0);
  assert.match(offline.nodes['review-service-status'].textContent,/Ricarica la pagina/);
  assert.equal(offline.requests.filter(request=>request.url.startsWith('api/reviews')).length,1);
  const reloaded=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{reviewStore});
  assert.equal(reloaded.requests.filter(request=>request.url.startsWith('api/reviews')).length,1);
  assert.match(findClass(reloaded.nodes['connection-candidates'].children[0],'review-badge').textContent,/Bad link/);
  const failed=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{
    reviewAPI:async(_url,options)=>options?.method==='POST'?{ok:false,status:503,json:async()=>({error:'unavailable'})}:
      {ok:true,status:200,json:async()=>({format:'editorial_reviews_v1',reviews:[]})}
  });
  findClass(failed.nodes['connection-candidates'].children[0],'review-edge').listeners.click();
  await new Promise<void>(resolve=>setImmediate(resolve));await choose(failed,0,0);
  assert.match(failed.nodes['review-dialog-status'].textContent,/non confermato/);
  assert.equal(evaluate('editorialReviews.size',failed.context),0);
  assert.equal(findClass(failed.nodes['connection-candidates'].children[0],'fix-link').tag,'button');

  let releaseSave: (()=>void) | undefined;
  const stale=await harness('connections',raw,'?dataset=fixture.json&page=1',null,{
    reviewAPI:async(_url,options)=>options?.method==='POST'?new Promise<MockResponse>(resolve=>{
      const body=reviewBody(options.body);
      releaseSave=()=>resolve({ok:true,status:200,json:async()=>({review:{language:body.language,source_id:body.source_id,
        target_id:body.target_id,status:body.status,version:1,updated_at:'2026-10-20T12:00:00Z'}})});
    }):{ok:true,status:200,json:async()=>({format:'editorial_reviews_v1',reviews:[]})}
  });
  findClass(stale.nodes['connection-candidates'].children[0],'review-edge').listeners.click();
  await new Promise<void>(resolve=>setImmediate(resolve));
  findClass(stale.nodes['review-rows'].children[0],'review-actions').children[0].listeners.click();
  const saving=stale.requests.find(request=>request.method==='POST');
  stale.nodes['review-close'].listeners.click();required(releaseSave)();await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(required(required(saving).signal).aborted,true);
  assert.equal(stale.nodes['review-dialog'].open,false);
  assert.equal(evaluate('editorialReviews.size',stale.context),0);
  assert.equal(stale.nodes['review-rows'].children.length,0);

  const wide=structuredClone(raw);
  wide.link_candidates[1]={candidate_count:7,status:'compared',candidates:Array.from({length:7},(_,i)=>({
    page_id:20+i,title:'Wide source '+i,evidence:candidates[0].evidence}))};
  const capped=await harness('deorphanize',wide);
  await preparePrompt(capped,findClass(capped.nodes['candidate-cards'].children[0],'fix-link'));
  capped.nodes['llm-prompt-finished'].listeners.click();await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(capped.nodes['review-rows'].children.length,5);
  assert(!capped.nodes['review-rows'].children.some(row=>row.children[0].children[0].textContent==='Wide source 5'));
  console.log('Shared editorial reviews: Finished pair scope, no text persistence, independent readers, closed-pair ordering/filtering, full Connect evidence, reopening, conflicts, translations, failed/stale saves and unavailable service passed.');
}
async function checkWikipediaAuthentication() {
  const raw=fixture();raw.metadata.demo=false;
  raw.metadata.topics={status:'predicted'};
  const target=required(raw.pages.find(page=>page.orphan));
  const candidate=required(raw.pages.find(page=>!page.orphan));
  raw.link_candidates={...raw.link_candidates,[target.page_id]:{candidate_count:1,status:'compared',candidates:[{
    page_id:candidate.page_id,title:candidate.title,evidence:[{language:'lmo',source_title:'Reference source',target_title:'Reference target'}]}]}};
  for(const page of raw.pages){page.topic_status='predicted';page.topic_labels=['Culture.Media.Music'];}
  const entries=[{language:'vec',file:'fixture.json',date:raw.metadata.finished_at,observations_file:'observations.json'}];
  const files={'fixture.json':raw,'observations.json':{format:'observations_v1',metadata:raw.metadata,
    columns:['page_id','topic_labels','topic_status'],rows:[[target.page_id,['Culture.Media.Music'],'predicted']]}};
  const username='Editor À & Test';
  const apiResponse=(url: URL)=>url.hostname==='meta.wikimedia.org'
    ?{query:{globaluserinfo:{id:1,name:username,merged:[{wiki:'vecwiki',url:'https://vec.wikipedia.org',id:11}]}}}
    :{query:{usercontribs:[{userid:11,pageid:target.page_id,ns:0,timestamp:'2026-10-01T00:00:00Z'}]}};
  const preferences={authSession:{enabled:true,username},apiResponse};
  const result=await harness('deorphanize',raw,'?dataset=fixture.json',entries,preferences,files);
  assert.equal(result.nodes['auth-user'].textContent,username);
  assert.equal(result.nodes['auth-user'].href,'https://vec.wikipedia.org/wiki/User:Editor_%C3%80_%26_Test');
  assert.equal(result.nodes['auth-user'].hidden,false);assert.equal(result.nodes['auth-login'].hidden,true);
  assert.equal(result.nodes['profile-username'].value,username);assert.equal(result.nodes['profile-username'].disabled,true);
  assert.equal(result.nodes['candidate-sort'].value,'personalized');
  const globalQueries=()=>result.requests.filter(request=>request.url.startsWith('https://meta.wikimedia.org'));
  assert.equal(new URL(globalQueries()[0].url).searchParams.get('guiuser'),username);
  // The authenticated identity wins even if a script changes the disabled input.
  result.nodes['profile-username'].value='Impersonated';await evaluateAsync('applyProfile()',result.context);
  assert.equal(new URL(required(globalQueries().at(-1)).url).searchParams.get('guiuser'),username);
  assert.equal(result.authRequests[0].credentials,'same-origin');
  assert(![...result.storage.values()].some(value=>value.includes(username)));
  await evaluateAsync('logout()',result.context);
  assert.equal(result.nodes['auth-user'].hidden,true);assert.equal(result.nodes['auth-login'].hidden,false);
  assert.equal(result.nodes['profile-username'].disabled,false);assert.equal(result.nodes['profile-username'].value,'');
  assert.equal(evaluate('profileContributions',result.context),null);assert.equal(evaluate('profileScores',result.context),null);
  assert.equal(result.nodes['profile-sort-option'].disabled,true);
  assert.equal(required(result.authRequests.at(-1)).method,'POST');

  const store=new Map<string,EditorialReview>();
  const connection=await harness('connections',raw,'?dataset=fixture.json&page='+target.page_id,null,{...preferences,reviewStore:store});
  const source=required(connection.nodes['connection-candidates'].children[0]);
  findClass(source,'review-edge').listeners.click();await new Promise<void>(resolve=>setImmediate(resolve));
  const button=findClass(connection.nodes['review-rows'].children[0],'review-actions').children[0];
  assert.equal(button.disabled,false);button.listeners.click();await new Promise<void>(resolve=>setImmediate(resolve));
  const review=required([...store.values()][0]);assert.equal(review.reviewer,username);
  const author=descendants(connection.nodes['review-rows']).find(node=>node.tag==='a'&&node.textContent===username);
  assert.equal(required(author).href,'https://vec.wikipedia.org/wiki/User:Editor_%C3%80_%26_Test');
  const post=required(connection.requests.find(request=>request.method==='POST'));
  assert.equal(post.credentials,'same-origin');assert(!required(post.body).includes(username));
  const signedOut=await harness('connections',raw,'?dataset=fixture.json&page='+target.page_id,null,
    {authSession:{enabled:true,username:null},reviewStore:store});
  assert.equal(signedOut.requests.filter(request=>request.url.startsWith('https://')).length,0);
  findClass(signedOut.nodes['connection-candidates'].children[0],'review-edge').listeners.click();await new Promise<void>(resolve=>setImmediate(resolve));
  assert(findClass(signedOut.nodes['review-rows'].children[0],'review-actions').children.every(button=>button.disabled));
  assert.match(signedOut.nodes['review-dialog-status'].textContent,/Accedi con Wikipedia/);
  assert(descendants(signedOut.nodes['connection-candidates']).some(node=>node.tag==='a'&&node.textContent===username));
  for(const page of ['articles','creators'] as const) {
    const dashboard=await harness(page,raw,undefined,null,preferences);
    assert.equal(dashboard.nodes['auth-user'].textContent,username);
    assert.equal(dashboard.requests.filter(request=>request.url.startsWith('https://')).length,0);
  }
  const unavailable=await harness('deorphanize',raw,undefined,null,{authAPI:()=>({ok:false,status:503,json:async()=>({})})});
  assert.equal(unavailable.nodes['auth-login'].hidden,false);
  assert.match(unavailable.nodes['auth-status'].textContent,/non disponibile/);
  assert(unavailable.nodes['candidate-cards'].children.length>0);
  for(const page of ['articles','creators','deorphanize','connections']) {
    const file=page==='articles'?'index.html':page+'.html',html=fs.readFileSync('web/'+file,'utf8');
    const header=required(html.match(/<header>([\s\S]*?)<\/header>/)?.[1]);
    assert.match(header,/id="ui-language-toggle"[\s\S]*id="theme-toggle"[\s\S]*<details id="auth-menu"[\s\S]*<summary id="auth-toggle"[\s\S]*<svg[\s\S]*id="auth-login"/);
    assert.equal((html.match(/id="auth-menu"/g)??[]).length,1);
    assert.match(header,/id="auth-status" role="alert"/);
    assert(!html.includes('account-toolbar'));
  }
  assert.equal(signedOut.nodes['auth-menu'].open,false);
  signedOut.nodes['auth-menu'].open=true;
  signedOut.nodes['auth-login'].listeners.click();await new Promise<void>(resolve=>setImmediate(resolve));
  assert.deepEqual(signedOut.navigations,['api/auth/login?return_to='+encodeURIComponent('connections.html?dataset=fixture.json&page='+target.page_id)]);
  signedOut.nodes['auth-menu'].listeners.keydown({key:'Escape',preventDefault(){}});
  assert.equal(signedOut.nodes['auth-menu'].open,false);assert.equal(signedOut.nodes['auth-toggle'].focused,true);
  assert.equal(signedOut.nodes['auth-status'].hidden,true);

  // Missing setup, server errors, offline requests and malformed responses stay in the menu.
  for(const authAPI of [
    ()=>({ok:true,json:async()=>({enabled:false,username:null})}),
    ()=>({ok:false,status:503,json:async()=>({})}),
    ()=>{throw new Error('offline');},
    ()=>({ok:true,json:async()=>({enabled:'bad',username:null})})
  ]) {
    const failed=await harness('deorphanize',raw,undefined,null,{authAPI});
    failed.nodes['auth-menu'].open=true;
    await evaluateAsync('login()',failed.context);
    assert.equal(failed.nodes['auth-menu'].open,true);
    assert.equal(failed.nodes['auth-login'].disabled,false);assert.equal(failed.nodes['auth-login'].hidden,false);
    assert.equal(failed.nodes['auth-status'].hidden,false);assert.match(failed.nodes['auth-status'].textContent,/non disponibile.*Riprova/);
    assert.deepEqual(failed.navigations,[]);
    failed.nodes['ui-language-toggle'].listeners.click();
    assert.match(failed.nodes['auth-status'].textContent,/unavailable.*try again/);
  }
  let available=false;
  const retry=await harness('deorphanize',raw,undefined,null,{authAPI:()=>({ok:available,status:available?200:503,json:async()=>({enabled:true,username:null})})});
  await evaluateAsync('login()',retry.context);assert.equal(retry.navigations.length,0);
  available=true;await evaluateAsync('login()',retry.context);
  assert.equal(retry.navigations.length,1);assert.equal(retry.nodes['auth-status'].hidden,true);
  const logoutFailure=await harness('articles',raw,undefined,null,{authAPI:url=>url.endsWith('/logout')
    ?{ok:false,status:503,json:async()=>({})}:{ok:true,json:async()=>({enabled:true,username})}});
  await evaluateAsync('logout()',logoutFailure.context);
  assert.equal(logoutFailure.nodes['auth-user'].textContent,username);
  assert.match(logoutFailure.nodes['auth-status'].textContent,/non disponibile/);
  console.log('Wikipedia account menu: header placement, keyboard dismissal, login navigation, unavailable service, retry and logout errors passed.');
  console.log('Wikipedia authentication: automatic recommendations, linked identities, server attribution, logout, anonymous readers and unavailable OAuth passed.');
}
async function checkPersonalization() {
  const raw=fixture();raw.metadata.demo=false;
  raw.metadata.topics={status:'predicted'};
  const target=raw.pages.find(page=>page.orphan&&(raw.link_candidates[page.page_id]?.candidate_count??0)>0);
  for(const page of raw.pages){page.topic_status='predicted';page.topic_labels=page.page_id===required(target).page_id?['Culture.Media.Music']:[];}
  const entries=[{language:'vec',file:'fixture.json',date:raw.metadata.finished_at,demo:false,observations_file:'fixture-observations.json'}];
  const files={'fixture.json':raw,'fixture-observations.json':{
    format:'observations_v1',metadata:raw.metadata,columns:['page_id','topic_labels','topic_status'],
    rows:[[required(target).page_id,['Culture.Media.Music'],'predicted']]}};
  const apiResponse=(url: URL)=>{
    if(url.hostname==='meta.wikimedia.org')return {query:{globaluserinfo:{id:1,name:'Editor',merged:[
      {wiki:'vecwiki',url:'https://vec.wikipedia.org',id:11,editcount:1},
      {wiki:'enwiki',url:'https://en.wikipedia.org',id:22,editcount:1},
      ...Array.from({length:133},(_,i)=>({wiki:'a'+i+'wiki',url:'https://a'+i+'.wikipedia.org',id:i+100,editcount:0}))]}}};
    if(url.hostname==='vec.wikipedia.org')return {query:{usercontribs:[{userid:11,pageid:required(target).page_id,ns:0,timestamp:'2026-10-01T00:00:00Z'}]}};
    if(url.hostname==='en.wikipedia.org')return {query:{usercontribs:[{userid:22,pageid:999,ns:0,timestamp:'2026-09-30T00:00:00Z'}]}};
    throw new Error('Unapproved API');
  };
  const result=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{apiResponse},files);
  assert.equal(result.requests.filter(request=>request.url.startsWith('https://')).length,0);
  result.nodes['profile-username'].value='Editor';
  result.nodes['profile-apply'].listeners.click();
  await new Promise<void>(resolve=>setImmediate(resolve));await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(result.nodes['candidate-sort'].value,'personalized');
  assert.equal(result.nodes['profile-sort-option'].disabled,false);
  assert.equal(titleLink(result.nodes['candidate-cards'].children[0]).textContent,required(target).title);
  assert.match(result.nodes['profile-status'].textContent,/1 modifiche nelle Wikipedie analizzate.*1/);
  const api=result.requests.filter(request=>request.url.startsWith('https://')).map(request=>new URL(request.url));
  assert.equal(api.length,2);
  assert.deepEqual(api.map(url=>url.hostname),['meta.wikimedia.org','vec.wikipedia.org']);
  assert(api.every(url=>url.searchParams.get('origin')==='*'&&['globaluserinfo','usercontribs'].includes(url.searchParams.get('meta')||url.searchParams.get('list')||'')));
  assert(api.filter(url=>url.searchParams.get('list')==='usercontribs').every(url=>url.searchParams.get('uclimit')==='100'&&url.searchParams.has('ucuserids')));
  const uncovered=await harness('deorphanize',raw,'?dataset=fixture.json',entries.map(({observations_file,...entry})=>entry),{apiResponse},{'fixture.json':raw});
  evaluate(`globalThis.interestCalls=0;const originalCalculate=nativeCalculate;
    nativeCalculate=(kind,input)=>{if(kind==='interests')interestCalls++;return originalCalculate(kind,input);}`,uncovered.context);
  uncovered.nodes['profile-username'].value='Editor';uncovered.nodes['profile-apply'].listeners.click();
  await new Promise<void>(resolve=>setImmediate(resolve));await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(uncovered.nodes['profile-sort-option'].disabled,true);
  assert.match(uncovered.nodes['profile-status'].textContent,/Nessuna.*temi utilizzabili/);
  assert.equal(evaluate('interestCalls',uncovered.context),0);
  const outside=await harness('deorphanize',raw,'?dataset=fixture.json',[
    ...entries,{language:'en',file:'demo-en.json',date:raw.metadata.finished_at,demo:true},
    {language:'fr',kind:'aggregate',file:'aggregate-fr.json',date:raw.metadata.finished_at}],{apiResponse:url=>{
    if(url.hostname==='meta.wikimedia.org')return {query:{globaluserinfo:{id:1,name:'Editor',merged:[
      {wiki:'enwiki',url:'https://en.wikipedia.org',id:22,editcount:1},
      {wiki:'frwiki',url:'https://fr.wikipedia.org',id:33,editcount:1},
      {wiki:'dewiki',url:'https://de.wikipedia.org',id:44,editcount:1}]}}};
    return {query:{usercontribs:[{userid:22,pageid:999,ns:0,timestamp:'2026-10-01T00:00:00Z'}]}};
  }},files);
  outside.nodes['profile-username'].value='Editor';outside.nodes['profile-apply'].listeners.click();
  await new Promise<void>(resolve=>setImmediate(resolve));await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(outside.nodes['profile-sort-option'].disabled,true);
  assert.match(outside.nodes['profile-status'].textContent,/Nessuna delle Wikipedie collegate all’account è presente nelle raccolte disponibili/);
  assert.equal(outside.requests.filter(request=>request.url.startsWith('https://')).length,1);
  outside.nodes['ui-language-toggle'].listeners.click();
  await evaluateAsync('applyProfile()',outside.context);
  assert.match(outside.nodes['profile-status'].textContent,/None of the Wikipedias attached to the account is present in the available collections/);
  assert.equal(outside.requests.filter(request=>request.url.startsWith('https://')).length,2);
  const missing=structuredClone(raw);delete missing.metadata.topics;
  const noModel=await harness('deorphanize',missing,'?dataset=fixture.json',entries,{apiResponse},
    {'fixture.json':missing,'fixture-observations.json':{...files['fixture-observations.json'],metadata:missing.metadata}});
  noModel.nodes['profile-username'].value='Editor';noModel.nodes['profile-apply'].listeners.click();
  await new Promise<void>(resolve=>setImmediate(resolve));await new Promise<void>(resolve=>setImmediate(resolve));
  assert.match(noModel.nodes['profile-status'].textContent,/Nessuna.*temi utilizzabili/);
  assert.equal(noModel.nodes['profile-sort-option'].disabled,true);

  // English edits inform the ranking of Venetian cards without loading English
  // cards or an aggregate. Read only the newest real census for each language.
  const crossRaw=structuredClone(raw);
  const crossTarget=crossRaw.pages.filter(page=>page.orphan&&(crossRaw.link_candidates[page.page_id]?.candidate_count??0)>0).at(-1);
  assert.notEqual(required(crossTarget).page_id,required(target).page_id);
  for(const page of crossRaw.pages)page.topic_labels=page.page_id===required(crossTarget).page_id?['Culture.Media.Music']:[];
  const englishMetadata={...raw.metadata,language:'en',finished_at:'2026-10-02T00:00:00Z'};
  const englishObservations={format:'observations_v1',metadata:englishMetadata,
    columns:['page_id','topic_labels','topic_status'],rows:[[999,['Culture.Media.Music'],'predicted']]};
  const crossEntries: ManifestEntry[]=[...entries,
    {language:'en',file:'en-old.json',date:'2026-09-01T00:00:00Z',observations_file:'en-old-observations.json'},
    {language:'en',file:'en-new.json',date:englishMetadata.finished_at,observations_file:'en-observations.json'},
    {language:'en',file:'en-demo.json',date:'2027-01-01T00:00:00Z',demo:true,observations_file:'en-demo-observations.json'},
    {language:'all',kind:'aggregate',file:'all.json',date:'2027-01-01T00:00:00Z',observations_file:'all-observations.json'}];
  const crossFiles={...files,'fixture.json':crossRaw,'en-observations.json':englishObservations};
  const apply=async (result: Awaited<ReturnType<typeof harness>>)=>{
    result.nodes['profile-username'].value='Editor';result.nodes['profile-apply'].listeners.click();
    await new Promise<void>(resolve=>setImmediate(resolve));await new Promise<void>(resolve=>setImmediate(resolve));
  };
  const cross=await harness('deorphanize',crossRaw,'?dataset=fixture.json',crossEntries,{apiResponse},crossFiles);
  assert.equal(cross.requests.filter(request=>request.url.endsWith('-observations.json')).length,0);
  await apply(cross);
  assert.equal(cross.nodes['candidate-sort'].value,'personalized');
  assert.equal(titleLink(cross.nodes['candidate-cards'].children[0]).textContent,required(crossTarget).title);
  assert.equal(cross.nodes['candidate-cards'].children.length,25);
  assert.match(cross.nodes['candidate-info'].textContent,new RegExp(`^${crossRaw.pages.filter(page=>page.orphan&&(crossRaw.link_candidates[page.page_id]?.candidate_count??0)>0).length} orfane`));
  assert.match(cross.nodes['profile-status'].textContent,/2 modifiche.*2.*EN, VEC/);
  assert(cross.requests.some(request=>request.url==='data/en-observations.json'));
  assert(!cross.requests.some(request=>/en-old|en-demo|all\.json|all-observations|en-new\.json/.test(request.url)));
  assert(cross.nodes['candidate-cards'].children.every(card=>titleLink(card).href.startsWith('https://vec.wikipedia.org/')));

  // Foreign topics alone suffice when local observations are unavailable.
  const foreignOnly=await harness('deorphanize',crossRaw,'?dataset=fixture.json',crossEntries.filter(entry=>entry.language!=='vec')
    .concat(entries.map(({observations_file,...entry})=>entry)),{apiResponse},crossFiles);
  await apply(foreignOnly);
  assert.equal(foreignOnly.nodes['candidate-sort'].value,'personalized');
  assert.equal(titleLink(foreignOnly.nodes['candidate-cards'].children[0]).textContent,required(crossTarget).title);
  assert.match(foreignOnly.nodes['profile-status'].textContent,/2 modifiche.*1.*EN\./);

  for(const patch of [{language:'lmo'},{finished_at:'2026-09-01T00:00:00Z'},{complete:false},{demo:true}]) {
    const invalid=await harness('deorphanize',crossRaw,'?dataset=fixture.json',crossEntries,{apiResponse},
      {...crossFiles,'en-observations.json':{...englishObservations,metadata:{...englishMetadata,...patch}}});
    await apply(invalid);
    assert.equal(invalid.nodes['profile-sort-option'].disabled,true);
    assert.match(invalid.nodes['profile-status'].textContent,/Personalizzazione non disponibile/);
  }
  console.log('Personalization: 135 attached wikis restricted to real platform editions, demo/aggregate/uncollected exclusions, cross-language topics, coverage, translations and topic-first ranking passed.');
}
async function checkPersonalizationRecovery() {
  const raw=fixture();raw.metadata.demo=false;raw.metadata.topics={status:'predicted'};
  const target=required(raw.pages.find(page=>page.orphan));
  for(const page of raw.pages){page.topic_status='predicted';page.topic_labels=['Culture.Media.Music'];}
  const entries=[{language:'vec',file:'fixture.json',date:raw.metadata.finished_at,observations_file:'topics.json'}];
  const files={'fixture.json':raw,'topics.json':{format:'observations_v1',metadata:raw.metadata,
    columns:['page_id','topic_labels','topic_status'],rows:[[target.page_id,['Culture.Media.Music'],'predicted']]}};
  const ok=(payload: unknown): MockResponse=>({ok:true,status:200,headers:{get:()=>null},json:async()=>payload});
  const global=(languages: string[],id=1)=>({query:{globaluserinfo:{id,name:'Editor',merged:languages.map((language,i)=>({
    wiki:language+'wiki',url:'https://'+language+'.wikipedia.org',id:i+11,editcount:0}))}}});
  const edits=(id: number)=>({query:{usercontribs:[{userid:id,pageid:target.page_id,ns:0,timestamp:'2026-10-01T00:00:00Z'}]}});
  const flush=async()=>{await new Promise<void>(resolve=>setImmediate(resolve));};
  const clock=(context: vm.Context)=>{
    evaluate(`globalThis.profileNow=Date.now();Date.now=()=>profileNow;
      globalThis.profileTimers=new Map();globalThis.profileTimerId=0;
      setTimeout=(callback,delay)=>{const id=++profileTimerId;profileTimers.set(id,{callback,due:profileNow+delay});return id;};
      clearTimeout=id=>profileTimers.delete(id);`,context);
    return async(milliseconds: number)=>{
      evaluate(`profileNow+=${milliseconds};for(const [id,timer] of [...profileTimers])if(timer.due<=profileNow){profileTimers.delete(id);timer.callback();}`,context);
      await flush();
    };
  };
  const apiRequests=(result: Awaited<ReturnType<typeof harness>>)=>result.requests.filter(request=>request.url.startsWith('https://'));
  const contributionRequests=(result: Awaited<ReturnType<typeof harness>>)=>apiRequests(result).filter(request=>new URL(request.url).searchParams.get('list')==='usercontribs');

  // A failed wiki keeps successful snapshots without publishing an incomplete ranking.
  let available=false,globalId=1;
  const cacheEntries=[...entries,...['en','fr'].map(language=>({language,file:language+'.json',date:raw.metadata.finished_at}))];
  const cached=await harness('deorphanize',raw,'?dataset=fixture.json',cacheEntries,{apiHTTP:url=>{
    if(url.hostname==='meta.wikimedia.org')return ok(global(['vec','en','fr'],globalId));
    if(url.hostname==='fr.wikipedia.org'&&!available)return {ok:false,status:403,headers:{get:()=>null},json:async()=>({})};
    return ok(edits(Number(url.searchParams.get('ucuserids'))));
  }},files);
  const advanceCached=clock(cached.context);cached.nodes['profile-username'].value='Editor';
  await evaluateAsync('applyProfile()',cached.context);
  assert.match(cached.nodes['profile-status'].textContent,/2\/3.*fr.wikipedia.org, HTTP 403.*cinque minuti/);
  assert.equal(cached.nodes['profile-sort-option'].disabled,true);
  assert.equal(evaluate('recentProfileCache.edits.length',cached.context),2);
  assert.equal(cached.requests.filter(request=>request.url==='data/topics.json').length,0);
  const snapshot=new URL(required(contributionRequests(cached)[0]).url).searchParams.get('ucstart');
  const offset=cached.requests.length;available=true;
  await evaluateAsync('applyProfile()',cached.context);
  assert.equal(cached.nodes['candidate-sort'].value,'personalized');
  assert.match(cached.nodes['profile-status'].textContent,/3 modifiche.*1/);
  const resumed=cached.requests.slice(offset).filter(request=>request.url.startsWith('https://'));
  assert.equal(resumed.length,2);assert.equal(new URL(resumed[1].url).hostname,'fr.wikipedia.org');
  assert.equal(new URL(resumed[1].url).searchParams.get('ucstart'),snapshot);
  const warm=cached.requests.length;
  await evaluateAsync('applyProfile()',cached.context);
  assert.equal(cached.requests.length-warm,1); // Reverify identity, reuse edits and requested topics.
  await advanceCached(300_001);
  const expired=contributionRequests(cached).length;
  await evaluateAsync('applyProfile()',cached.context);
  assert.equal(contributionRequests(cached).length-expired,3);
  globalId=2;
  const changed=contributionRequests(cached).length;
  await evaluateAsync('applyProfile()',cached.context);
  assert.equal(contributionRequests(cached).length-changed,3);
  // Removing an edition changes the cache identity and excludes its previous edits.
  evaluate("datasetEntries=datasetEntries.filter(entry=>entry.language!=='fr')",cached.context);
  available=false;
  const rescoped=contributionRequests(cached).length;
  await evaluateAsync('applyProfile()',cached.context);
  assert.equal(contributionRequests(cached).length-rescoped,2);
  assert(contributionRequests(cached).slice(rescoped).every(request=>new URL(request.url).hostname!=='fr.wikipedia.org'));
  assert.equal(evaluate('recentProfileCache.edits.length',cached.context),2);
  assert.equal(evaluate('recentProfileCache.completed.size',cached.context),2);
  assert.equal(cached.nodes['candidate-sort'].value,'personalized');
  evaluate('clearProfile()',cached.context);
  assert.equal(evaluate('recentProfileCache',cached.context),null);

  // Retry-After seconds and HTTP dates are respected without real sleeps.
  let attempts=0;
  const retry=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{apiHTTP:url=>{
    if(url.hostname==='meta.wikimedia.org')return ok(global(['vec']));
    attempts++;
    if(attempts<=2)return {ok:false,status:attempts===1?429:503,
      headers:{get:()=>attempts===1?'7':new Date(Number(evaluate('Date.now()',retry.context))+9000).toUTCString()},json:async()=>({})};
    return ok(edits(11));
  }},files);
  const advanceRetry=clock(retry.context);retry.nodes['profile-username'].value='Editor';
  const retryRun=evaluateAsync('applyProfile()',retry.context);await flush();
  assert.equal(attempts,1);assert.match(retry.nodes['profile-status'].textContent,/0\/1.*7 s.*vec.wikipedia.org/);
  await advanceRetry(6999);assert.equal(attempts,1);
  await advanceRetry(1);assert.equal(attempts,2);
  // HTTP dates have second precision.
  await advanceRetry(9000);await retryRun;
  assert.equal(attempts,3);assert.equal(retry.nodes['candidate-sort'].value,'personalized');
  assert.equal(evaluate('profileTimers.size',retry.context),0);

  const cancelled=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{apiHTTP:url=>url.hostname==='meta.wikimedia.org'
    ?ok(global(['vec'])):{ok:false,status:429,headers:{get:()=> '5'},json:async()=>({})}},files);
  const advanceCancelled=clock(cancelled.context);cancelled.nodes['profile-username'].value='Editor';
  const cancelledRun=evaluateAsync('applyProfile()',cancelled.context);await flush();
  evaluate('clearProfile()',cancelled.context);await cancelledRun;
  const cancelledCount=apiRequests(cancelled).length;
  await advanceCancelled(60_000);
  assert.equal(apiRequests(cancelled).length,cancelledCount);
  assert.equal(evaluate('profileTimers.size',cancelled.context),0);
  assert.equal(cancelled.nodes['profile-status'].textContent,'');

  // Slow requests time out, retry a bounded number of times, and identify the host in English.
  const timeout=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{apiHTTP:(url,options)=>{
    if(url.hostname==='meta.wikimedia.org')return ok(global(['vec']));
    return new Promise<MockResponse>((_resolve,reject)=>required(options.signal).addEventListener('abort',()=>reject(required(options.signal).reason),{once:true}));
  }},files);
  timeout.nodes['ui-language-toggle'].listeners.click();
  const advanceTimeout=clock(timeout.context);timeout.nodes['profile-username'].value='Editor';
  const timeoutRun=evaluateAsync('applyProfile()',timeout.context);await flush();
  for(const duration of [15_000,5000,15_000,10_000,15_000])await advanceTimeout(duration);
  await timeoutRun;
  assert.equal(contributionRequests(timeout).length,3);
  assert.match(timeout.nodes['profile-status'].textContent,/Personalization unavailable.*vec.wikipedia.org, request timed out/);
  assert.equal(evaluate('profileTimers.size',timeout.context),0);

  // An API error inside HTTP 200 also retries, and a long Retry-After is reported without retrying early.
  let ratelimited=true;
  const apiError=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{apiHTTP:url=>url.hostname==='meta.wikimedia.org'
    ?ok(global(['vec'])):ratelimited?{...ok({error:{code:'ratelimited'}}),headers:{get:()=> '8'}}:ok(edits(11))},files);
  const advanceAPIError=clock(apiError.context);apiError.nodes['profile-username'].value='Editor';
  const apiErrorRun=evaluateAsync('applyProfile()',apiError.context);await flush();ratelimited=false;
  await advanceAPIError(7999);assert.equal(contributionRequests(apiError).length,1);
  await advanceAPIError(1);await apiErrorRun;
  assert.equal(apiError.nodes['candidate-sort'].value,'personalized');
  const longWait=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{apiHTTP:url=>url.hostname==='meta.wikimedia.org'
    ?ok(global(['vec'])):{ok:false,status:429,headers:{get:()=> '120'},json:async()=>({})}},files);
  clock(longWait.context);longWait.nodes['profile-username'].value='Editor';
  await evaluateAsync('applyProfile()',longWait.context);
  assert.equal(contributionRequests(longWait).length,1);
  assert.match(longWait.nodes['profile-status'].textContent,/HTTP 429.*riprova tra 120 s/);

  // Reject a whole malformed response before merging any of its otherwise valid rows.
  for(const bad of [null,{userid:99,pageid:target.page_id,ns:0,timestamp:'2026-10-01T00:00:00Z'}]) {
    const invalid=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{apiHTTP:url=>url.hostname==='meta.wikimedia.org'
      ?ok(global(['vec'])):ok({query:{usercontribs:[edits(11).query.usercontribs[0],bad]}})},files);
    invalid.nodes['profile-username'].value='Editor';await evaluateAsync('applyProfile()',invalid.context);
    assert.match(invalid.nodes['profile-status'].textContent,/Modifiche Wikimedia non verificabili: vec.wikipedia.org/);
    assert.equal(evaluate('recentProfileCache.edits.length',invalid.context),0);
    assert.equal(evaluate('recentProfileCache.completed.size',invalid.context),0);
    assert.equal(contributionRequests(invalid).length,1);
  }

  // The shared rolling budget includes the global identity call and never skips zero counters.
  const languages=Array.from({length:180},(_,i)=>'a'+i);
  const budgetEntries=[...entries,...languages.map(language=>({language,file:language+'.json',date:raw.metadata.finished_at}))];
  const budget=await harness('deorphanize',raw,'?dataset=fixture.json',budgetEntries,{apiHTTP:url=>url.hostname==='meta.wikimedia.org'
    ?ok(global(languages)):ok({query:{usercontribs:[]}})},files);
  const advanceBudget=clock(budget.context);
  let finished=false;
  const budgetRun=evaluateAsync("recentWikipediaEdits('Editor',profileController.signal)",budget.context).then(value=>{finished=true;return value;});
  await flush();assert.equal(apiRequests(budget).length,180);assert.equal(finished,false);
  await advanceBudget(59_999);assert.equal(apiRequests(budget).length,180);
  await advanceBudget(1);assert.deepEqual(structuredClone(await budgetRun),[]);
  assert.equal(apiRequests(budget).length,181);assert.equal(contributionRequests(budget).length,180);
  assert.equal(evaluate('profileAPIStarts.length',budget.context),1);
  assert.equal(evaluate('profileTimers.size',budget.context),0);
  console.log('Personalization resilience: bounded retries/timeouts, Retry-After/cooldowns, 180/min budget, cancellation, snapshot cache recovery/expiry/identity and translated errors passed.');
}
async function checkProfileTopicShards() {
  const raw=fixture();raw.metadata.demo=false;raw.metadata.topics={status:'predicted'};
  const entry={language:'vec',file:'fixture.json',date:raw.metadata.finished_at,
    observations_file:'must-not-load.json',profile_topics_dir:'topics'};
  const index={format:'profile_topics_index_v1',metadata:raw.metadata,shard_size:1024,shards:[0,1],census_page_count:4};
  const shard: TopicFile={format:'profile_topics_shard_v1',metadata:raw.metadata,bucket:0,
    rows:[[1,'predicted',['Culture.Media.Music']],[2,'predicted',[]],[3,'missing',null]]};
  const files: Record<string,TopicFile>={'fixture.json':raw,'topics/index.json':index,'topics/0.json':shard,
    'topics/1.json':{...shard,bucket:1,rows:[[1024,'predicted',['Culture.Media.Films']]]}};
  const result=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{},files);
  const edits: RecentEdit[]=[1,2,3,1024,99999,1].map(page_id=>({language:'vec',page_id,namespace:0,
    timestamp:'2026-10-01T00:00:00Z',topic_labels:null}));
  result.context.edits=edits;
  const enriched=await evaluateAsync('observedTopics(edits,profileRequestVersion)',result.context);
  assert(isList(enriched));
  assert.deepEqual(structuredClone(enriched.map(edit=>{assert(isRecord(edit));return edit.topic_labels;})),
    [['Culture.Media.Music'],[],null,['Culture.Media.Films'],null,['Culture.Media.Music']]);
  assert.equal(result.requests.filter(request=>request.url.startsWith('data/topics/')).length,3);
  const before=result.requests.length;
  await evaluate('observedTopics(edits,profileRequestVersion)',result.context);
  assert.equal(result.requests.length,before);
  for(const [path,patch] of [
    ['topics/index.json',{metadata:{...raw.metadata,language:'en'}}],
    ['topics/index.json',{shard_size:0}],['topics/index.json',{shards:[1,0]}],
    ['topics/0.json',{bucket:1}],['topics/0.json',{metadata:{...raw.metadata,finished_at:'old'}}],
    ['topics/0.json',{rows:[[1024,'predicted',[]]]}],
    ['topics/0.json',{rows:[[1,'predicted',[]],[1,'predicted',[]]]}],
    ['topics/0.json',{rows:Array.from({length:1025},(_,id)=>[id+1,'predicted',[]])}]
  ] as const) {
    const invalid=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{},
      {...files,[path]:{...files[path],...patch}});
    invalid.context.edits=[edits[0]];
    await assert.rejects(evaluateAsync('observedTopics(edits,profileRequestVersion)',invalid.context),/Trasporto osservazioni non valido/);
  }
  const unsafe=await harness('deorphanize',raw,'?dataset=fixture.json',[{...entry,profile_topics_dir:'../topics'}],{},files);
  unsafe.context.edits=[edits[0]];
  await assert.rejects(evaluateAsync('observedTopics(edits,profileRequestVersion)',unsafe.context),/Trasporto osservazioni non valido/);

  let release: (()=>void) | undefined;
  const stale=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{
    beforeFetch:url=>url==='data/topics/index.json'?new Promise<void>(resolve=>{release=resolve;}):undefined
  },files);
  stale.context.edits=[edits[0]];
  const loading=evaluateAsync('observedTopics(edits,profileRequestVersion)',stale.context);
  evaluate('++profileRequestVersion;profileController.abort();profileController=new AbortController()',stale.context);
  required(release)();
  await assert.rejects(loading,error=>isRecord(error)&&error.name==='AbortError');
  assert.equal(evaluate('profileTopicCache.size',stale.context),0);
  assert(required(required(stale.requests.find(request=>request.url==='data/topics/index.json')).signal).aborted);
  assert(!stale.requests.some(request=>request.url==='data/topics/0.json'));
  console.log('Profile topic shards: sparse IDs, empty/missing topics, deduplication, bounded caching, metadata validation and cancellation passed.');
}
async function benchmarkPersonalization() {
  // Fixed census and edit sequences; compare deterministic work, not noisy CPU
  // timings or public API latency. No production files or network are used.
  const raw: FixtureDataset=fixture();raw.metadata={language:'vec',complete:true,demo:false,
    started_at:'2026-10-01T00:00:00Z',finished_at:'2026-10-01T00:00:00Z',topics:{status:'predicted'}};
  const languages=['vec','en','fr','de'],size=65536;
  const files: Record<string,TopicFile>={'fixture.json':raw};
  const columns=['page_id','topic_status','topic_labels','title','orphan','length_bytes',
    'age_days','creator_key','in_degree','out_degree','quality_score','category'];
  const entries=languages.map(language=>({language,file:language==='vec'?'fixture.json':language+'.json',
    date:raw.metadata.finished_at,observations_file:language+'-observations.json',profile_topics_dir:language+'-topics'}));
  for(const entry of entries) {
    const metadata={...raw.metadata,language:entry.language};
    const rows: TopicRow[]=Array.from({length:size},(_,i)=>{
      const id=i+1,status=id%13===0?'missing':'predicted';
      const labels=status==='missing'?null:id%7===0?[]:[id%2?'Culture.Media.Music':'Culture.Media.Films'];
      return [id,status,labels,'Synthetic article '+id+' '+ 'x'.repeat(80),id%3===0,5000,1000,'registered:12',3,7,0.5,'Synthetic category'];
    });
    files[entry.observations_file]={format:'observations_v1',metadata,columns,rows};
    const buckets=new Map<number,TopicRow[]>();
    for(const row of rows) {
      const bucket=Math.floor(required(row[0])/1024);
      if(!buckets.has(bucket))buckets.set(bucket,[]);
      required(buckets.get(bucket)).push([row[0],row[1],row[2]]);
    }
    files[entry.profile_topics_dir+'/index.json']={format:'profile_topics_index_v1',metadata,
      shard_size:1024,shards:[...buckets.keys()],census_page_count:size};
    for(const [bucket,shardRows] of buckets)files[entry.profile_topics_dir+'/'+bucket+'.json']={
      format:'profile_topics_shard_v1',metadata,bucket,rows:shardRows};
  }
  let gated=false;
  const topicPending: (()=>void)[]=[];
  const result=await harness('deorphanize',raw,'?dataset=fixture.json',entries,{
    beforeFetch:(url,options)=>gated&&String(url).includes('-topics/')?new Promise<void>((resolve,reject)=>{
      topicPending.push(resolve);required(options.signal).addEventListener('abort',()=>reject(required(options.signal).reason),{once:true});
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
      const row=required(files[required(entry).observations_file].rows).find(row=>row[0]===edit.page_id);
      return {...edit,topic_labels:edit.namespace===0&&row?.[1]==='predicted'?row[2]:null};
    });
    const legacyFiles=entries.map(entry=>files[entry.observations_file]);
    const baseline={requests:legacyFiles.length,rows:legacyFiles.reduce((n,file)=>n+required(file.rows).length,0),
      bytes:legacyFiles.reduce((n,file)=>n+Buffer.byteLength(JSON.stringify(file)),0)};
    evaluate('profileTopicCache.clear()',result.context);
    result.context.benchmarkEdits=edits;
    const offset=result.requests.length;
    gated=true;
    let finished=false,rounds=0,maxConcurrent=0;
    const loading=evaluateAsync('observedTopics(benchmarkEdits,profileRequestVersion)',result.context)
      .then(value=>{finished=true;return value;});
    for(let turn=0;!finished;turn++) {
      assert(turn<40,'topic scheduler must finish');
      await new Promise<void>(resolve=>setImmediate(resolve));
      if(topicPending.length) {
        rounds++;maxConcurrent=Math.max(maxConcurrent,topicPending.length);
        assert(topicPending.length<=4,'shared topic request budget');
        for(const release of topicPending.splice(0))release();
      }
    }
    gated=false;
    const actual=await loading;
    assert.deepEqual(structuredClone(actual),reference);
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
    const warm=await evaluate('observedTopics(benchmarkEdits,profileRequestVersion)',result.context);
    assert.deepEqual(structuredClone(warm),reference);
    assert.equal(result.requests.length,warmOffset);
    console.log('Personalization deterministic benchmark: '+JSON.stringify({workload,edits:100,census_pages:size*4,
      baseline,optimized,sequential_sharded_rounds:sequentialRounds,parallel_sharded_rounds:rounds,max_concurrent:maxConcurrent,warm_requests:0}));
  }

  // A logical round completes only when the test releases all outstanding
  // responses. This makes the contribution fan-out benchmark reproducible.
  const wikiLanguages=Array.from({length:24},(_,i)=>'a'+String.fromCharCode(97+i));
  const pending: (()=>void)[]=[],contributions=wikiLanguages.flatMap((language,index)=>Array.from({length:100},(_,i)=>({
    language,page_id:1000+i,namespace:0,timestamp:new Date(Date.UTC(2026,9,1,0,Math.floor(index/2),Math.floor(i/2))).toISOString(),topic_labels:null})));
  const scopedEntries=[...entries,...wikiLanguages.map(language=>({language,file:language+'.json',date:raw.metadata.finished_at}))];
  const scheduled=await harness('deorphanize',raw,'?dataset=fixture.json',scopedEntries,{
    beforeFetch:(url,options)=>String(url).includes('list=usercontribs')?new Promise<void>((resolve,reject)=>{
      pending.push(resolve);required(options.signal).addEventListener('abort',()=>reject(required(options.signal).reason),{once:true});
    }):undefined,
    apiResponse:url=>url.hostname==='meta.wikimedia.org'?{query:{globaluserinfo:{id:1,name:'Editor',merged:wikiLanguages.map((language,i)=>({
      wiki:language+'wiki',url:'https://'+language+'.wikipedia.org',id:i+1}))}}}:
      {query:{usercontribs:contributions.filter(edit=>edit.language===url.hostname.split('.')[0]).map(edit=>({
        userid:wikiLanguages.indexOf(edit.language)+1,pageid:edit.page_id,ns:edit.namespace,timestamp:edit.timestamp}))}}
  },files);
  evaluate(`globalThis.retainedEditPeaks=[];const originalSort=Array.prototype.sort;
    Array.prototype.sort=function(...args){
      if(this.length&&this[0]?.namespace!==undefined&&this[0]?.topic_labels===null)retainedEditPeaks.push(this.length);
      return originalSort.apply(this,args);
    };`,scheduled.context);
  let finished=false;
  const recent=evaluateAsync("recentWikipediaEdits('Editor',profileController.signal)",scheduled.context).then(value=>{finished=true;return value;});
  let rounds=0,maxConcurrent=0;
  while(!finished) {
    await new Promise<void>(resolve=>setImmediate(resolve));
    if(pending.length) {
      maxConcurrent=Math.max(maxConcurrent,pending.length);rounds++;
      for(const release of pending.splice(0).reverse())release();
    }
    assert(rounds<=8);
  }
  const actual=await recent;
  contributions.sort((a,b)=>b.timestamp.localeCompare(a.timestamp)||a.language.localeCompare(b.language)||a.page_id-b.page_id);
  assert.deepEqual(structuredClone(actual),contributions.slice(0,100));
  assert.equal(rounds,8);assert.equal(maxConcurrent,3);
  const retainedPeak=evaluate('Math.max(...retainedEditPeaks)',scheduled.context);
  assert.equal(retainedPeak,200);
  console.log('Personalization deterministic contribution benchmark: '+JSON.stringify({wikipedias:24,edits:100,
    previous_rounds:3,current_rounds:rounds,max_concurrent:maxConcurrent,previous_retained_peak:2400,retained_peak:retainedPeak}));
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
  const raw=fixture(),summary=fixtureTransport('summary_file');
  const creatorIndex=fixtureTransport('creator_index_file');
  raw.metadata.demo=false;summary.metadata.demo=false;creatorIndex.metadata.demo=false;
  const entry: ManifestEntry={language:'vec',file:'fixture.json',date:raw.metadata.finished_at,demo:false,
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
  await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(articles.nodes['article-rows'].children.length,50);
  assert(articles.requests.some(request=>request.url==='data/fixture.json'));
  const stale=structuredClone(summary);required(stale.article_preview).pop();
  const rejected=await harness('articles',raw,'?dataset=fixture.json',[entry],{}, {...files,'fixture-summary.json':stale});
  assert.match(rejected.nodes.status.textContent,/Riepilogo non valido/);
  const legacy=structuredClone(summary);delete legacy.article_preview;
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
async function checkRelationalDashboard() {
  const raw=fixture(),summary=fixtureTransport('summary_file'),creators=fixtureTransport('creator_index_file');
  raw.metadata.demo=false;summary.metadata.demo=false;creators.metadata.demo=false;
  const entry: ManifestEntry={language:'vec',file:'fixture.json',date:raw.metadata.finished_at,storage:'relational_census_v1',summary_file:'fixture-summary.json',creator_index_file:'fixture-creators.json'};
  const files={'fixture-summary.json':summary};
  const requests: Record<string,unknown>[]=[];
  let releaseSlow: (()=>void) | undefined,slowSignal: AbortSignal | undefined;
  const pagesAPI=async(options: FetchOptions): Promise<MockResponse>=>{
    const query: unknown=JSON.parse(options.body??'{}');assert(isRecord(query));requests.push(query);
    if(query.search==='slow') {slowSignal=options.signal;await new Promise<void>(resolve=>{releaseSlow=resolve;});}
    const offset=Number(query.offset);
    const available=query.kind==='creators'?creators.creators:query.kind==='cards'?raw.pages.filter(page=>page.orphan):raw.pages;
    const rows=query.search?[available[query.search==='fast'?1:0]]:available;
    const selected=rows.slice(offset,offset+25);
    return {ok:true,json:async()=>query.kind==='creators'?{format:'creator_results_v1',dataset:query.dataset,offset,total:rows.length,creators:selected}:
      {format:'page_results_v1',dataset:query.dataset,offset,total:rows.length,metadata:summary.metadata,census_page_count:summary.census_page_count,
        pages:selected,link_candidates:raw.link_candidates,card_transports:{}}};
  };
  const articles=await harness('articles',raw,'?dataset=fixture.json',[entry],{pagesAPI},files);
  assert.equal(articles.nodes['article-rows'].children.length,25);
  assert.equal(requests[0].offset,0);
  assert(articles.requests.every(request=>!['data/fixture.json','data/fixture-creators.json'].includes(request.url)));
  articles.nodes['article-more'].listeners.click();await evaluateAsync('listHydration',articles.context);
  assert.equal(articles.nodes['article-rows'].children.length,50);assert.equal(requests[1].offset,25);
  articles.nodes['article-search'].value='slow';articles.nodes['article-search'].listeners.input();
  await new Promise<void>(resolve=>setImmediate(resolve));
  articles.nodes['article-search'].value='fast';articles.nodes['article-search'].listeners.input();await evaluateAsync('listHydration',articles.context);
  assert(slowSignal?.aborted);required(releaseSlow)();await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(articles.nodes['article-rows'].children.length,1);
  assert.equal(articles.nodes['article-rows'].children[0].children[0].children[0].textContent,raw.pages[1].title);
  articles.nodes['article-classification'].value='topics';articles.nodes['article-classification'].listeners.change();await evaluateAsync('listHydration',articles.context);
  assert.equal(requests.at(-1)?.classification,'topics');assert.equal(requests.at(-1)?.offset,0);
  const authors=await harness('creators',raw,'?dataset=fixture.json',[entry],{pagesAPI},files);
  assert.equal(authors.nodes['creator-rows'].children.length,Math.min(25,creators.creators.length));
  assert(!authors.requests.some(request=>request.url==='data/fixture-creators.json'));
  const pooled=structuredClone(summary);pooled.metadata={...pooled.metadata,language:'all',kind:'aggregate',languages:['vec','lmo']};
  pooled.census_page_count*=2;pooled.reports.overview.total*=2;
  if(pooled.article_preview)pooled.article_preview=pooled.article_preview.map(page=>({...page,language:'vec'}));
  const aggregate: ManifestEntry={...entry,language:'all',kind:'aggregate',file:'all.json',summary_file:'all-summary.json'};
  const combined=await harness('articles',raw,'?dataset=all.json',[aggregate,entry],{pagesAPI:async options=>{
    const query:unknown=JSON.parse(options.body??'{}');assert(isRecord(query));
    return {ok:true,json:async()=>({format:'page_results_v1',dataset:'all.json',offset:query.offset,total:pooled.census_page_count,
      metadata:pooled.metadata,census_page_count:pooled.census_page_count,pages:raw.pages.slice(0,25).map(page=>({...page,language:'vec'})),link_candidates:{},card_transports:{}})};
  }},{'all-summary.json':pooled});
  assert.equal(combined.nodes['article-rows'].children.length,25);
  assert(!combined.nodes['article-info'].textContent.includes('non valido'));
  const cards=await harness('deorphanize',raw,'?dataset=fixture.json',[entry],{pagesAPI},files);
  assert.equal(cards.nodes['candidate-cards'].children.filter(node=>node.className==='candidate-card').length,25);
  changeCheckbox(cards.nodes,'candidate-show-without-connections',true);await evaluateAsync('listHydration',cards.context);
  assert.equal(requests.at(-1)?.show_without_connections,true);
  assert(!cards.requests.some(request=>request.url==='data/fixture.json'));
  console.log('Relational dashboard: server pagination, filter parameters, cancellation, creators, cards and CSV routes passed.');
}

async function checkArticleIndex() {
  const raw=fixture(),summary=fixtureTransport('summary_file');
  const observations=fixtureTransport('observations_file');
  const articleIndex=fixtureTransport('articles_file');
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
    document.rows=document.rows.map(row=>document.columns.map(key=>required(byID.get(numberValue(row[document.columns.indexOf('page_id')])))[key]??null));
  const entry: ManifestEntry={language:'vec',file:'fixture.json',date:raw.metadata.finished_at,demo:false,
    summary_file:'fixture-summary.json',observations_file:'fixture-observations.json',articles_file:'fixture-articles.json'};
  const files={'fixture.json':raw,'fixture-summary.json':summary,'fixture-observations.json':observations,'fixture-articles.json':articleIndex};
  const legacyEntry: ManifestEntry={...entry};delete legacyEntry.articles_file;
  const legacy=await harness('articles',raw,'?dataset=fixture.json',[legacyEntry],{},files);
  const indexed=await harness('articles',raw,'?dataset=fixture.json',[entry],{},files);
  for(const result of [legacy,indexed])result.nodes['article-more'].listeners.click();
  await new Promise<void>(resolve=>setImmediate(resolve));
  assert.equal(indexed.nodes['article-rows'].children.length,50);
  assert(indexed.requests.some(request=>request.url==='data/fixture-articles.json'));
  assert(!indexed.requests.some(request=>['data/fixture.json','data/fixture-observations.json'].includes(request.url)));
  assert(legacy.requests.some(request=>request.url==='data/fixture-observations.json'));
  const rows=(result: Awaited<ReturnType<typeof harness>>)=>result.nodes['article-rows'].children.map(row=>[
    row.children[0].children[0].textContent,...row.children.slice(1).map(cell=>cell.textContent)]);
  const parity=()=>assert.deepEqual(rows(indexed),rows(legacy));
  const change=(id: string,value: string)=>{
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
  const bytes=(document: unknown)=>Buffer.byteLength(JSON.stringify(document));
  assert(bytes(articleIndex)<bytes(observations)*.5);
  for(const mutate of [
    (document: CompactObservations)=>document.census_page_count=required(document.census_page_count)+1,
    (document: CompactObservations)=>{document.rows.pop();document.census_page_count=required(document.census_page_count)-1;},
    (document: CompactObservations)=>document.metadata.finished_at='2000-01-01',
    (document: CompactObservations)=>document.metadata.measures_rule='obsolete',
    (document: CompactObservations)=>document.format='observations_v1',
    (document: CompactObservations)=>{document.columns.pop();document.rows.forEach(row=>row.pop());},
    (document: CompactObservations)=>document.rows[1][0]=document.rows[0][0]
  ]) {
    const bad=structuredClone(articleIndex);mutate(bad);
    const result=await harness('articles',raw,'?dataset=fixture.json',[entry],{}, {...files,'fixture-articles.json':bad});
    result.nodes['article-more'].listeners.click();await new Promise<void>(resolve=>setImmediate(resolve));
    assert.match(result.nodes['article-info'].textContent,/non valid/);
  }
  const lmo=fixtureTransport('articles_file','lmo-demo.json');lmo.metadata.demo=false;
  const lmoEntry={language:'lmo',file:'lmo-demo.json',date:lmo.metadata.finished_at,demo:false,articles_file:'lmo-demo-articles.json'};
  const aggregate={metadata:{...raw.metadata,language:'all',kind:'aggregate',languages:['vec','lmo']},pages:[],
    members:[{language:'vec',file:entry.file,page_count:raw.pages.length,metadata:raw.metadata},
      {language:'lmo',file:lmoEntry.file,page_count:lmo.census_page_count,metadata:lmo.metadata}]};
  const pooled=await harness('articles',raw,'?dataset=fixture.json',[entry,lmoEntry],{}, {...files,'lmo-demo-articles.json':lmo});
  pooled.context.articleAggregate=aggregate;
  const result=await evaluate('loadAggregate(articleAggregate,loadVersion,true)',pooled.context);
  assert(isRecord(result)&&isList(result.pages));
  assert.equal(result.pages.length,raw.pages.length+required(lmo.census_page_count));
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
  assert.equal(required(newArticles).children[2].textContent,'— (0/0)');
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
  const bar=(name: string)=>required(articles.nodes['features-paper'].children.find(node=>node.className==='bar-row'&&node.children[0].textContent===name));
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
  const light=required(css.match(/:root\s*\{([^}]+)\}/))[1],dark=required(css.match(/:root\[data-theme="dark"\]\s*\{([^}]+)\}/))[1];
  const luminance=(hex: string)=>{
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
(async()=>{try{await checkFreshResources();await checkProjectFooter();await checkPage(false);await checkPage(true);await checkPaperProfiles();await checkDeorphanize();await checkCandidateSorting();await checkHiddenPages();await checkConnections();await checkFixPrompts();await checkSingleConnectionPrompts();await checkLlmSelection();await checkWikitextPreparation();await checkDatasetSelection();await checkProgressiveCandidates();await checkCompactLists();await checkCreatorBots();await checkPreferences();await checkAggregation();await checkCompactTransport();await checkEditorialTransports();await checkTopicExplorer();await checkConnectionCopy();await checkSharedEditorialReviews();await checkWikipediaAuthentication();await checkPersonalization();await checkPersonalizationRecovery();await checkProfileTopicShards();await benchmarkPersonalization();await checkCreatorHistory();await checkPreparedSummaries();await checkRelationalDashboard();await checkReconstructedCreatorFallback();await checkMissingCreationAge();await checkReconstructedCreationQualityAndCreators();checkThemeContrast();}finally{fs.rmSync(fixtureDir,{recursive:true});}})().catch(error=>{console.error(error);process.exitCode=1;});
