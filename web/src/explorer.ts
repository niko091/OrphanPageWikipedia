import type {Dataset,DashboardSummaryData,PageObservation,CreatorRow,CandidateRecord,CardTransport,RecentEdit,UIElements} from './types.js';
import {$,element} from './dom.js';
import {t,number,uiLanguage} from './i18n.js';
import {canonicalJSON,validateDataset} from './datasets.js';
import {isRecord} from './validation.js';
import {editorialReviews} from './editorial.js';
export type RemoteKind='articles'|'cards'|'creators';
export const DEFAULT_CARD_ORDER='length-desc';
const listPageSize=(kind: RemoteKind)=>kind==='cards'?16:25;
export interface VoteReceipt {context: string; signature: number[]}
export interface CardVote {
  stars: number | null;
  receipt: VoteReceipt;
  pendingStars?: number | null;
  saving?: Promise<void>;
}
export function readVoteReceipt(value: unknown): VoteReceipt | null {
  if(!isRecord(value)||typeof value.context!=='string'||!Array.isArray(value.signature)||value.signature.length!==32)return null;
  const signature=value.signature.filter((byte: unknown): byte is number=>
    typeof byte==='number'&&Number.isInteger(byte)&&byte>=0&&byte<=255);
  return signature.length===32?{context:value.context,signature}:null;
}
export const validStars=(value: unknown): value is number=>
  typeof value==='number'&&Number.isInteger(value)&&value>=1&&value<=5;
export async function submitCardVote(receipt: VoteReceipt,stars: number | null): Promise<void> {
  const response=await fetch('/api/orphan-votes',{method:'POST',cache:'no-store',credentials:'same-origin',
    headers:{'Content-Type':'application/json'},signal:AbortSignal.timeout(8000),body:JSON.stringify({stars,receipt})});
  if(response.status===401)throw new Error('Accedi con Wikimedia per votare.');
  if(!response.ok)throw new Error('Impossibile salvare il voto. Riprova.');
  const result: unknown=await response.json();
  if(!isRecord(result)||result.stars!==stars)throw new Error('Impossibile salvare il voto. Riprova.');
}
interface RemoteList {
  key: string; offset: number; total: number; pages: PageObservation[]; creators: CreatorRow[];
  records: Record<string,CandidateRecord>; transports: Record<string,CardTransport>;
  votes: Map<string,CardVote>;
}

interface ExplorerContext {
 readonly data: Dataset; readonly datasetFile: string; readonly loadVersion: number; readonly precomputed: DashboardSummaryData;
 readonly authUsername: string | null;
 pageKey: (page: PageObservation) => string; hiddenTargetCategories: () => string[];
 creatorListInfo: (total:number,shown:number) => string; setCreators: (rows: CreatorRow[]) => void;
 abortCards: () => void; render: (kind: RemoteKind) => void; fail: (error: unknown) => void;
}
export function createExplorer(context: ExplorerContext) {
let remoteRendering=false,remoteController=new AbortController(),remoteVersion=0;
let remoteList: RemoteList | null=null,listHydration: Promise<void>=Promise.resolve();
let remoteQueryKey='',remoteOffset=0,paginationFocus=false;
let returningCardOffset=0;
let remoteProfileEdits: RecentEdit[]=[];
const pendingVotes=new Set<Promise<void>>();

function saveCardVote(key: string,vote: CardVote,stars: number | null): Promise<void> | undefined {
  if(remoteList?.votes.get(key)!==vote||!context.authUsername)return;
  vote.pendingStars=stars;
  if(vote.saving)return vote.saving;
  const saving=(async()=>{
    let next=stars;
    for(;;) {
      await submitCardVote(vote.receipt,next);
      vote.stars=next;
      const wanted=vote.pendingStars;
      if(wanted===undefined||wanted===next)break;
      next=wanted;
    }
  })();
  vote.saving=saving;
  pendingVotes.add(saving);
  const finish=()=>{pendingVotes.delete(saving);delete vote.saving;delete vote.pendingStars;};
  void saving.then(finish,finish);
  return saving;
}
function resetRemoteList() {
  ++remoteVersion;remoteController.abort();remoteList=null;remoteProfileEdits=[];
  remoteQueryKey='';remoteOffset=0;paginationFocus=false;
  returningCardOffset=0;
  for(const prefix of ['article','creator','candidate'] as const) {
    const previous=$(prefix+'-previous'),pages=$(prefix+'-pages'),more=$(prefix+'-more') as HTMLButtonElement | null;
    if(previous)previous.hidden=true;
    if(pages){pages.hidden=true;pages.replaceChildren();}
    if(more) {
      more.dataset.i18n='Successiva';
      more.textContent=t(more.dataset.i18n);more.disabled=false;
    }
  }
}
function changeListPage(kind: RemoteKind,offset: number,focusPage=false) {
  if(!remoteList||offset<0||offset>=remoteList.total||offset===remoteList.offset)return;
  remoteOffset=offset;paginationFocus=focusPage;
  context.render(kind);
}
function stepListPage(kind: RemoteKind,direction: number) {
  if(direction>0&&remoteOffset!==remoteList?.offset) {
    (()=>context.render(kind))();
  } else if(remoteList)changeListPage(kind,remoteList.offset+direction*listPageSize(kind),true);
}
function listPagination(kind: RemoteKind,list: {offset: number; total: number},busy=false) {
  const pageSize=listPageSize(kind);
  const prefix=kind==='cards'?'candidate':kind==='creators'?'creator':'article';
  const previous=$(prefix+'-previous') as HTMLButtonElement,more=$(prefix+'-more') as HTMLButtonElement,numbers=$(prefix+'-pages')!;
  const current=Math.floor(list.offset/pageSize)+1,total=Math.ceil(list.total/pageSize);
  previous.hidden=more.hidden=numbers.hidden=total<=1;
  previous.disabled=busy||list.offset===0;more.disabled=busy||current>=total;
  more.dataset.i18n='Successiva';more.textContent=t('Successiva');
  numbers.replaceChildren();
  // Bound the controls even for a complete edition with millions of rows.
  const start=Math.max(1,Math.min(current-2,total-4)),end=Math.min(total,start+4);
  const pages=[...new Set([1,...Array.from({length:Math.max(0,end-start+1)},(_,i)=>start+i),total])].filter(page=>page>0);
  let last=0;
  for(const page of pages) {
    if(last&&page-last>1)numbers.append(element('span','…'));
    const button=element('button',number(page));button.type='button';button.disabled=busy;
    button.setAttribute('aria-label',t('Pagina {page}',{page:number(page)}));
    if(page===current)button.setAttribute('aria-current','page');
    button.addEventListener('click',()=>changeListPage(kind,(page-1)*pageSize,true));
    numbers.append(button);last=page;
    if(page===current&&paginationFocus&&!busy){button.focus();paginationFocus=false;}
  }
}
function listRange(kind: RemoteKind,total: number,offset: number,shown: number): string {
  return total?' · '+t('Risultati {from}–{to} · Pagina {page} di {pages}',
    {from:number(offset+1),to:number(offset+shown),page:number(offset/listPageSize(kind)+1),pages:number(Math.ceil(total/listPageSize(kind)))}):'';
}

function databaseList(kind: RemoteKind,render: ()=>void): boolean {
  if(remoteRendering)return false;
  listHydration=loadDatabaseList(kind,render);void listHydration.catch(context.fail);return true;
}
async function loadDatabaseList(kind: RemoteKind,render: ()=>void): Promise<void> {
  const pageSize=listPageSize(kind);
  const prefix=kind==='cards'?'candidate':kind==='creators'?'creator':'article';
  const field=(id: keyof UIElements)=>{const node=$(id);return node&&'value' in node?String(node.value):'';};
  const request={dataset:context.datasetFile,kind,search:field((prefix+'-search') as keyof UIElements),
    category:kind==='articles'&&field('article-classification')==='macro'?field('article-macro'):kind==='creators'?'':field((prefix+'-category') as keyof UIElements),
    classification:kind==='articles'?field('article-classification')||'categories':'categories',
    group:kind==='articles'?field('article-group')||'orphans':'orphans',
    order:field((prefix+'-sort') as keyof UIElements)||(kind==='cards'?DEFAULT_CARD_ORDER:kind==='creators'?'orphans-desc':'title-asc'),
    show_disambiguation:kind==='cards'&&Boolean($('candidate-show-disambiguation')?.checked),
    show_redirects:kind==='cards'&&Boolean($('candidate-show-redirects')?.checked),
    show_without_connections:kind==='cards'&&Boolean($('candidate-show-without-connections')?.checked),
    hidden_categories:kind==='cards'?context.hiddenTargetCategories():[],
    missing_category:t('Senza categoria idonea'),missing_topics:t('Temi non disponibili'),empty_topics:t('Nessun tema sopra soglia'),
    closed_pairs:kind==='cards'?[...editorialReviews.values()].filter(review=>review.status==='fixed'||review.status==='bad_link').map(review=>[review.language,review.target_id,review.source_id]):[],
    edits:kind==='cards'&&field('candidate-sort')==='personalized'?remoteProfileEdits:[]};
  const queryKey=canonicalJSON([{...request,missing_category:'',missing_topics:'',empty_topics:''},kind==='cards'?[...editorialReviews.values()]:[]]);
  const key=canonicalJSON([queryKey,uiLanguage,kind==='cards'?context.authUsername:null]);
  if(queryKey!==remoteQueryKey) {
    remoteQueryKey=queryKey;
    remoteOffset=kind==='cards'?returningCardOffset:0;
    returningCardOffset=0;paginationFocus=false;
  }
  const offset=remoteOffset;
  const more=$(prefix+'-more' as keyof UIElements) as HTMLButtonElement;
  const info=$(prefix+'-info' as keyof UIElements)!;
  function display(list: RemoteList) {
    context.data.pages=list.pages;context.data.link_candidates=list.records;context.data.card_transports=list.transports;
    if(kind==='creators')context.setCreators(list.creators);
    remoteRendering=true;
    try {render();} finally {remoteRendering=false;}
    const shown=kind==='creators'?list.creators.length:list.pages.length;
    if(kind!=='creators')info.textContent=t(kind==='cards'?'{n} orfane corrispondenti · {shown} schede visualizzate':'{n} voci · {shown} visualizzate',{n:number(list.total),shown:number(shown)});
    else info.textContent=context.creatorListInfo(list.total,shown);
    info.textContent+=listRange(kind,list.total,list.offset,shown);
    listPagination(kind,list);
  }
  if(remoteList?.key===key&&remoteList.offset===offset) {
    ++remoteVersion;remoteController.abort();display(remoteList);return;
  }
  const version=++remoteVersion,load=context.loadVersion;
  remoteController.abort();remoteController=new AbortController();
  const signal=remoteController.signal;
  context.abortCards();
  if(remoteList)listPagination(kind,remoteList,true);
  more.disabled=true;info.textContent=t('Caricamento…');
  try {
    if(kind==='cards'&&pendingVotes.size) {
      await Promise.allSettled(pendingVotes);
      signal.throwIfAborted();
    }
    const response=await fetch('/api/pages',{method:'POST',cache:'no-store',credentials:kind==='cards'?'same-origin':'omit',signal,
      headers:{'Content-Type':'application/json'},body:JSON.stringify({...request,offset})});
    const payload: unknown=await response.json();
    if(signal.aborted||version!==remoteVersion||load!==context.loadVersion)return;
    if(!response.ok||!isRecord(payload)||payload.dataset!==context.datasetFile||payload.offset!==offset||
      !Number.isSafeInteger(payload.total)||Number(payload.total)<0)throw new Error('Indice degli articoli non valido');
    const next: RemoteList={key,offset,total:0,pages:[],creators:[],records:{},transports:{},votes:new Map()};
    next.total=Number(payload.total);
    if(kind==='creators') {
      if(payload.format!=='creator_results_v1'||!Array.isArray(payload.creators)||payload.creators.length>pageSize||
        payload.creators.some(row=>!isRecord(row)||typeof row.key!=='string'||!Number.isSafeInteger(row.total)||!Number.isSafeInteger(row.orphans)||!Array.isArray(row.languages)))throw new Error('Indice dei creatori non valido');
      next.creators.push(...payload.creators as CreatorRow[]);
    } else {
      if(payload.format!=='page_results_v1'||!Array.isArray(payload.pages)||payload.pages.length>pageSize||
        canonicalJSON(payload.metadata)!==canonicalJSON(context.data.metadata)||payload.census_page_count!==context.precomputed?.census_page_count||
        !isRecord(payload.link_candidates)||!isRecord(payload.card_transports))throw new Error('Indice degli articoli non valido');
      if(payload.pages.some(page=>!isRecord(page)||!Number.isSafeInteger(page.page_id)||Number(page.page_id)<=0||typeof page.title!=='string'||typeof page.orphan!=='boolean'))throw new Error('Indice degli articoli non valido');
      const member={metadata:context.data.metadata,pages:payload.pages,link_candidates:payload.link_candidates} as Dataset;
      if(context.data.metadata.kind==='aggregate') {
        for(const page of member.pages) {
          if(!context.data.metadata.languages?.includes(page.language??''))throw new Error('Indice degli articoli non valido');
          validateDataset({metadata:{...member.metadata,language:page.language!},pages:[page]});
        }
      } else validateDataset(member);
      next.pages.push(...member.pages);Object.assign(next.records,member.link_candidates);Object.assign(next.transports,payload.card_transports);
      if(new Set(next.pages.map(context.pageKey)).size!==next.pages.length)throw new Error('Indice degli articoli non valido');
      const voting=payload.orphan_voting;
      if(kind==='cards'&&isRecord(voting)) {
        for(const page of next.pages) {
          const key=context.pageKey(page),vote=voting[key];
          if(isRecord(vote)&&(vote.stars===null||validStars(vote.stars))) {
            const receipt=readVoteReceipt(vote.receipt);
            if(receipt)next.votes.set(key,{stars:context.authUsername?vote.stars:null,receipt});
          }
        }
      }
    }
    const loaded=kind==='creators'?next.creators.length:next.pages.length;
    if(loaded!==Math.min(pageSize,Math.max(0,next.total-offset)))throw new Error('Indice degli articoli non valido');
    if(offset>0&&offset>=next.total) {
      remoteOffset=Math.max(0,Math.floor((next.total-1)/pageSize)*pageSize);
      await loadDatabaseList(kind,render);return;
    }
    remoteList=next;display(next);
  } catch(error) {
    if(!signal.aborted&&version===remoteVersion&&load===context.loadVersion) {
      remoteList=null;
      $(kind==='cards'?'candidate-cards':kind==='creators'?'creator-rows':'article-rows')!.replaceChildren();
      throw error;
    }
  }
}

return {resetRemoteList,databaseList,stepListPage,saveCardVote,get hydration(){return listHydration;},
 get cardOffset(){return remoteList?.offset??0;},
 restoreCardOffset(offset: number){
   if(Number.isSafeInteger(offset)&&offset>=0&&offset%listPageSize('cards')===0)returningCardOffset=offset;
 },
 cardVote(key: string){return remoteList?.votes.get(key);},
 setProfileEdits(edits: RecentEdit[]){remoteProfileEdits=edits;}};
}
