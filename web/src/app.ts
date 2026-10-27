import {createExplorer} from './explorer.js';
import {wikimediaAPI} from './wikimedia.js';
import {configureProfile,clearProfileCache,recentWikipediaEdits,readProfileTopics} from './profile.js';
import {configureEditorial, resetReviews, setConnectionCopyRequest, wikiLink, candidateReasons, cardTitle, candidateOrder, reviewedEdge, closedEdge, reviewedOnly, reviewBadge, loadSharedReviews, closeReview, activePromptReview, showReview, fixRequest, updatePreparedPrompt, closePrompt, copyPrompt, connectionCopyRequest, copyConnectionPrompt, fixLink} from './editorial.js';
import {createCharts} from './charts.js';
import type {PageObservation, Candidate, CandidateRecord, Dataset, ManifestEntry, Provider, CreatorRow, DashboardModule, DashboardSummaryData, RecentEdit, InterestReport} from './types.js';
import {ARTICLE_INDEX_FIELDS} from './types.js';
import {$, element, creatorsPage, connectionsPage, deorphanizePage} from './dom.js';
import {t, number, readPreference, savePreference, uiLanguage, setUILanguage} from './i18n.js';
import {validateDataset, readConnections, readSummary, validateManifest, canonicalJSON} from './datasets.js';
import {isRecord} from './validation.js';
import {calculateReport, setCalculator} from './reports.js';

declare const nativeModuleUrl: string;
const llmProviders: Record<string, Provider> = {
  qwen:{name:'Qwen',url:'https://chat.qwen.ai/'},
  kimi:{name:'Kimi',url:'https://www.kimi.ai/'},
  glm:{name:'GLM',url:'https://chat.z.ai/'},
  mistral:{name:'Mistral',url:'https://chat.mistral.ai/chat'},
  other:{name:'Altro',url:null}
};
let llmProvider=readPreference('orphanwiki-llm','qwen',Object.keys(llmProviders));
let theme=readPreference('orphanwiki-theme','system',['system','light','dark']);
const systemTheme=window.matchMedia?.('(prefers-color-scheme: dark)');
function applyTheme() {
  const active=theme==='system'?(systemTheme?.matches?'dark':'light'):theme;
  document.documentElement.dataset.theme=active;
  $('theme-light-icon')!.hidden=active!=='light';
  $('theme-dark-icon')!.hidden=active!=='dark';
  const description=t(active==='light'?'Modalità chiara: passa alla modalità scura':'Modalità scura: passa alla modalità chiara');
  $('theme-toggle')!.setAttribute('aria-label',description);
  $('theme-toggle')!.title=description;
}
function applyTranslations() {
  document.documentElement.lang=uiLanguage;
  const languageButton=$('ui-language-toggle')!;
  languageButton.textContent=uiLanguage.toUpperCase();
  const description=t(uiLanguage==='it'?'Interfaccia italiana: passa all’inglese':'Interfaccia inglese: passa all’italiano');
  languageButton.setAttribute('aria-label',description);languageButton.title=description;
  applyTheme();
  for(const node of document.querySelectorAll<HTMLElement>('[data-i18n]'))node.textContent=t(node.dataset.i18n!);
  for(const attr of ['placeholder','aria-label','title'])for(const node of document.querySelectorAll(`[data-i18n-${attr}]`))node.setAttribute(attr,t(node.getAttribute('data-i18n-'+attr)!));
  if(deorphanizePage&&$('dataset'))for(const option of $('dataset')!.children) {
    if((option as HTMLOptionElement).value==='')option.textContent=t('Seleziona');
    if((option as HTMLOptionElement).value==='all')option.textContent=t('Tutte le lingue');
  }
  if(deorphanizePage&&datasetEntries.length&&$('dataset')!.value==='')$('status')!.textContent=t('Seleziona una lingua per visualizzare le voci orfane.');
}

const pageLanguage = (row: PageObservation) => row.language || data.metadata.language;
const pageKey = (row: PageObservation) => data.metadata.kind==='aggregate'?`${pageLanguage(row)}:${row.page_id}`:String(row.page_id);
const creatorMode = (): 'observed' | 'history' | 'missing' =>
  ['collected','partially_collected'].includes(data.metadata.creators?.status??'')?'observed':
  ['collected','partially_collected'].includes(data.metadata.history?.status??'')?'history':'missing';
const creatorsAvailable = () => creatorMode()!=='missing';
const label = (row: PageObservation) => (data.metadata.kind==='aggregate'?pageLanguage(row).toUpperCase()+' · ':'')+(row.category ?? t('Senza categoria idonea'));
let data: Dataset, datasetFile: string, loadVersion = 0, connectionLimit = 50, creatorRows: CreatorRow[] = [];
let datasetController = new AbortController();
let precomputed: DashboardSummaryData;
let profileRequestVersion = 0;
let profileController = new AbortController();
let profileContributions: RecentEdit[] | null = null;
let cardRequestVersion=0,cardController=new AbortController(),cardHydration: Promise<void>=Promise.resolve();
let activeCardFetches=0;
const waitingCardFetches: (()=>void)[]=[];

let authEnabled: boolean | null=null,authUsername: string | null=null;
let authError: string | null=null,authBusy=false;
const validUsername=(value: unknown): value is string=>typeof value==='string'&&value.length>0&&value.length<=255&&!/[\u0000-\u001f\u007f#<>|\[\]{}]/.test(value);
function userPage(username: string, language: string): string {
  const edition=/^[a-z][a-z0-9-]{0,19}$/.test(language)&&language!=='all'?language:uiLanguage;
  return `https://${edition}.wikipedia.org/wiki/User:${encodeURIComponent(username.replaceAll(' ','_'))}`;
}
function renderAuth() {
  $('auth-login')!.hidden=authUsername!==null;
  $('auth-login')!.disabled=authBusy;
  $('auth-login')!.setAttribute('aria-busy',String(authBusy));
  $('auth-toggle')!.setAttribute('aria-label',t('Account Wikimedia')+(authUsername?' · '+authUsername:''));
  const user=$('auth-user')!;user.hidden=authUsername===null;user.textContent=authUsername??'';
  if(authUsername)user.href=userPage(authUsername,data?.metadata.language??uiLanguage);
  $('auth-logout')!.hidden=authUsername===null;
  const message=authError??(authEnabled!==true?'Accesso Wikimedia non disponibile al momento. Riprova.':'');
  $('auth-status')!.textContent=t(message);$('auth-status')!.hidden=!message;
  if($('profile-username')) { $('profile-username')!.disabled=authUsername!==null;if(authUsername)$('profile-username')!.value=authUsername; }
}
function clearProfile() {
  ++profileRequestVersion;profileController.abort();profileController=new AbortController();
  profileContributions=null;clearProfileCache();
  if($('profile-username'))$('profile-username')!.value='';
  if($('profile-status'))$('profile-status')!.textContent='';
  if($('profile-sort-option'))$('profile-sort-option')!.disabled=true;
  if($('profile-apply'))$('profile-apply')!.disabled=false;
  if($('candidate-sort')?.value==='personalized')$('candidate-sort')!.value='candidates-desc';
}
async function authRequest(action: 'session' | 'logout'): Promise<void> {
  const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),8000);
  try {
    const response=await fetch('api/auth/'+action,{cache:'no-store',credentials:'same-origin',signal:controller.signal,
      ...(action==='logout'?{method:'POST',headers:{'Content-Type':'application/json'},body:'{}'}:{})});
    if(!response.ok)throw new Error('Accesso Wikipedia non disponibile');
    const payload: unknown=await response.json();
    if(!isRecord(payload)||typeof payload.enabled!=='boolean'||!(payload.username===null||validUsername(payload.username))||!payload.enabled&&payload.username!==null)throw new Error('Accesso Wikipedia non disponibile');
    if(authUsername!==payload.username)clearProfile();
    authEnabled=payload.enabled;authUsername=payload.username;authError=null;
  } finally {clearTimeout(timer);}
  renderAuth();
}
async function loadAuth() {await authRequest('session');}
async function login() {
  if(authBusy)return;
  authBusy=true;authError=null;renderAuth();
  try {
    await authRequest('session');
    if(authEnabled!==true)throw new Error('Accesso non disponibile');
    if(authUsername)return;
    const page=connectionsPage?'connections.html':deorphanizePage?'deorphanize.html':creatorsPage?'creators.html':'index.html';
    window.location.assign('api/auth/login?return_to='+encodeURIComponent(page+window.location.search));
  } catch {authError='Accesso Wikimedia non disponibile al momento. Riprova.';}
  finally {authBusy=false;renderAuth();}
}
async function logout() {
  $('auth-logout')!.disabled=true;
  try {await authRequest('logout');closePrompt();if(data)renderDataset();}
  catch {authError='Accesso Wikimedia non disponibile al momento. Riprova.';renderAuth();}
  finally {$('auth-logout')!.disabled=false;}
}

async function observedTopics(edits: RecentEdit[], version: number): Promise<RecentEdit[] | null> {
  if(version!==profileRequestVersion||!data)return null;
  const enriched=edits.map(edit=>({...edit,topic_labels:null as string[] | null}));
  const groups=new Map<string,RecentEdit[]>();
  for(const edit of enriched)if(edit.namespace===0&&edit.page_id>0) {
    const group=groups.get(edit.language)??[];group.push(edit);groups.set(edit.language,group);
  }
  const signal=profileController.signal;
  let active=0;
  const waiting: (() => void)[]=[];
  // Share four download/JSON slots across all languages and their shard workers.
  async function schedule(operation: () => Promise<unknown>): Promise<unknown> {
    if(active>=4)await new Promise<void>(resolve=>waiting.push(resolve));else active++;
    try {signal.throwIfAborted();return await operation();}
    finally {const next=waiting.shift();if(next)next();else active--;}
  }
  const jobs: {entry: ManifestEntry; edits: RecentEdit[]}[]=[];
  for(const [language,edits] of groups) {
    // The manifest is sorted newest first, independently of displayed cards.
    const entry=datasetEntries.find(item=>item.language===language&&item.kind!=='aggregate');
    if(!entry?.profile_topics_dir)throw new Error('Trasporto osservazioni non disponibile');
    jobs.push({entry,edits});
  }
  async function observe(job: typeof jobs[number]) {
    signal.throwIfAborted();
    const found=await readProfileTopics(job.entry,new Set(job.edits.map(edit=>edit.page_id)),signal,schedule);
    if(version!==profileRequestVersion)return;
    for(const edit of job.edits)edit.topic_labels=found.get(edit.page_id)??null;
  }
  let next=0;
  async function worker() {
    while(next<jobs.length)await observe(jobs[next++]);
  }
  try {
    await Promise.all(Array.from({length:Math.min(4,jobs.length)},()=>worker()));
  } catch(error) {
    if(version===profileRequestVersion)profileController.abort();
    throw error;
  }
  if(version!==profileRequestVersion)return null;
  return enriched;
}
async function scoreProfile(edits: RecentEdit[], version: number) {
  $('profile-status')!.textContent=t('Caricamento dei temi delle pagine modificate…');
  const enriched=await observedTopics(edits,version);
  if(version!==profileRequestVersion||!enriched||!data)return;
  if(!enriched.some(edit=>edit.topic_labels?.length))throw new Error('Nessuna delle ultime 100 modifiche nelle Wikipedie analizzate ha temi utilizzabili nelle ultime raccolte disponibili.');
  const report=calculateReport<InterestReport>('interests',{edits:enriched,candidates:[],aggregate:data.metadata.kind==='aggregate'});
  explorer.setProfileEdits(enriched);
  $('profile-sort-option')!.disabled=false;
  $('candidate-sort')!.value='personalized';candidateCards();
  const languages=[...new Set(enriched.filter(edit=>edit.topic_labels?.length).map(edit=>edit.language))].sort();
  $('profile-status')!.textContent=t('Profilo: {edits} modifiche nelle Wikipedie analizzate; temi utilizzabili per {known}, nelle lingue {languages}. Le altre modifiche restano senza temi utilizzabili. Fonti: ultime raccolte disponibili per lingua.',
    {edits:number(report.total_edits),known:number(report.known_topic_edits),languages:languages.map(language=>language.toUpperCase()).join(', ')});
}
async function applyProfile() {
  if(deorphanizePage&&(!$('dataset')!.value||document.body.dataset.loading==='true'))return;
  const version=++profileRequestVersion;
  profileController.abort();profileController=new AbortController();
  const button=$('profile-apply')!;button.disabled=true;
  $('profile-sort-option')!.disabled=true;
  if($('candidate-sort')!.value==='personalized')$('candidate-sort')!.value='candidates-desc';
  $('profile-status')!.textContent=t('Caricamento delle ultime 100 modifiche nelle Wikipedie analizzate…');
  try {
    const edits=await recentWikipediaEdits(authUsername??$('profile-username')!.value.trim(),profileController.signal,
      message=>{if(version===profileRequestVersion)$('profile-status')!.textContent=message;});
    if(version!==profileRequestVersion)return;
    profileContributions=edits;
    if(!edits.length)throw new Error('L’utente non ha modifiche pubbliche nelle Wikipedie analizzate collegate');
    await scoreProfile(edits,version);
  } catch(error) {
    if(version===profileRequestVersion) {
      profileContributions=null;$('profile-sort-option')!.disabled=true;
      throw error;
    }
  } finally {if(version===profileRequestVersion)button.disabled=false;}
}
// Taxonomy labels are supplied by the same Rust configuration as inference.
let topicTaxonomy: Record<string, string[]> = {};
let topicDisplayNames = new Map<string,string>();
const {paperProfiles, categories, distribution, creatorCharts, historyCharts}=createCharts({
  get data(){return data;}, get precomputed(){return precomputed;},
  get topicDisplayNames(){return topicDisplayNames;}, creatorMode
});
function articleLabels(page: PageObservation): string[] {
  const mode=$('article-classification')?.value||'categories';
  if(mode==='categories')return [label(page)];
  if(page.topic_status!=='predicted'||!Array.isArray(page.topic_labels))return [t('Temi non disponibili')];
  const names=mode==='macro'?Object.keys(topicTaxonomy).filter(parent=>page.topic_labels!.some(topic=>topic.startsWith(parent+'.'))):
    page.topic_labels.map(topic=>topicDisplayNames.get(topic)).filter((name): name is string=>typeof name==='string').sort();
  return names.length?names:[t('Nessun tema sopra soglia')];
}
function articleList() {
  if(!$('article-rows')!)return;
  if(databaseList('articles',articleList))return;
  const visible=data.pages;
  const box=$('article-rows')!;box.replaceChildren();
  for(const page of visible) {
    const row=element('tr'),title=element('td');title.append(wikiLink(page.title,pageLanguage(page)));
    row.append(title,...[articleLabels(page).join(' · '),page.orphan?t('Sì'):t('No'),number(page.length_bytes),page.created_at?new Date(page.created_at).toISOString().slice(0,10):'—',number(page.in_degree),number(data.link_candidates?.[pageKey(page)]?.candidate_count)].map(value=>element('td',value)));
    box.append(row);
  }
  $('article-info')!.textContent=t('{n} voci · {shown} visualizzate',{n:number(data.pages.length),shown:number(visible.length)});
}
function creatorListInfo(total: number,shown: number): string {
  return creatorsAvailable()?t(creatorMode()==='history'?
    '{n} creatori ricostruiti · {shown} visualizzati. ID storici locali; attribuzione dalla prima revisione osservata. Tipo account alla data del dump; conteggi limitati alle voci attribuite nel censimento.':
    '{n} creatori · {shown} visualizzati. Identificativi locali; lingue limitate alle wiki esaminate.',
    {n:number(total),shown:number(shown)}):t('Dati sui creatori non disponibili per questa raccolta.');
}
function creatorList() {
  if(!$('creator-rows')!)return;
  if(databaseList('creators',creatorList))return;
  const articlesHeading=document.getElementById('creator-articles-heading');
  if(articlesHeading)articlesHeading.hidden=creatorMode()==='history';
  const visible=creatorRows;
  const box=$('creator-rows')!;box.replaceChildren();
  const types: Record<string, string> = {registered:'Registrato',bot:'Bot',temporary:'Temporaneo',no_bot_flag:'Senza flag bot'};
  for(const creator of visible) {
    const row=element('tr');
    row.append(...[creator.key,t(types[creator.type ?? '']||'—'),...(creatorMode()==='history'?[]:[number(creator.articles)]),number(creator.total),number(creator.orphans),number(creator.percent)+'%',[...creator.languages].sort().join(', ').toUpperCase()||'—'].map(value=>element('td',value)));
    box.append(row);
  }
  $('creator-info')!.textContent=creatorListInfo(visible.length,visible.length);
}
interface VisibleCardRequest { page: PageObservation; record: CandidateRecord; footer: HTMLElement; connect: HTMLAnchorElement; }
async function readVisibleCard(filename: string,signal: AbortSignal): Promise<unknown> {
  if(!/^[a-zA-Z0-9][a-zA-Z0-9_.-]*\/[1-9][0-9]*\.json$/.test(filename))throw new Error('Nome dataset non valido');
  if(activeCardFetches>=4)await new Promise<void>(resolve=>waitingCardFetches.push(resolve));else activeCardFetches++;
  try {
    signal.throwIfAborted();
    const response=await fetch('data/'+filename,{cache:'no-store',credentials:'omit',signal});
    if(!response.ok)throw new Error('Dataset non disponibile');
    return await response.json();
  } finally {const next=waitingCardFetches.shift();if(next)next();else activeCardFetches--;}
}
async function hydrateVisibleCards(requests: VisibleCardRequest[],request: number,signal: AbortSignal): Promise<void> {
  const snapshot=data,version=loadVersion;
  const current=()=>!signal.aborted&&request===cardRequestVersion&&version===loadVersion&&data===snapshot;
  let position=0;
  await Promise.all(Array.from({length:Math.min(4,requests.length)},async()=>{
    while(current()&&position<requests.length) {
      const {page,record,footer,connect}=requests[position++];
      try {
        const transport=snapshot.card_transports?.[pageLanguage(page)];
        if(!transport)throw new Error('Osservazioni compatte non valide');
        const payload=await readVisibleCard(`${transport.directory}/${page.page_id}.json`,signal);
        if(!current())return;
        if(!isRecord(payload)||!isRecord(payload.page)||!isRecord(payload.candidate_record))throw new Error('Osservazioni compatte non valide');
        const fullPage=payload.page,fullRecord=payload.candidate_record;
        if(payload.format!=='orphan_card_v1'||payload.card_identity!==transport.identity||
          payload.census_page_count!==transport.census_page_count||fullPage.page_id!==page.page_id||
          fullPage.title!==page.title||fullPage.orphan!==true||
          canonicalJSON(ARTICLE_INDEX_FIELDS.map(key=>fullPage[key]??null))!==canonicalJSON(ARTICLE_INDEX_FIELDS.map(key=>page[key]??null))||
          fullRecord.status!==record.status||fullRecord.candidate_count!==record.candidate_count||
          !Array.isArray(fullRecord.candidates))throw new Error('Osservazioni compatte non valide');
        const candidates: unknown[]=fullRecord.candidates;
        if(canonicalJSON(candidates.map(candidate=>isRecord(candidate)?candidate.page_id:null))!==canonicalJSON(record.candidate_ids)||
          candidates.some(candidate=>!isRecord(candidate)||typeof candidate.title!=='string'||!candidate.title.trim()||
            (candidate.out_degree!=null&&(!Number.isSafeInteger(candidate.out_degree)||Number(candidate.out_degree)<0))||
            (candidate.dead_end!=null&&typeof candidate.dead_end!=='boolean')||!Array.isArray(candidate.evidence)||candidate.evidence.length!==1||
            candidate.evidence.some((edge: unknown)=>!isRecord(edge)||typeof edge.language!=='string'||
              !/^[a-z][a-z0-9-]{0,19}$/.test(edge.language)||['all',pageLanguage(page)].includes(edge.language)||
              typeof edge.source_title!=='string'||!edge.source_title.trim()||typeof edge.target_title!=='string'||!edge.target_title.trim()||
              [edge.source_page_id,edge.target_page_id].some(id=>id!=null&&(!Number.isSafeInteger(id)||Number(id)<=0)))))
          throw new Error('Osservazioni compatte non valide');
        validateDataset({metadata:transport.metadata,pages:[fullPage as PageObservation]});
        record.candidates=candidates as Candidate[];
        footer.replaceChildren(fixLink(fixRequest(page,record)),connect);
      } catch(error) {if(current())throw error;}
    }
  }));
}
// These are category labels, not a classification inferred from article titles or topics.
const specialOrphanCategories=new Set([
  'disambigua','disambigue','disambiguazione','disambiguazioni','disanbigua','disanbigue',
  'registar da pagin da desambiguazziun',
  'pagine di disambiguazione','disambiguation','disambiguation pages',
  'pagines de desambiguacio','paginas de desambiguacao','homonymie','homonymies',
  'insiemi di caratteri','set di caratteri','character sets','jeux de caracteres',
  'conjunts de caracters','conjuntos de caracteres'
]);
const redirectOrphanCategories=new Set([
  'redirect','redirects','pagine di redirect','redirezioni','redireccions','redirecionamentos','redirections'
]);
function orphanCategoryName(page: PageObservation): string {
  return (page.category??'').normalize('NFD').replace(/\p{M}/gu,'').toLowerCase().replace(/[_\s]+/g,' ').trim();
}
// Exact category labels only; these UI filters do not infer article types.
const nameIndexCategories=['nomi','prenomi','cognomi','given names','surnames','prénoms','patronymes','cognoms','prenoms','sobrenomes','prenomes'];
const setIndexCategories=['set index articles','lists of lists','liste di liste','listes de listes','listas de listas','llistes de llistes'];
const maintenanceCategories=['temporary maintenance holdings','temporary maintenance holdings/rfd'];
const softRedirectCategories=['soft redirect','soft redirects','redirect interwiki','interwiki soft redirects'];
function filterCategoryName(category: string): string {return category.toLowerCase().replaceAll('_',' ').trim();}
function hiddenTargetCategories(): string[] {
  return [
    ...(!$('candidate-show-names')?.checked?nameIndexCategories:[]),
    ...(!$('candidate-show-indexes')?.checked?setIndexCategories:[]),
    ...(!$('candidate-show-maintenance')?.checked?maintenanceCategories:[]),
    ...(!$('candidate-show-redirects')?.checked?softRedirectCategories:[])
  ];
}
const linkFilterIds=['candidate-hide-link-disambiguation','candidate-hide-link-names','candidate-hide-link-redirects'] as const;
function hiddenSuggestedLink(page: PageObservation,candidate: Candidate): boolean {
  if(!linkFilterIds.some(id=>$(id)?.checked))return false;
  const category=candidate.category??data.pages.find(source=>source.page_id===candidate.page_id&&pageLanguage(source)===pageLanguage(page))?.category;
  if(typeof category!=='string')return false;
  const normalized=filterCategoryName(category),folded=orphanCategoryName({...page,category});
  return Boolean($('candidate-hide-link-disambiguation')?.checked&&specialOrphanCategories.has(folded)&&
      !['insiemi di caratteri','set di caratteri','character sets','jeux de caracteres','conjunts de caracters','conjuntos de caracteres'].includes(folded)||
    $('candidate-hide-link-names')?.checked&&nameIndexCategories.includes(normalized)||
    $('candidate-hide-link-redirects')?.checked&&(redirectOrphanCategories.has(folded)||softRedirectCategories.includes(normalized)));
}
function candidateCards() {
  if(!deorphanizePage||$('dataset')!.value==='')return;
  if(databaseList('cards',candidateCards))return;
  const request=++cardRequestVersion;cardController.abort();cardController=new AbortController();
  const records=data.link_candidates;
  if(!records)throw new Error('Risultati del confronto mancanti');
  const visible=data.pages;
  const box=$('candidate-cards')!;box.replaceChildren();
  const keys=new Set(visible.map(pageKey)),pending: VisibleCardRequest[]=[];
  for(const [key,record] of Object.entries(records))if(record?.candidate_ids&&!keys.has(key))delete record.candidates;
  for(const page of visible) {
    const record=records[pageKey(page)],count=record?.candidate_count;
    const card=element('article',null,'candidate-card'),body=element('div',null,'candidate-body');
    const header=element('div',null,'candidate-header');
    header.append(cardTitle('h3',page.title,pageLanguage(page)),element('span',count==null?'—':number(count),'candidate-badge'));
    body.append(header,element('p',label(page),'candidate-category'),element('p',count==null?t('Dati non disponibili'):t('{n} pagine candidate',{n:number(count)}),'candidate-hint'));
    const reviewed=(record?.candidate_ids??record?.candidates?.map(candidate=>candidate.page_id)??[]).filter(id=>closedEdge(page,id)).length;
    if(reviewed)body.append(element('p',t('{n} coppie segnate come Fixato o Bad link, escluse dai prompt.',{n:number(reviewed)}),'candidate-hint'));
    const footer=element('div',null,'candidate-footer'),connect=element('a',t('Connetti'),'connect-button');
    connect.href=`connections.html?dataset=${encodeURIComponent(datasetFile)}&page=${encodeURIComponent(page.page_id)}`;
    if(data.metadata.kind==='aggregate') connect.href+=`&language=${encodeURIComponent(pageLanguage(page))}`;
    connect.setAttribute('aria-label',t('Connetti {title}',{title:page.title}));
    footer.append(fixLink(fixRequest(page,record)),connect);card.append(body,footer);box.append(card);
    if(data.card_transports?.[pageLanguage(page)]&&record?.candidate_ids?.length&&!record.candidates&&!reviewedOnly(page,record)) {
      footer.children[0].setAttribute('title',t('Caricamento…'));
      pending.push({page,record,footer,connect});
    }
  }
  if(!visible.length)box.append(element('p',t('Nessuna voce orfana corrispondente.')));
  cardHydration=hydrateVisibleCards(pending,request,cardController.signal);void cardHydration.catch(fail);
}
function renderConnections() {
  const id=new URLSearchParams(window.location.search).get('page');
  if(!/^[1-9][0-9]*$/.test(id||''))throw new Error('Identificativo della pagina non valido');
  const language=new URLSearchParams(window.location.search).get('language');
  const page=data.pages.find(p=>String(p.page_id)===id&&(data.metadata.kind!=='aggregate'||pageLanguage(p)===language));
  if(!page||!page.orphan)throw new Error('Pagina orfana non presente in questa raccolta');
  $('connection-title')!.textContent=page.title;
  const target=$('connection-target')!;target.replaceChildren();
  target.append(element('h2',t('Pagina da collegare')),wikiLink(page.title,pageLanguage(page)),element('p',t('Lingua: {language} · {category}',{language:pageLanguage(page).toUpperCase(),category:label(page)})));
  const record=data.link_candidates?.[pageKey(page)],count=record?.candidate_count;
  setConnectionCopyRequest(fixRequest(page,record));
  const copyButton=$('connection-copy-prompt')!;
  copyButton.disabled=!connectionCopyRequest;
  copyButton.textContent=t('Prepara prompt e fonti');
  copyButton.title=t('Prepara il prompt della voce e il wikitesto delle prime cinque candidate disponibili');
  const candidates=(record?.candidates||[]).filter(candidate=>!hiddenSuggestedLink(page,candidate)).sort((a,b)=>Number(closedEdge(page,a.page_id))-Number(closedEdge(page,b.page_id))||candidateOrder(a,b));
  const box=$('connection-candidates')!;box.replaceChildren();
  $('connection-info')!.textContent=count==null?(t(candidateReasons[record?.status ?? '']||'Confronto tra lingue non ancora disponibile.'))
    :count===0?t('Nessuna pagina candidata individuata con le corrispondenze disponibili.'):t('{n} pagine candidate · {shown} visualizzate. Verifica il contenuto prima di aggiungere il collegamento.',{n:number(count),shown:number(Math.min(connectionLimit,candidates.length))});
  const hidden=(record?.candidates?.length??0)-candidates.length;
  if(hidden)$('connection-info')!.textContent+=' '+t('{n} collegamenti nascosti dai filtri.',{n:number(hidden)});
  for(const candidate of candidates.slice(0,connectionLimit)) {
    const card=element('article',null,'candidate-card connection-card'),body=element('div',null,'candidate-body');
    const header=element('div',null,'candidate-header');header.append(cardTitle('h2',candidate.title,pageLanguage(page)));
    if(candidate.dead_end===true)header.append(element('span','Dead-end page','dead-end-badge'));
    const review=reviewedEdge(page,candidate.page_id);
    if(review)header.append(reviewBadge(review));
    body.append(header);
    const proposed=element('div',null,'connection-proposed');
    proposed.append(element('h3',t('Collegamento da valutare in {language}',{language:pageLanguage(page).toUpperCase()})));
    const local=element('p');local.append(wikiLink(candidate.title,pageLanguage(page)),element('span',' → '),wikiLink(page.title,pageLanguage(page)));proposed.append(local);body.append(proposed);
    for(const evidence of candidate.evidence||[]) {
      const existing=element('div',null,'connection-evidence');existing.append(element('h3',t('Collegamento esistente in {language}',{language:evidence.language.toUpperCase()})));
      const line=element('p');line.append(wikiLink(evidence.source_title,evidence.language),element('span',' → '),wikiLink(evidence.target_title,evidence.language));existing.append(line);body.append(existing);
    }
    const footer=element('div',null,'candidate-footer');footer.append(fixLink(fixRequest(page,record,candidate)));
    const reviewButton=element('button',t('Valuta coppia'),'review-edge');reviewButton.type='button';reviewButton.disabled=Boolean(data.metadata.demo);
    reviewButton.addEventListener('click',()=>showReview({dataset:datasetFile,language:pageLanguage(page),target_id:page.page_id,
      target_title:page.title,sources:[{page_id:candidate.page_id,title:candidate.title}]}));footer.append(reviewButton);
    card.append(body,footer);box.append(card);
  }
  $('connection-more')!.hidden=connectionLimit>=candidates.length;
}
let datasetEntries: ManifestEntry[] = [];
function languageEntries(language: string): ManifestEntry[] {return datasetEntries.filter(entry=>entry.language===language);}
function refreshDateChoices(preferred='latest') {
  const dates=$('dataset-date');if(!dates)return;
  const available=[...new Set(languageEntries($('dataset')!.value).map(entry=>entry.date.slice(0,10)))].sort().reverse();
  dates.replaceChildren(new Option(t('Ultima disponibile'),'latest'),...available.map(date=>new Option(date,date)));
  dates.value=available.includes(preferred)?preferred:'latest';
}
function selectedEntry(): ManifestEntry | undefined {
  const choices=languageEntries($('dataset')!.value);
  const date=$('dataset-date')!.value;
  return date==='latest'?choices[0]:choices.find(entry=>entry.date.startsWith(date));
}
function datasetLinks(entry: ManifestEntry) {
  if($('download')){$('download')!.hidden=entry.kind==='aggregate';$('download')!.href='api/export?dataset='+encodeURIComponent(entry.file);}
  $('articles-link')!.href='index.html?dataset='+encodeURIComponent(entry.file);
  $('creators-link')!.href='creators.html?dataset='+encodeURIComponent(entry.file);
  $('deorphanize-link')!.href='deorphanize.html?dataset='+encodeURIComponent(entry.file);
}
function clearLanguageSelection() {
  resetRemoteList();
  ++loadVersion;datasetController.abort();
  ++cardRequestVersion;cardController.abort();cardHydration=Promise.resolve();
  ++profileRequestVersion;profileController.abort();
  closePrompt();lastError=null;
  document.body.dataset.loading='true';
  $('dataset-date')!.disabled=true;
  $('profile-apply')!.disabled=true;
  $('download')!.hidden=true;
  $('candidate-cards')!.replaceChildren();$('candidate-info')!.textContent='';
  $('status')!.hidden=false;$('status')!.textContent=t('Seleziona una lingua per visualizzare le voci orfane.');
  for(const [id,file] of [['articles-link','index.html'],['creators-link','creators.html'],['deorphanize-link','deorphanize.html']] as const)$(id)!.href=file;
}
async function load(entry: ManifestEntry) {
  resetRemoteList();
  closePrompt();
  ++cardRequestVersion;cardController.abort();cardHydration=Promise.resolve();
  const version=++loadVersion;
  datasetController.abort();datasetController=new AbortController();
  const signal=datasetController.signal;
  ++profileRequestVersion;
  profileController.abort();profileController=new AbortController();
  if($('profile-apply'))$('profile-apply')!.disabled=deorphanizePage;
  if($('dataset-date'))$('dataset-date')!.disabled=false;
  if($('profile-sort-option'))$('profile-sort-option')!.disabled=true;
  if($('candidate-sort')?.value==='personalized')$('candidate-sort')!.value='candidates-desc';
  setConnectionCopyRequest(null);
  if($('connection-copy-prompt')!)$('connection-copy-prompt')!.disabled=true;
  document.body.dataset.loading='true';
  $('status')!.hidden=false;
  $('status')!.textContent=t('Caricamento…');

  for(const id of ['candidate-search','candidate-category','article-search','creator-search','article-category','article-macro'] as const)if($(id)!)$(id)!.value='';
  const summary=await readSummary(entry,signal);
  if(version!==loadVersion)return;
  precomputed=summary;
  let next: Dataset={metadata:summary.metadata,pages:[],link_candidates:{}};
  if(connectionsPage) {
    const language=entry.kind==='aggregate'?new URLSearchParams(window.location.search).get('language'):entry.language;
    if(!language||(entry.kind==='aggregate'&&!summary.metadata.languages?.includes(language)))throw new Error('Codice lingua non valido');
    const member=entry.kind==='aggregate'?datasetEntries.find(e=>e.language===language&&e.kind!=='aggregate'):entry;
    if(!member)throw new Error('Raccolta richiesta non disponibile');
    next=await readConnections(member,signal);
    if(version!==loadVersion)return;
    if(entry.kind==='aggregate') {
      next.pages.forEach(page=>{page.language=language;});
      next={...next,metadata:summary.metadata,link_candidates:Object.fromEntries(Object.entries(next.link_candidates!).map(([id,record])=>[language+':'+id,record]))};
    }
  }
  if(!next)return;
  data=next; datasetFile=entry.file; connectionLimit=50;
  datasetLinks(entry);
  if(deorphanizePage||connectionsPage) {
    resetReviews();
    await loadSharedReviews();
    if(version!==loadVersion)return;
  }
  refreshCategoryChoices();
  if($('profile-apply'))$('profile-apply')!.disabled=false;
  lastError=null;
  document.body.dataset.loading='false';
  renderDataset();renderAuth();
  if(!connectionsPage)await explorer.hydration;
  if(deorphanizePage)await cardHydration;
  if(version!==loadVersion)return;
  if(deorphanizePage&&profileContributions){const profileVersion=++profileRequestVersion;void scoreProfile(profileContributions,profileVersion).catch(error=>{
    if(profileVersion===profileRequestVersion)$('profile-status')!.textContent=t('Personalizzazione non disponibile: {error}',{error:String(error)});
  });}
}
function refreshCategoryChoices() {
  for(const prefix of ['candidate','article'])if($(prefix+'-categories')!) {
    $(prefix+'-categories')!.replaceChildren();
    const mode=prefix==='article'?$('article-classification')?.value||'categories':'categories';
    const names=mode==='macro'?Object.keys(topicTaxonomy):mode==='topics'?[...topicDisplayNames.values()]:
      [...new Set(precomputed.reports.categories.map(row=>row.label))].sort();
    for(const name of names)$(prefix+'-categories')!.append(new Option(name,name));
    if(prefix==='article')for(const id of ['article-category-label','article-category-heading'])if($(id)!)$(id)!.textContent=t(mode==='macro'?'Macrotema':mode==='topics'?'Tema':'Categoria');
    if(prefix==='article') {
      $('article-category-control')!.hidden=mode==='macro';
      $('article-macro-control')!.hidden=mode!=='macro';
      const selected=$('article-macro')!.value;
      $('article-macro')!.replaceChildren(new Option(t('Tutti i macrotemi'),''),...Object.keys(topicTaxonomy).map(name=>new Option(name,name)));
      $('article-macro')!.value=Object.hasOwn(topicTaxonomy,selected)?selected:'';
      $('article-category')!.placeholder=t(mode==='topics'?'Tema':'Categoria');
    }
  }
}
function renderDataset() {
  if(document.body.dataset.loading==='true')return;
  if(deorphanizePage&&$('dataset')!.value==='')return;
  if($('download'))$('download')!.textContent=t('Scarica tutti i dati CSV');
  if($('dataset')) for(const option of $('dataset')!.children)if((option as HTMLOptionElement).value==='all')option.textContent=t('Tutte le lingue');
  if($('dataset-date'))$('dataset-date')!.children[0].textContent=t('Ultima disponibile');
  if($('creator-sort'))for(const child of $('creator-sort')!.children) {
    const option=child as HTMLOptionElement;
    if(option.value==='articles-desc')option.textContent=t(creatorMode()==='history'?'Più voci attribuite':'Più voci create');
    if(option.value==='articles-asc')option.textContent=t(creatorMode()==='history'?'Meno voci attribuite':'Meno voci create');
  }
  $('status')!.className=data.metadata.demo?'demo':'';
  $('status')!.textContent=data.metadata.demo?t('DEMO · Dati sintetici per verificare il sistema. Questi non sono risultati di Wikipedia.'):'';
  $('status')!.hidden=!data.metadata.demo;
  if(connectionsPage) {renderConnections();return;}
  if(deorphanizePage) {
    candidateCards();return;
  }
  const report=precomputed.reports.overview;
  $('metrics')!.replaceChildren();
  const creatorAvailable=creatorsAvailable();
  const metrics=creatorsPage ? [
    ['Pagine orfane',number(report.orphans)],
    [creatorMode()==='history'?'Orfane con creatore storico ricostruito':'Orfane con creatore attribuito',creatorAvailable?`${number(report.attributed_orphans)} / ${number(report.orphans)}`:'—'],
    [creatorMode()==='history'?'Creatori storici distinti delle orfane':'Creatori distinti delle orfane attribuite',creatorAvailable?number(report.distinct_creators):'—'],
    ['Orfane senza creatore attribuito',number(report.missing_creators)],
  ] : [['Voci analizzate',number(report.total)],['Pagine orfane',number(report.orphans)],['Percentuale orfane',report.percent===null?'—':number(report.percent)+'%'],['Senza link uscenti',number(report.dead_ends)]];
  for(const [name,value] of metrics) {
    const card=element('div',t(name),'metric');card.append(element('strong',value));$('metrics')!.append(card);
  }
  renderCharts();articleList();creatorList();
}
function renderCharts() {
  if(creatorsPage) {creatorCharts();historyCharts();}
  else {
    categories();
    paperProfiles();
    distribution('age','age_days','anni');
    distribution('length','length_bytes','byte');
  }
}
function populateLlmProviders() {
  const select=$('llm-provider');if(!select)return;
  select.replaceChildren(...Object.entries(llmProviders).map(([key,provider])=>new Option(t(provider.name),key)));
  select.value=llmProvider;
}
let lastError: Error | null = null;
function fail(cause: unknown) {const error=cause instanceof Error?cause:new Error(String(cause));lastError=error;$('status')!.hidden=false;$('status')!.textContent=t('Impossibile caricare: {error}. Esegui la pipeline dei dump e servi la cartella web tramite HTTP.',{error:t(error.message)});}
applyTheme();applyTranslations();
$('ui-language-toggle')!.addEventListener('click',()=>{
  closePrompt();
  setUILanguage(uiLanguage==='it'?'en':'it');savePreference('orphanwiki-language',uiLanguage);applyTranslations();
  renderAuth();populateLlmProviders();
  if(data) {refreshCategoryChoices();renderDataset();}
  if(lastError)fail(lastError);
});
$('theme-toggle')!.addEventListener('click',()=>{
  theme=document.documentElement.dataset.theme==='light'?'dark':'light';savePreference('orphanwiki-theme',theme);applyTheme();
});
systemTheme?.addEventListener?.('change',()=>{if(theme==='system')applyTheme();});
if($('llm-provider')!) {
  populateLlmProviders();
  $('llm-provider')!.addEventListener('change',()=>{
    const value=$('llm-provider')!.value;if(!Object.hasOwn(llmProviders,value)){$('llm-provider')!.value=llmProvider;return;}
    closePrompt();
    llmProvider=value;savePreference('orphanwiki-llm',value);
    if(data) {if(deorphanizePage)candidateCards();if(connectionsPage)renderConnections();}
  });
}
$('llm-prompt-copy')?.addEventListener('click',()=>copyPrompt());
$('llm-prompt-debug')?.addEventListener('change',updatePreparedPrompt);
$('connection-copy-prompt')?.addEventListener('click',()=>{void copyConnectionPrompt().catch(fail);});
$('llm-prompt-close')?.addEventListener('click',closePrompt);
$('llm-prompt-finished')?.addEventListener('click',()=>{if(activePromptReview)void showReview(activePromptReview);});
$('review-close')?.addEventListener('click',closeReview);
$('review-dialog')?.addEventListener('cancel',event=>{event.preventDefault();closeReview();});
$('llm-prompt')?.addEventListener('cancel',event=>{event.preventDefault();closePrompt();});
$('chart-type')?.addEventListener('change',()=>{if(data&&document.body.dataset.loading!=='true')renderCharts();});
$('category-metric')?.addEventListener('change',()=>{if(data)categories();});
for(const id of ['topics-full-metric','topics-paper-metric'])$(id)?.addEventListener('change',()=>{if(data)paperProfiles();});
for(const id of ['candidate-search','candidate-category','candidate-sort']) $(id)?.addEventListener('input',()=>{if(data)candidateCards();});
if(deorphanizePage||connectionsPage) {
  const button=$('candidate-hidden')!,options=$('candidate-hidden-options')!;
  const open=(visible: boolean)=>{
    options.hidden=!visible;button.setAttribute('aria-expanded',String(visible));
    const anchor=button.getBoundingClientRect?.(),origin=options.offsetParent?.getBoundingClientRect();
    if(!visible||!anchor||!origin)return;
    const above=anchor.top-12,below=window.innerHeight-anchor.bottom-12;
    const upwards=below<320&&above>below;
    options.style.maxHeight=Math.max(120,Math.min(620,upwards?above:below))+'px';
    options.style.top=upwards?'auto':(anchor.bottom-origin.top+6)+'px';
    options.style.bottom=upwards?(origin.bottom-anchor.top+6)+'px':'auto';
    const width=options.getBoundingClientRect().width;
    options.style.left=(Math.max(12,Math.min(anchor.right-width,window.innerWidth-width-12))-origin.left)+'px';
    options.style.right='auto';
  };
  open(false);
  button.addEventListener('click',()=>open(Boolean(options.hidden)));
  $('candidate-hidden-control')!.addEventListener('keydown',event=>{
    if(event.key==='Escape'&&!options.hidden){event.preventDefault();open(false);button.focus();}
  });
}
for(const id of ['candidate-show-disambiguation','candidate-show-redirects','candidate-show-without-connections','candidate-show-names','candidate-show-indexes','candidate-show-maintenance'] as const)$(id)?.addEventListener('change',()=>{
  if(data)candidateCards();
});
for(const id of linkFilterIds) {
  const node=$(id);if(!node)continue;
  node.checked=readPreference('orphanwiki-'+id,'show',['show','hide'])==='hide';
  node.addEventListener('change',()=>{
    savePreference('orphanwiki-'+id,node.checked?'hide':'show');
    closePrompt();connectionLimit=50;
    if(data){if(deorphanizePage)candidateCards();else if(connectionsPage)renderConnections();}
  });
}
$('auth-menu')?.addEventListener('keydown',event=>{
  if(event.key==='Escape') {event.preventDefault();$('auth-menu')!.open=false;$('auth-toggle')!.focus();}
});
$('auth-login')?.addEventListener('click',()=>{void login();});
$('auth-logout')?.addEventListener('click',()=>{void logout();});
$('profile-apply')?.addEventListener('click',()=>{if(data)void applyProfile().catch(fail);});
for(const id of ['article-search','article-category','article-group','article-sort']) $(id)?.addEventListener('input',()=>{if(data)articleList();});
$('article-macro')?.addEventListener('change',()=>{if(data)articleList();});
for(const id of ['creator-search','creator-sort']) $(id)?.addEventListener('input',()=>{if(data)creatorList();});
$('article-classification')?.addEventListener('change',()=>{if(data){$('article-category')!.value='';$('article-macro')!.value='';refreshCategoryChoices();articleList();}});
$('article-more')?.addEventListener('click',()=>stepListPage('articles',1));
$('creator-more')?.addEventListener('click',()=>stepListPage('creators',1));
$('article-previous')?.addEventListener('click',()=>stepListPage('articles',-1));
$('creator-previous')?.addEventListener('click',()=>stepListPage('creators',-1));
$('candidate-previous')?.addEventListener('click',()=>stepListPage('cards',-1));
$('connection-more')?.addEventListener('click',()=>{connectionLimit+=50;renderConnections();});
$('candidate-more')?.addEventListener('click',()=>stepListPage('cards',1));
const explorer=createExplorer({get data(){return data;},get datasetFile(){return datasetFile;},get loadVersion(){return loadVersion;},
 get precomputed(){return precomputed;},pageKey,hiddenTargetCategories,creatorListInfo,
 setCreators(rows){creatorRows=rows;},abortCards(){++cardRequestVersion;cardController.abort();},
 render(kind){(kind==='articles'?articleList:kind==='creators'?creatorList:candidateCards)();},fail});
const {resetRemoteList,databaseList,stepListPage}=explorer;
configureProfile({get datasetEntries(){return datasetEntries;},get topicDisplayNames(){return topicDisplayNames;}});
configureEditorial({
 get data(){return data;}, get datasetFile(){return datasetFile;}, get loadVersion(){return loadVersion;},
 get authEnabled(){return authEnabled;}, get authUsername(){return authUsername;},
 get llmProvider(){return llmProvider;}, get llmProviders(){return llmProviders;},
 pageLanguage,pageKey,hiddenSuggestedLink,userPage,validUsername,loadAuth,renderDataset,wikimediaAPI,fail
});
export const dashboardReady=(async()=>{
  await loadAuth();
  const bindings: DashboardModule = await import(nativeModuleUrl);
  await bindings.default();setCalculator(bindings.calculate);
  for(const label of calculateReport<string[]>('taxonomy',null)) {
    const separator=label.indexOf('.'),parent=label.slice(0,separator),child=label.slice(separator+1);
    (topicTaxonomy[parent]??=[]).push(child);topicDisplayNames.set(label,parent+' · '+child);
  }
  const response=await fetch('data/manifest.json',{cache:'no-store'});if(!response.ok)throw new Error('Nessuna raccolta presente');
  const entries=validateManifest(await response.json()).filter(entry=>!entry.demo).sort((a,b)=>b.date.localeCompare(a.date)||
    (b.published_at??'').localeCompare(a.published_at??'')||b.file.localeCompare(a.file));if(!entries.length)throw new Error('Nessuna raccolta reale presente');
  datasetEntries=entries;
  for(const entry of entries) {
    if(!/^[a-zA-Z0-9_.-]+\.json$/.test(entry.file))throw new Error('Nome dataset non valido');
  }
  const requested=new URLSearchParams(window.location.search).get('dataset');
  if(connectionsPage) {
    const entry=entries.find(entry=>entry.file===requested);
    if(!entry)throw new Error('Raccolta richiesta non disponibile');
    await load(entry);return;
  }
  const languages=[...new Set(entries.map(entry=>entry.language))].sort();
  if(deorphanizePage)$('dataset')!.append(new Option(t('Seleziona'),''));
  if(languages.includes('all'))$('dataset')!.append(new Option(t('Tutte le lingue'),'all'));
  for(const language of languages)if(language!=='all')$('dataset')!.append(new Option(language.toUpperCase(),language));
  const matching=entries.find(entry=>entry.file===requested);
  if(requested&&!matching)throw new Error('Raccolta richiesta non disponibile');
  $('dataset')!.value=deorphanizePage&&(!matching||matching.language==='all')?'':matching?.language??(languages.includes('all')?'all':languages[0]);
  refreshDateChoices(matching?.date.slice(0,10));
  $('dataset')!.addEventListener('change',async()=>{
    const previous=$('dataset-date')!.value;
    refreshDateChoices(previous);
    if(deorphanizePage&&$('dataset')!.value===''){clearLanguageSelection();return;}
    const entry=selectedEntry();if(entry)await load(entry).catch(fail);
  });
  $('dataset-date')!.addEventListener('change',async()=>{
    const entry=selectedEntry();if(entry)await load(entry).catch(fail);
  });
  if(deorphanizePage&&$('dataset')!.value==='') {
    clearLanguageSelection();return;
  }
  const initial=matching??selectedEntry();
  if(!initial)throw new Error('Nessuna raccolta reale presente');
  await load(initial);
})().catch(fail);

let chartResizeTimer: ReturnType<typeof setTimeout> | undefined;
window.addEventListener('resize',()=>{
  clearTimeout(chartResizeTimer);
  chartResizeTimer=setTimeout(()=>{
    if(data&&!connectionsPage&&!deorphanizePage) renderCharts();
  },150);
});
