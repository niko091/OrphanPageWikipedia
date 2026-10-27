import type {ManifestEntry,DatasetMetadata,GlobalWiki,GlobalUser,RecentEdit,UserContribution} from './types.js';
import {t} from './i18n.js';
import {isRecord} from './validation.js';
import {canonicalJSON} from './datasets.js';
import {wikimediaAPI} from './wikimedia.js';
interface ProfileContext {readonly datasetEntries: ManifestEntry[];readonly topicDisplayNames: Map<string,string>;}
let profileContext: ProfileContext;
export function configureProfile(context: ProfileContext): void {profileContext=context;}
export function clearProfileCache(): void {profileTopicCache.clear();recentProfileCache=null;}
// Only requested page IDs are retained, never full observation files or text.
const profileTopicCache = new Map<string, Map<number, string[] | null>>();
// One submitted profile, held only in this tab; the merge always retains at most 100 edits.
let recentProfileCache: {identity: string; started: number; completed: Set<string>; edits: RecentEdit[]} | null=null;
function attachedWikipedia(value: unknown): {language: string; id: number; url: string} | null {
  if(!value||typeof value!=='object')return null;
  const wiki=value as Partial<GlobalWiki>;
  if(typeof wiki.url!=='string'||typeof wiki.id!=='number'||!Number.isSafeInteger(wiki.id)||wiki.id<=0)return null;
  const match=/^https:\/\/([a-z][a-z0-9-]{0,19})\.wikipedia\.org\/?$/.exec(wiki.url);
  if(!match||typeof wiki.wiki!=='string'||!wiki.wiki.endsWith('wiki'))return null;
  return {language:match[1],id:wiki.id,url:`https://${match[1]}.wikipedia.org`};
}
export async function recentWikipediaEdits(username: string, signal: AbortSignal, progress: (message: string) => void = ()=>{}): Promise<RecentEdit[]> {
  if(!username||username.length>255||/[\u0000-\u001f\u007f#<>|\[\]{}]/.test(username))throw new Error('Nome utente Wikimedia non valido');
  const globalURL=new URL('https://meta.wikimedia.org/w/api.php');
  globalURL.searchParams.set('action','query');globalURL.searchParams.set('meta','globaluserinfo');
  globalURL.searchParams.set('guiuser',username);globalURL.searchParams.set('guiprop','merged');
  const waiting=(host: string)=>(seconds: number)=>progress(t('Attesa di {seconds} s prima di interrogare {host}…',{host,seconds}));
  const payload=await wikimediaAPI(globalURL,signal,waiting(globalURL.hostname)) as {query?: {globaluserinfo?: GlobalUser}};
  const user=payload.query?.globaluserinfo;
  if(!user||user.missing||!Number.isSafeInteger(user.id)||user.id<=0||!Array.isArray(user.merged)||typeof user.name!=='string')
    throw new Error('Identità globale Wikimedia non verificata');
  const attached=new Map<string,{language: string; id: number; url: string}>();
  for(const merged of user.merged){const wiki=attachedWikipedia(merged);if(wiki){if(attached.has(wiki.language))throw new Error('Identità globale Wikimedia ambigua');attached.set(wiki.language,wiki);}}
  if(!attached.size)throw new Error('Nessuna Wikipedia collegata all’account globale');
  const available=new Set(profileContext.datasetEntries.filter(entry=>!entry.demo&&entry.kind!=='aggregate'&&entry.language!=='all').map(entry=>entry.language));
  const wikis=[...attached.values()].filter(wiki=>available.has(wiki.language));
  if(!wikis.length) {
    recentProfileCache=null;profileTopicCache.clear();
    throw new Error('Nessuna delle Wikipedie collegate all’account è presente nelle raccolte disponibili');
  }
  const identity=JSON.stringify([user.id,wikis.map(wiki=>[wiki.language,wiki.id]).sort()]);
  if(!recentProfileCache||recentProfileCache.identity!==identity||Date.now()-recentProfileCache.started>=300_000) {
    profileTopicCache.clear();
    recentProfileCache={identity,started:Date.now(),completed:new Set(),edits:[]};
  }
  const cached=recentProfileCache,edits=cached.edits;
  const status=()=>t('Wikipedie analizzate verificate: {done}/{total}.',{done:cached.completed.size,total:wikis.length});
  const pending=wikis.filter(wiki=>!cached.completed.has(wiki.language));
  const failures: string[]=[];
  let next=0;
  progress(status());
  async function worker() {
    while(next<pending.length&&failures.length===0) {
      signal.throwIfAborted();
      const wiki=pending[next++],url=new URL(wiki.url+'/w/api.php');
      url.searchParams.set('action','query');url.searchParams.set('list','usercontribs');
      url.searchParams.set('ucuserids',String(wiki.id));url.searchParams.set('uclimit','100');
      url.searchParams.set('ucprop','ids|title|timestamp');url.searchParams.set('ucdir','older');
      // Retries and cached responses refer to the same submitted snapshot.
      url.searchParams.set('ucstart',new Date(cached.started).toISOString());
      try {
        const result=await wikimediaAPI(url,signal,seconds=>progress(status()+' '+t(
          'Attesa di {seconds} s prima di interrogare {host}…',{host:url.hostname,seconds}))) as {query?: {usercontribs?: UserContribution[]}};
        const rows=result.query?.usercontribs;
        const invalid=()=>new Error(t('Modifiche Wikimedia non verificabili: {host}',{host:url.hostname}));
        if(!Array.isArray(rows)||rows.length>100)throw invalid();
        for(const row of rows) {
          if(!isRecord(row)||row.userid!==wiki.id||!Number.isSafeInteger(row.ns)||
            typeof row.timestamp!=='string'||!Number.isFinite(Date.parse(row.timestamp)))throw invalid();
        }
        signal.throwIfAborted();
        for(const row of rows) {
          edits.push({language:wiki.language,page_id:Number.isSafeInteger(row.pageid)&&row.pageid>0?row.pageid:0,
            namespace:row.ns,timestamp:row.timestamp,topic_labels:null});
        }
        // Trimming after each analyzed wiki preserves the platform-wide top 100,
        // regardless of response order, with at most 200 retained edits.
        edits.sort((a,b)=>b.timestamp.localeCompare(a.timestamp)||a.language.localeCompare(b.language)||a.page_id-b.page_id);
        if(edits.length>100)edits.length=100;
        cached.completed.add(wiki.language);progress(status());
      } catch(error) {
        signal.throwIfAborted();
        failures.push(error instanceof Error?error.message:String(error));
      }
    }
  }
  await Promise.all(Array.from({length:Math.min(3,pending.length)},()=>worker()));
  signal.throwIfAborted();
  if(failures.length)throw new Error(status()+' '+failures.join('; ')+'. '+t(
    'Riprova: le risposte verificate restano in memoria per cinque minuti dall’inizio della ricerca.'));
  return edits.slice();
}
export async function readProfileTopics(entry: ManifestEntry, wanted: Set<number>, signal: AbortSignal,
  schedule: (operation: () => Promise<unknown>) => Promise<unknown> = operation=>operation()): Promise<Map<number, string[] | null>> {
  const key=JSON.stringify([entry.file,entry.date,entry.profile_topics_dir]);
  const cached=profileTopicCache.get(key)??new Map<number,string[] | null>();
  const missing=new Set([...wanted].filter(id=>!cached.has(id)));
  if(!missing.size)return cached;
  const found=new Map<number,string[] | null>();
  const validMetadata=(metadata: DatasetMetadata | undefined)=>metadata?.language===entry.language&&
    metadata.finished_at===entry.date&&metadata.complete===true&&Boolean(metadata.demo)===Boolean(entry.demo);
  async function read(filename: string): Promise<unknown> {
    return schedule(async()=>{
      signal.throwIfAborted();
      const response=await fetch('data/'+filename,{cache:'no-store',signal});
      if(!response.ok)throw new Error('Trasporto osservazioni non disponibile');
      return response.json();
    });
  }
  function addRows(rows: unknown[][], columns: number, indexes: number[], bucket?: number) {
    if(!Array.isArray(rows))throw new Error('Trasporto osservazioni non valido');
    const seen=new Set<number>();
    for(const row of rows) {
      if(!Array.isArray(row)||row.length!==columns)throw new Error('Trasporto osservazioni non valido');
      const id=row[indexes[0]];
      if(typeof id!=='number'||!Number.isSafeInteger(id)||id<=0||seen.has(id)||
        (bucket!==undefined&&Math.floor(id/1024)!==bucket))throw new Error('Trasporto osservazioni non valido');
      if(bucket!==undefined||missing.has(id))seen.add(id);
      if(missing.has(id)) {
        const status=row[indexes[1]],labels=row[indexes[2]];
        if(status==='predicted') {
          if(!Array.isArray(labels)||labels.some(label=>typeof label!=='string'||!profileContext.topicDisplayNames.has(label))||new Set(labels).size!==labels.length)
            throw new Error('Trasporto osservazioni non valido');
          found.set(id,labels);
        } else {
          if(!['no_model_vocabulary','no_mapped_outlinks'].includes(String(status))||labels!==null)throw new Error('Trasporto osservazioni non valido');
          found.set(id,null);
        }
      }
    }
  }
  if(entry.profile_topics_dir) {
    const directory=entry.profile_topics_dir;
    if(!/^[a-zA-Z0-9][a-zA-Z0-9_.-]*$/.test(directory))throw new Error('Trasporto osservazioni non valido');
    const index=await read(directory+'/index.json') as {
      format?: string; metadata?: DatasetMetadata; shard_size?: number; shards?: number[]; census_page_count?: number;
    };
    const shards=index?.shards;
    if(index?.format!=='profile_topics_index_v1'||!validMetadata(index.metadata)||index.shard_size!==1024||
      typeof index.census_page_count!=='number'||!Number.isSafeInteger(index.census_page_count)||index.census_page_count<0||
      !Array.isArray(shards)||shards.length>index.census_page_count||
      shards.some((bucket,i)=>!Number.isSafeInteger(bucket)||bucket<0||(i>0&&bucket<=shards[i-1])))
      throw new Error('Trasporto osservazioni non valido');
    if(index.metadata?.topics?.status==='predicted') {
      const available=new Set(shards);
      const buckets=[...new Set([...missing].map(id=>Math.floor(id/1024)))].filter(bucket=>available.has(bucket));
      let next=0;
      async function worker() {
        while(next<buckets.length) {
          signal.throwIfAborted();
          const bucket=buckets[next++];
          const shard=await read(directory+'/'+bucket+'.json') as {
            format?: string; metadata?: DatasetMetadata; bucket?: number; rows: unknown[][];
          };
          if(shard?.format!=='profile_topics_shard_v1'||!validMetadata(shard.metadata)||
            canonicalJSON(shard.metadata)!==canonicalJSON(index.metadata)||shard.bucket!==bucket||
            !Array.isArray(shard.rows)||shard.rows.length>1024)throw new Error('Trasporto osservazioni non valido');
          addRows(shard.rows,3,[0,1,2],bucket);
        }
      }
      await Promise.all(Array.from({length:Math.min(4,buckets.length)},()=>worker()));
    }
  } else {throw new Error('Trasporto osservazioni non disponibile');}
  for(const id of missing)cached.set(id,found.get(id)??null);
  profileTopicCache.set(key,cached);
  return cached;
}
