import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import type {Dataset, ManifestEntry} from '../../web/src/types.js';

export const root=fileURLToPath(new URL('../../',import.meta.url));
export const executable=path.join(root,'target/debug/orphanwiki');
export const title=(id: number): string=>`Voce sintetica ${String(id).padStart(3,'0')}`;
export const record=(value: unknown): value is Record<string,unknown>=>
  typeof value==='object'&&value!==null&&!Array.isArray(value);
export const env=Object.fromEntries(Object.entries(process.env).filter(([key])=>!key.startsWith('ORPHANWIKI_')));

function native(args: string[]): void {
  const result=spawnSync(executable,args,{cwd:root,env,encoding:'utf8',timeout:60_000});
  assert.ifError(result.error);assert.equal(result.status,0,result.stderr);
}

// Private complete synthetic censuses. Nothing reads or modifies research data.
export function prepareSite(directory: string): {web: string; datasets: Dataset[]; manifest: ManifestEntry[]} {
  const web=path.join(directory,'web'),data=path.join(web,'data');
  fs.mkdirSync(data,{recursive:true});
  for(const page of ['index','creators','deorphanize','connections','about'])
    fs.copyFileSync(path.join(root,'web',`${page}.html`),path.join(web,`${page}.html`));
  fs.cpSync(path.join(root,'web/assets'),path.join(web,'assets'),{recursive:true});
  const inputs=['vec','lmo'].map(language=>{
    const graph={metadata:{language,complete:true,demo:true,source:'Synthetic browser census',
      item_mapping:{source:'page_props.wikibase_item',complete:true},
      started_at:'2026-09-29T00:00:00Z',finished_at:'2026-09-29T00:00:00Z'},
      pages:Array.from({length:64},(_,index)=>{
        const id=index+1;
        return {page_id:id,title:title(id),wikibase_item:`Q${id}`,length_bytes:id===1?null:100+id,
          categories:['Categoria sintetica'],page_created_at:'2020-01-01T00:00:00Z',
          links:id<=32?[]:[title(id===64?33:id+1),...(language==='lmo'?[title(id-32)]:[])]};
      })};
    const file=path.join(directory,`${language}-raw.json`);fs.writeFileSync(file,JSON.stringify(graph));return file;
  });
  native(['compare',...inputs,'--output-dir',data,'--work-dir',path.join(directory,'staging')]);
  const manifest=JSON.parse(fs.readFileSync(path.join(data,'manifest.json'),'utf8')) as ManifestEntry[];
  const datasets: Dataset[]=[];
  for(const entry of manifest.filter(entry=>entry.kind!=='aggregate')) {
    const dataset=JSON.parse(fs.readFileSync(path.join(data,entry.file),'utf8')) as Dataset;
    entry.demo=false;dataset.metadata.demo=false;
    // Only private fixtures enable editorial controls. All external calls are mocked.
    Object.assign(dataset.metadata,{sql_release:'20261001',history:{status:'collected',history_release:'2026-09'},
      quality:{status:'not_collected',content_release:'2026-10-01'},topics:{status:'predicted'},
      creators:{status:'collected',language:entry.language,languages_scanned:[entry.language]}});
    for(const page of dataset.pages)Object.assign(page,{
      creator_key:`synthetic:creator:${(page.page_id-1)%32}`,creator_attribution:'synthetic',creator_account_type:'registered',
      creator_articles_created_total:2,creator_articles_in_census:2,creator_orphans_current:1,creator_orphans_current_share:0.5,
      creator_prior_articles:0,creator_prior_edits_main:0,creator_prior_edits_other:0,creator_tenure_days:10,
      creator_continuity_months_before_creation:page.page_id===1?null:page.page_id<=40?0:page.page_id<=50?1:page.page_id<=58?2:page.page_id===64?113:page.page_id-56,
      creator_languages_created:[entry.language],creator_data_status:'matched',
      topic_status:'predicted',topic_labels:page.page_id===1?[]:['Culture.Media.Music']});
    const original=entry.file;entry.file=entry.language==='vec'?'fixture.json':'lmo.json';
    fs.writeFileSync(path.join(data,entry.file),JSON.stringify(dataset));
    for(const aggregate of manifest.filter(item=>item.kind==='aggregate')) {
      const index=JSON.parse(fs.readFileSync(path.join(data,aggregate.file),'utf8')) as Record<string,unknown>;
      assert(record(index.metadata)&&Array.isArray(index.members));index.metadata.demo=false;aggregate.demo=false;
      for(const member of index.members)if(record(member)&&member.file===original) {
        member.file=entry.file;member.metadata=dataset.metadata;
      }
      fs.writeFileSync(path.join(data,aggregate.file),JSON.stringify(index));
    }
    datasets.push(dataset);
  }
  fs.writeFileSync(path.join(data,'manifest.json'),JSON.stringify(manifest));
  native(['dashboard','--data-dir',data,'--collection-database',path.join(data,'collection.sqlite')]);
  const current=JSON.parse(fs.readFileSync(path.join(data,'manifest.json'),'utf8')) as ManifestEntry[];
  return {web,datasets,manifest:current};
}
