import type {Dataset,DashboardSummaryData,PageObservation,CreatorRow,CandidateRecord,CardTransport,RecentEdit,UIElements} from './types.js';
import {$,element} from './dom.js';
import {t,number,uiLanguage} from './i18n.js';
import {canonicalJSON,validateDataset} from './datasets.js';
import {isRecord} from './validation.js';
import {editorialReviews} from './editorial.js';
export type RemoteKind='articles'|'cards'|'creators';
const listPageSize=25;
interface RemoteList {
  key: string; offset: number; total: number; pages: PageObservation[]; creators: CreatorRow[];
  records: Record<string,CandidateRecord>; transports: Record<string,CardTransport>;
}

interface ExplorerContext {
 readonly data: Dataset; readonly datasetFile: string; readonly loadVersion: number; readonly precomputed: DashboardSummaryData;
 pageKey: (page: PageObservation) => string; hiddenTargetCategories: () => string[];
 creatorListInfo: (total:number,shown:number) => string; setCreators: (rows: CreatorRow[]) => void;
 abortCards: () => void; render: (kind: RemoteKind) => void; fail: (error: unknown) => void;
}
export function createExplorer(context: ExplorerContext) {
let remoteRendering=false,remoteController=new AbortController(),remoteVersion=0;
let remoteList: RemoteList | null=null,listHydration: Promise<void>=Promise.resolve();
let remoteQueryKey='',remoteOffset=0,paginationFocus=false;
let remoteProfileEdits: RecentEdit[]=[];
function resetRemoteList() {
  ++remoteVersion;remoteController.abort();remoteList=null;remoteProfileEdits=[];
  remoteQueryKey='';remoteOffset=0;paginationFocus=false;
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
  } else if(remoteList)changeListPage(kind,remoteList.offset+direction*listPageSize,true);
}
function listPagination(kind: RemoteKind,list: {offset: number; total: number},busy=false) {
  const prefix=kind==='cards'?'candidate':kind==='creators'?'creator':'article';
  const previous=$(prefix+'-previous') as HTMLButtonElement,more=$(prefix+'-more') as HTMLButtonElement,numbers=$(prefix+'-pages')!;
  const current=Math.floor(list.offset/listPageSize)+1,total=Math.ceil(list.total/listPageSize);
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
    button.addEventListener('click',()=>changeListPage(kind,(page-1)*listPageSize,true));
    numbers.append(button);last=page;
    if(page===current&&paginationFocus&&!busy){button.focus();paginationFocus=false;}
  }
}
function listRange(total: number,offset: number,shown: number): string {
  return total?' · '+t('Risultati {from}–{to} · Pagina {page} di {pages}',
    {from:number(offset+1),to:number(offset+shown),page:number(offset/listPageSize+1),pages:number(Math.ceil(total/listPageSize))}):'';
}

function databaseList(kind: RemoteKind,render: ()=>void): boolean {
  if(remoteRendering)return false;
  listHydration=loadDatabaseList(kind,render);void listHydration.catch(context.fail);return true;
}
async function loadDatabaseList(kind: RemoteKind,render: ()=>void): Promise<void> {
  const prefix=kind==='cards'?'candidate':kind==='creators'?'creator':'article';
  const field=(id: keyof UIElements)=>{const node=$(id);return node&&'value' in node?String(node.value):'';};
  const request={dataset:context.datasetFile,kind,search:field((prefix+'-search') as keyof UIElements),
    category:kind==='articles'&&field('article-classification')==='macro'?field('article-macro'):kind==='creators'?'':field((prefix+'-category') as keyof UIElements),
    classification:kind==='articles'?field('article-classification')||'categories':'categories',
    group:kind==='articles'?field('article-group')||'orphans':'orphans',
    order:field((prefix+'-sort') as keyof UIElements)||(kind==='cards'?'candidates-desc':kind==='creators'?'orphans-desc':'title-asc'),
    show_disambiguation:kind==='cards'&&Boolean($('candidate-show-disambiguation')?.checked),
    show_redirects:kind==='cards'&&Boolean($('candidate-show-redirects')?.checked),
    show_without_connections:kind==='cards'&&Boolean($('candidate-show-without-connections')?.checked),
    hidden_categories:kind==='cards'?context.hiddenTargetCategories():[],
    missing_category:t('Senza categoria idonea'),missing_topics:t('Temi non disponibili'),empty_topics:t('Nessun tema sopra soglia'),
    closed_pairs:kind==='cards'?[...editorialReviews.values()].filter(review=>review.status==='fixed'||review.status==='bad_link').map(review=>[review.language,review.target_id,review.source_id]):[],
    edits:kind==='cards'&&field('candidate-sort')==='personalized'?remoteProfileEdits:[]};
  const queryKey=canonicalJSON([{...request,missing_category:'',missing_topics:'',empty_topics:''},kind==='cards'?[...editorialReviews.values()]:[]]);
  const key=canonicalJSON([queryKey,uiLanguage]);
  if(queryKey!==remoteQueryKey){remoteQueryKey=queryKey;remoteOffset=0;paginationFocus=false;}
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
    info.textContent+=listRange(list.total,list.offset,shown);
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
    const response=await fetch('api/pages',{method:'POST',cache:'no-store',credentials:'omit',signal,
      headers:{'Content-Type':'application/json'},body:JSON.stringify({...request,offset})});
    const payload: unknown=await response.json();
    if(signal.aborted||version!==remoteVersion||load!==context.loadVersion)return;
    if(!response.ok||!isRecord(payload)||payload.dataset!==context.datasetFile||payload.offset!==offset||
      !Number.isSafeInteger(payload.total)||Number(payload.total)<0)throw new Error('Indice degli articoli non valido');
    const next: RemoteList={key,offset,total:0,pages:[],creators:[],records:{},transports:{}};
    next.total=Number(payload.total);
    if(kind==='creators') {
      if(payload.format!=='creator_results_v1'||!Array.isArray(payload.creators)||payload.creators.length>listPageSize||
        payload.creators.some(row=>!isRecord(row)||typeof row.key!=='string'||!Number.isSafeInteger(row.total)||!Number.isSafeInteger(row.orphans)||!Array.isArray(row.languages)))throw new Error('Indice dei creatori non valido');
      next.creators.push(...payload.creators as CreatorRow[]);
    } else {
      if(payload.format!=='page_results_v1'||!Array.isArray(payload.pages)||payload.pages.length>listPageSize||
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
    }
    const loaded=kind==='creators'?next.creators.length:next.pages.length;
    if(loaded!==Math.min(listPageSize,Math.max(0,next.total-offset)))throw new Error('Indice degli articoli non valido');
    if(offset>0&&offset>=next.total) {
      remoteOffset=Math.max(0,Math.floor((next.total-1)/listPageSize)*listPageSize);
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

return {resetRemoteList,databaseList,stepListPage,get hydration(){return listHydration;},
 setProfileEdits(edits: RecentEdit[]){remoteProfileEdits=edits;}};
}
