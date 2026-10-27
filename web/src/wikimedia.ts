import {t} from './i18n.js';
import {isRecord} from './validation.js';
const profileAPIStarts: number[]=[];
let profileAPINotBefore=0;
function profileWait(milliseconds: number, signal?: AbortSignal): Promise<void> {
  signal?.throwIfAborted();
  return new Promise((resolve,reject)=>{
    const abort=()=>{clearTimeout(timer);signal?.removeEventListener('abort',abort);reject(signal?.reason);};
    const timer=setTimeout(()=>{signal?.removeEventListener('abort',abort);resolve();},milliseconds);
    signal?.addEventListener('abort',abort,{once:true});
  });
}
export async function wikimediaAPI(url: URL, signal?: AbortSignal, onWait?: (seconds: number) => void): Promise<unknown> {
  if(url.protocol!=='https:' || !(url.hostname==='meta.wikimedia.org'||/^[a-z][a-z0-9-]{0,19}\.wikipedia\.org$/.test(url.hostname)))
    throw new Error('API Wikimedia non valida');
  url.searchParams.set('format','json');url.searchParams.set('origin','*');
  // Editorial requests retain their single attempt and their existing outer cancellation.
  if(!onWait) {
    const response=await fetch(url.href,{cache:'no-store',credentials:'omit',signal});
    if(!response.ok)throw new Error(t('API Wikimedia: {host}, HTTP {status}',{host:url.hostname,status:response.status}));
    const payload: unknown=await response.json();
    if(!isRecord(payload)||'error' in payload)throw new Error(t('Risposta API Wikimedia non valida: {host}',{host:url.hostname}));
    return payload;
  }
  for(let attempt=0;attempt<3;attempt++) {
    // A shared rolling budget includes identity lookups, retries and later submissions.
    // A cooldown from any wiki also applies to all other profile workers in this tab.
    while(true) {
      signal?.throwIfAborted();
      const now=Date.now();
      while(profileAPIStarts.length&&profileAPIStarts[0]<=now-60_000)profileAPIStarts.shift();
      const wait=Math.max(profileAPINotBefore-now,profileAPIStarts.length>=180?profileAPIStarts[0]+60_000-now:0);
      if(wait<=0){profileAPIStarts.push(now);break;}
      const seconds=Math.ceil(wait/1000);
      if(wait>60_000)throw new Error(t('API Wikimedia: {host}, riprova tra {seconds} s',{host:url.hostname,seconds}));
      onWait(seconds);await profileWait(wait,signal);
    }
    const controller=new AbortController();
    const abort=()=>controller.abort(signal?.reason);
    signal?.addEventListener('abort',abort,{once:true});
    const timer=setTimeout(()=>controller.abort(),15_000);
    let failure: Error | null=null,retry=false,delay=5000*2**attempt;
    try {
      const response=await fetch(url.href,{cache:'no-store',credentials:'omit',signal:controller.signal});
      const after=response.headers.get('Retry-After');
      if(after!==null) {
        const seconds=Number(after),date=Date.parse(after);
        if(after.trim()!==''&&Number.isFinite(seconds)&&seconds>=0)delay=seconds*1000;
        else if(Number.isFinite(date))delay=Math.max(0,date-Date.now());
      }
      if(!response.ok) {
        failure=new Error(t('API Wikimedia: {host}, HTTP {status}',{host:url.hostname,status:response.status}));
        retry=[429,502,503,504].includes(response.status);
        throw failure;
      }
      let payload: unknown;
      try {payload=await response.json();}
      catch {failure=new Error(t('Risposta API Wikimedia non valida: {host}',{host:url.hostname}));throw failure;}
      if(!isRecord(payload)) {
        failure=new Error(t('Risposta API Wikimedia non valida: {host}',{host:url.hostname}));throw failure;
      }
      if('error' in payload) {
        const code=isRecord(payload.error)&&typeof payload.error.code==='string'?payload.error.code:'unknown';
        failure=new Error(t('API Wikimedia: {host}, errore {code}',{host:url.hostname,code}));
        retry=['ratelimited','maxlag'].includes(code);throw failure;
      }
      signal?.throwIfAborted();
      return payload;
    } catch {
      signal?.throwIfAborted();
      if(controller.signal.aborted) {
        failure=new Error(t('API Wikimedia: {host}, tempo di attesa scaduto',{host:url.hostname}));retry=true;
      } else if(!failure) {
        failure=new Error(t('API Wikimedia: {host}, errore di rete',{host:url.hostname}));retry=true;
      }
      if(!retry)throw failure;
      profileAPINotBefore=Math.max(profileAPINotBefore,Date.now()+delay);
      if(attempt===2)throw failure;
      if(delay>60_000)throw new Error(failure.message+'. '+t('API Wikimedia: {host}, riprova tra {seconds} s',{
        host:url.hostname,seconds:Math.ceil(delay/1000)}));
    } finally {
      clearTimeout(timer);signal?.removeEventListener('abort',abort);
    }
  }
  throw new Error(t('Risposta API Wikimedia non valida: {host}',{host:url.hostname}));
}
