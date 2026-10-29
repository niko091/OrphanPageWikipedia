import type {ManifestEntry, ConnectionMetadata, Dataset, DashboardSummaryData} from './types.js';
import {isRecord} from './validation.js';

export function validateManifest(value: unknown): ManifestEntry[] {
  if(!Array.isArray(value)||!value.length)throw new Error('Nessuna raccolta presente');
  const languages=new Set<string>(),files=new Set<string>();
  for(const entry of value) {
    if(!isRecord(entry)||entry.storage!=='relational_census_v1'||typeof entry.language!=='string'||
      !/^[a-z][a-z0-9-]{0,19}$/.test(entry.language)||languages.has(entry.language)||
      typeof entry.file!=='string'||!/^[a-zA-Z0-9_.-]+\.json$/.test(entry.file)||files.has(entry.file)||
      typeof entry.date!=='string'||!Number.isFinite(Date.parse(entry.date))||
      typeof entry.summary_file!=='string'||!/^[a-zA-Z0-9_.-]+\.json$/.test(entry.summary_file)||
      (entry.language==='all')!==(entry.kind==='aggregate'))throw new Error('Manifest della raccolta non valido');
    if(entry.kind!=='aggregate')for(const key of ['cards_dir','connections_dir','profile_topics_dir','connections_metadata_file']) {
      if(typeof entry[key]!=='string'||!/^[a-zA-Z0-9][a-zA-Z0-9_.-]*$/.test(entry[key]))throw new Error('Manifest della raccolta non valido');
    }
    languages.add(entry.language);files.add(entry.file);
  }
  return value as ManifestEntry[];
}

export function validateDataset(next: Dataset) {
  if(!next?.metadata||!Array.isArray(next.pages)||!/^[a-z][a-z0-9-]{0,19}$/.test(next.metadata.language)||next.metadata.language==='all')throw new Error('Codice lingua non valido');
  if(next.pages.some(page=>!isRecord(page)||!Number.isSafeInteger(page.page_id)||page.page_id<=0||typeof page.title!=='string'||typeof page.orphan!=='boolean'))throw new Error('Indice degli articoli non valido');
  if(next.metadata.biographies || next.biographies || next.pages.some(p=>'gender_group' in p || 'biography' in p)) throw new Error('Dataset con arricchimento esterno: rigenera i risultati dai dump ufficiali');
}
export async function readConnections(entry: ManifestEntry,signal?: AbortSignal): Promise<Dataset> {
  const pageID=new URLSearchParams(window.location.search).get('page');
  if(!pageID||!/^[1-9][0-9]*$/.test(pageID))throw new Error('Identificativo della pagina non valido');
  if(!entry.connections_dir||!/^[a-zA-Z0-9][a-zA-Z0-9_.-]*$/.test(entry.connections_dir)||
    !entry.connections_metadata_file||!/^[a-zA-Z0-9_.-]+\.json$/.test(entry.connections_metadata_file))throw new Error('Nome dataset non valido');
  const [detailResponse,sharedResponse]=await Promise.all([
    fetch(`data/${entry.connections_dir}/${pageID}.json`,{cache:'no-store',signal}),
    fetch('data/'+entry.connections_metadata_file,{cache:'no-store',signal})]);
  if(!detailResponse.ok||!sharedResponse.ok)throw new Error('Dataset non disponibile');
  const detail: unknown=await detailResponse.json(),shared=await sharedResponse.json() as ConnectionMetadata;
  if(shared?.format!=='connections_metadata_v1'||!Number.isSafeInteger(shared.census_page_count)||shared.census_page_count<0||
    shared.metadata?.language!==entry.language||shared.metadata.finished_at!==entry.date||shared.metadata.complete!==true||
    Boolean(shared.metadata.demo)!==Boolean(entry.demo)||typeof shared.connection_identity!=='string'||
    !/^[a-f0-9]{64}$/.test(shared.connection_identity)||!isRecord(detail)||detail.format!=='connections_v2'||
    detail.metadata!==undefined||detail.connection_identity!==shared.connection_identity||detail.census_page_count!==shared.census_page_count||
    !Array.isArray(detail.pages)||detail.pages.length!==1||!isRecord(detail.pages[0])||String(detail.pages[0].page_id)!==pageID||
    detail.pages[0].orphan!==true||!isRecord(detail.link_candidates)||!isRecord(detail.link_candidates[pageID])||
    !Array.isArray(detail.link_candidates[pageID].candidates))throw new Error('Metadati dei collegamenti non validi');
  const dataset={metadata:shared.metadata,pages:detail.pages,link_candidates:detail.link_candidates,census_page_count:shared.census_page_count} as Dataset;
  validateDataset(dataset);return dataset;
}

export async function readSummary(entry: ManifestEntry,signal?: AbortSignal): Promise<DashboardSummaryData> {
  const filename=entry.summary_file;
  if(!filename||!/^[a-zA-Z0-9_.-]+\.json$/.test(filename))throw new Error('Nome riepilogo non valido');
  const response=await fetch('data/'+filename,{cache:'no-store',signal});
  if(!response.ok)throw new Error('Riepilogo non disponibile');
  const summary=await response.json() as DashboardSummaryData;
  if(summary?.format!=='dashboard_summary_v1'||!['creation_quality_creators_v3','mixed_per_language'].includes(summary.measures_rule)||summary.metadata?.language!==entry.language||
    summary.metadata.finished_at!==entry.date||summary.metadata.complete!==true||
    Boolean(summary.metadata.demo)!==Boolean(entry.demo)||
    !Number.isSafeInteger(summary.census_page_count)||summary.census_page_count<0||
    !Number.isSafeInteger(summary.reports?.overview?.orphans)||summary.reports.overview.orphans<0||
    summary.reports.overview.orphans>summary.census_page_count||
    !summary.reports||!summary.reports.overview||!Array.isArray(summary.reports.categories)||
    !summary.reports.profiles||!summary.reports.numeric||!summary.reports.origins||
    !Array.isArray(summary.reports.creator_groups)||
    summary.reports.overview.total!==summary.census_page_count)
    throw new Error('Riepilogo non valido');
  for(const key of ['age_days','length_bytes','creator_prior_edits_main','creator_prior_edits_other','creator_prior_articles',
    'creator_tenure_days','creator_articles_created_total','creator_orphans_current','creator_continuity_months_before_creation']) {
    const report=summary.reports.numeric[key];
    if(!report||!Array.isArray(report.groups)||report.groups.length!==2)throw new Error('Riepilogo non valido');
  }
  const months=summary.reports.numeric.creator_continuity_months_before_creation.month_histogram;
  if(months&&(months.rule!=='months_0_12_overflow_v1'||months.through!==12||!Array.isArray(months.groups)||months.groups.length!==2||
    !Number.isSafeInteger(months.count_max)||months.count_max<1||
    months.groups.some(group=>!Array.isArray(group.counts)||group.counts.length!==14||
      group.counts.some(value=>!Number.isSafeInteger(value)||value<0)||
      !Array.isArray(group.percentages)||group.percentages.length!==14||
      group.percentages.some(value=>value!==null&&(!Number.isFinite(value)||value<0||value>100)))))
    throw new Error('Riepilogo della continuità non valido.');
  return summary;
}

export function canonicalJSON(value: unknown): string {
  if(Array.isArray(value))return '['+value.map(canonicalJSON).join(',')+']';
  if(value&&typeof value==='object')return '{'+Object.keys(value).sort().map(key=>JSON.stringify(key)+':'+canonicalJSON((value as Record<string, unknown>)[key])).join(',')+'}';
  return JSON.stringify(value);
}
