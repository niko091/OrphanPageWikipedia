import type {Dataset, DashboardSummaryData, FeatureSummary, MetricField, NumericReport} from './types.js';
import {$, element, appendBarRow} from './dom.js';
import {t, number, locale} from './i18n.js';
interface ChartContext {
 readonly data: Dataset; readonly precomputed: DashboardSummaryData;
 readonly topicDisplayNames: Map<string,string>;
 creatorMode: () => 'observed' | 'history' | 'missing';
}
export function createCharts(context: ChartContext) {
function featureBars(id: string, summary: FeatureSummary, count=false) {
  const box=$(id)!;box.replaceChildren();
  const population=summary.totals[0]+summary.totals[1];
  const maximum=count?Math.max(1,...summary.rows.map(row=>row.orphans)):100;
  for(const feature of summary.rows) {
    // A known empty category has zero observed orphans, but no defined rate.
    // Without any observations, neither counts nor percentages are available.
    const value=count?(feature.known?feature.orphans:null):feature.percent;
    const description=feature.known===0
      ?t('{label}: dato non disponibile; copertura {known}/{population}.',{label:feature.label,known:number(feature.known),population:number(population)})
      :feature.total===0&&!count
        ?t('{label}: Nessuna voce soddisfa questo requisito; percentuale non definita. Copertura {known}/{population}.',{label:feature.label,known:number(feature.known),population:number(population)})
        :t('{label}: {orphans}/{total} voci orfane; copertura {known}/{population}.',{label:feature.label,orphans:number(feature.orphans),total:number(feature.total),known:number(feature.known),population:number(population)});
    const display=`${number(value)}${value!=null&&!count?'%':''} (${number(feature.orphans)}/${number(feature.total)})`;
    appendBarRow(box,feature.label,value,maximum,display,description);
  }
  box.append(element('p',t(count
      ?'Ogni barra conta le orfane che soddisfano il requisito; tra parentesi: orfane/tutte le voci con il requisito. I dati mancanti sono esclusi. — indica dati non disponibili.'
      :'Ogni barra mostra la percentuale di orfane fra le voci che soddisfano il requisito: orfane/tutte le voci con il requisito. I dati mancanti sono esclusi. — indica dati non disponibili o un denominatore vuoto.')));
}
function paperProfiles() {
  const report=structuredClone(context.precomputed.reports.profiles);
  for(const row of report.topics.rows)row.label=context.topicDisplayNames.get(row.label)??row.label;
  featureBars('topics-full',report.topics,$('topics-full-metric')!.value==='count');
  featureBars('topics-paper',report.macrothemes,$('topics-paper-metric')!.value==='count');
  $('topics-full')!.append(element('p',t(report.topics.rows[0].known>0?
    'Temi assegnati con probabilità > 0,5. Una voce può avere più temi; le percentuali non devono sommare a 100%. I macrotemi uniscono le etichette dei propri sottotemi. Women è un tema predetto, non una misura del genere biografico.':
    'I temi del modello non sono presenti nella raccolta. Nessun tema viene dedotto dalle categorie o dal genere della voce.')));
  const featureLabels: Record<string,string> = {
    new:'Voci nuove (< mediana)',old:'Voci vecchie (≥ mediana)',
    quality_high:'Qualità strutturale alta (Johnson V2, ≥ mediana)',quality_low:'Qualità strutturale bassa (Johnson V2, < mediana)',
    short:'Lunghezza minore (< mediana)',long:'Lunghezza maggiore (≥ mediana)'
  };
  for(const row of report.features.rows)row.label=t(featureLabels[row.label]);
  featureBars('features-paper',report.features);
  const measureRule=context.precomputed.measures_rule;
  const qualityNote=measureRule==='mixed_per_language'
    ?'Qualità strutturale Johnson V2: le edizioni incluse usano regole di revisione diverse. Consultare la provenienza di ciascuna edizione prima di confrontare la copertura.'
    :"Qualità strutturale stimata con Johnson V2 dal wikitesto dell'export XML ufficiale selezionato. Soglia alla mediana della singola edizione; i punteggi non sono una scala assoluta tra lingue. La copertura include revisioni diverse dal grafo SQL quando il testo è disponibile; le due revisioni restano distinte.";
  $('features-paper')!.append(element('p',t(qualityNote)));
}
function categories() {
  const ordered=context.precomputed.reports.categories;
  const box = $('categories')!; box.replaceChildren();
  const count = $('category-metric')!.value === 'count';
  const max = count ? Math.max(1,ordered[0]?.orphans??0) : 100;
  for (const g of ordered.slice(0,20)) {
    const value = count ? g.orphans : g.percent;
    appendBarRow(box,g.label,value,max,`${number(value)}${count ? '' : '%'} (${g.orphans}/${g.total})`);
  }
  if (!ordered.length) box.append(element('p', t('Nessuna voce disponibile.')));
}
function distribution(id: string, field: MetricField, unit: string, axisLabel: string | null = null,
  ): NumericReport {
  unit=t(unit);
  const box = $(id)!; box.replaceChildren();
  const mode = $('chart-type')!.value || 'percent';
  const histogram = mode !== 'cumulative', percent = mode === 'percent';
  const summaryKey=id==='age'?field:id==='length'?'length_bytes':id.replace(/-chart$/,'');
  const report=context.precomputed.reports.numeric[summaryKey];
  if(!report)throw new Error('Riepilogo non valido');
  const groups=report.groups,totals=groups.map(group=>group.total);
  if(!report.available) {
    box.append(element('p',t('Dati non disponibili.')));
    return report;
  }
  const ns = 'http://www.w3.org/2000/svg';
  function svgNode(tag: string, attrs: Record<string, string | number>, text?: string) {
    const n = document.createElementNS(ns,tag); for(const [k,v] of Object.entries(attrs)) n.setAttribute(k,String(v));
    if(text!=null) n.textContent=text; return n;
  }
  const width=Math.max(260, box.clientWidth || 900), height=width<600?380:520;
  const left=64, right=24, top=32, bottom=height-76;
  const plotWidth=width-left-right, plotHeight=bottom-top;
  const binCount=20,bins=groups.map(group=>group.counts),groupedTails=Array.isArray(report.cumulative_ticks);
  const yMax=mode==='count'?report.count_max:100;
  const chartLabel=t(percent?'Percentuale del gruppo per intervallo':histogram?'Numero di pagine per intervallo':'Percentuale cumulativa per gruppo');
  const svg = svgNode('svg', {viewBox:`0 0 ${width} ${height}`, role:'img', 'aria-label':`${chartLabel}: ${field}`});
  svg.append(svgNode('text',{x:left,y:18,fill:'var(--text)','font-size':16},mode==='count'?t('Numero di pagine'):t('% di voci del gruppo')));
  for (const fraction of [0,0.25,0.5,0.75,1]) {
    const y=bottom-fraction*plotHeight, value=fraction*yMax;
    svg.append(svgNode('line',{x1:left,x2:width-right,y1:y,y2:y,stroke:'var(--border)'}),svgNode('text',{x:2,y:y+4,fill:'var(--muted)','font-size':16},number(value)+(mode==='count'?'':'%')));
  }
  if(histogram) {
    const binWidth=plotWidth/binCount, barWidth=binWidth*0.42;
    bins.forEach((frequencies,group)=>frequencies.forEach((count,bin)=>{
      if(percent && totals[group]===0)return;
      const lo=report.bin_edges[bin],hi=report.bin_edges[bin+1];
      const boundary=(value: number)=>new Intl.NumberFormat(locale(),{maximumSignificantDigits:6}).format(value);
      const value=percent?groups[group].percentages[bin]:count;
      if(value===null)return;
      const range=groupedTails&&bin===0?`<${boundary(hi)}`:groupedTails&&bin===binCount-1?`>${boundary(lo)}`:
        `[${boundary(lo)}, ${boundary(hi)}${bin===(groupedTails?binCount-2:binCount-1)?']':')'}`;
      const description=`${t(group===0?'Orfane':'Non orfane')} · ${range} ${unit}: ${percent?`${number(value)}% (${count}/${totals[group]} ${t('pagine del gruppo')})`:`${count} ${t('pagine')}`}`;
      const bar=svgNode('rect',{x:left+bin*binWidth+binWidth*0.06+group*barWidth,y:bottom-value/yMax*plotHeight,width:barWidth,height:value/yMax*plotHeight,fill:group===0?'var(--orange)':'var(--teal)','aria-label':description});
      bar.append(svgNode('title',{},description));
      svg.append(bar);
    }));
  } else groups.forEach((group,index)=>{
    if(!group.known)return;
    let path=`M${left} ${bottom}`;
    for(const [x,y] of group.cumulative)path+=` H${left+x*plotWidth} V${bottom-y*plotHeight}`;
    svg.append(svgNode('path',{d:path,stroke:index===0?'var(--orange)':'var(--teal)',fill:'none','stroke-width':3}));
  });
  const positions=width<600?[0,2,4]:[0,1,2,3,4];
  for(const index of positions) {
    const original=(histogram?report.ticks:report.cumulative_ticks)[index];
    const value=field==='length_bytes'?new Intl.NumberFormat(locale(),{notation:'compact',maximumFractionDigits:1}).format(original):number(original);
    const tick=histogram&&groupedTails&&index===0?`<${value}`:histogram&&groupedTails&&index===4?`>${value}`:value;
    const x=histogram&&groupedTails?(1+index*18/4)/20:index/4;
    svg.append(svgNode('text',{x:left+x*plotWidth,y:bottom+30,'text-anchor':index===0?'start':index===4?'end':'middle',fill:'var(--muted)','font-size':16},tick));
  }
  svg.append(svgNode('text',{x:left+plotWidth/2,y:height-10,'text-anchor':'middle',fill:'var(--text)','font-size':16},axisLabel || (field==='age_days'?t('Età (anni)'):t('Lunghezza (byte, scala logaritmica)'))));
  box.append(svg);
  const orphanMedian=groups[0].median,otherMedian=groups[1].median;
  box.append(element('p',t('Mediana orfane: {a} {unit} (n={n}) · Non orfane: {b} {unit} (n={m})',{a:number(orphanMedian),b:number(otherMedian),unit,n:groups[0].known,m:groups[1].known})));
  const legend=element('div',null,'legend');
  legend.append(element('span',t('● Orfane'),'orphan'),element('span',t('● Non orfane'),'other'));
  box.append(legend);
  const missing=groups.map(group=>group.missing);
  box.append(element('p',t('Dati mancanti esclusi dal grafico: {a} orfane, {b} non orfane.',{a:number(missing[0]),b:number(missing[1])})));
  if(!histogram)box.append(element('p',t('Ogni curva usa come denominatore le voci del proprio gruppo con dato disponibile; arriva al 100%. Le mediane sono calcolate separatamente per gruppo.')));
  return report;
}
const originLabels: Record<string, string> = {translation_tagged:'Traduzione segnalata',new_page_unclassified:'Nuova pagina, modalità non documentata',imported_history_observed:'Cronologia importata',unknown:'Origine non determinabile'};
function creatorCharts() {
  const mode=context.creatorMode(),available=mode!=='missing';
  $('creators-empty')!.hidden=available;
  $('creator-section')!.hidden=!available; $('origin-section')!.hidden=!available;
  if(!available)return;
  const history=mode==='history';
  const definitions: Partial<Record<MetricField, [string, string]>> = {
    creator_prior_edits_main:['Modifiche precedenti sulle voci','modifiche'],
    creator_prior_edits_other:['Modifiche precedenti negli altri namespace','modifiche'],
    creator_prior_articles:['Voci già create prima dell’articolo','voci'],
    creator_tenure_days:['Anzianità del profilo alla creazione','giorni'],
    creator_articles_created_total:['Voci create, ancora esistenti','voci'],
    creator_orphans_current:['Voci orfane del creatore nel censimento','voci'],
  };
  for(const [field,[title,unit]] of Object.entries(definitions) as [MetricField, [string, string]][]) {
    const sourceField=history&&field==='creator_prior_edits_main'?'creator_prior_edits_main_history':
      history&&field==='creator_prior_edits_other'?'creator_prior_edits_other_history':field;
    const censusCount=history&&(field==='creator_articles_created_total'||field==='creator_orphans_current');
    const report=distribution(field+'-chart',sourceField,unit,t('{unit} (scala logaritmica)',{unit:t(unit)}));
    const heading=$(field+'-heading');
    if(heading)heading.textContent=t(history&&censusCount?
      field==='creator_articles_created_total'?'Voci attribuite al creatore storico nel censimento':'Orfane attuali attribuite al creatore storico':history&&field==='creator_prior_articles'?'Voci del censimento attribuite prima di questa voce':history&&field==='creator_tenure_days'?'Anzianità locale alla creazione ricostruita':title);
    if(history) {
      $(field+'-note')!.textContent=t(censusCount?
        'Attribuzione ricostruita dalla prima revisione in MediaWiki History; conteggio delle voci ancora nel censimento, non totale delle creazioni dell’account. Disponibile per {n}/{total} voci.':
        field==='creator_prior_edits_main'||field==='creator_prior_edits_other'?
          'Modifiche pubbliche strettamente precedenti alla creazione ricostruita in MediaWiki History; copertura limitata alla cronologia disponibile. Disponibile per {n}/{total} voci.':
          field==='creator_prior_articles'?
          'Altre voci del censimento attribuite allo stesso autore locale con creazione ricostruita strettamente precedente; copertura limitata alle date note. Disponibile per {n}/{total} voci.':
          'Registrazione locale fino alla creazione ricostruita; date mancanti o incoerenti restano indisponibili. Disponibile per {n}/{total} voci.',
        {n:number(report.known),total:number(context.precomputed.census_page_count)});
    } else {
      const coverage=(context.data.metadata.creators!.languages_scanned||[]).join(', ').toUpperCase();
      $(field+'-note')!.textContent=t('{title}. {coverage}; ogni voce è un’osservazione. Disponibile per {n}/{total} voci. {scope}',{
        title:t(title),coverage:t('Lingue esaminate: {languages}',{languages:coverage}),n:number(report.known),total:number(context.precomputed.census_page_count),
        scope:t(field.includes('prior')?'Conteggi strettamente precedenti alla creazione, sulla cronologia pubblica disponibile.':field==='creator_tenure_days'?'Registrazione locale, non creazione dell’account globale.':'Conteggi osservati nella raccolta, non totali storici comprensivi di pagine cancellate.')
      });
    }
  }
  const box=$('origins')!;box.replaceChildren();
  const report=context.precomputed.reports.origins;
  for(const group of report.origins) {
    appendBarRow(box,t(originLabels[group.label]||group.label),group.percent,100,
      `${number(group.percent)}% (${group.orphans}/${group.total})`);
  }
  const botNames=history?['Bot (data del dump)','Senza flag bot (data del dump)','Creatore o stato bot non disponibile']:
    ['Bot (stato attuale)','Non bot (stato attuale)','Creatore o stato bot non disponibile'];
  $('creator-bots-note')!.textContent=t(history?
    'Flag bot locale alla data del dump SQL; senza flag non significa umano. Disponibile per {n}/{total} voci. Ogni barra mostra orfane/tutte le voci del gruppo.':
    'Stato bot attuale del creatore, non stato storico alla creazione. Disponibile per {n}/{total} voci. Ogni barra mostra la percentuale di orfane nel gruppo (orfane/tutte le voci del gruppo); i dati mancanti sono separati.',{n:number(report.known_bots),total:number(context.precomputed.census_page_count)});
  const botBox=$('creator-bots')!;botBox.replaceChildren();
  for(const group of report.bots) {
    const value=group.percent;
    appendBarRow(botBox,t(botNames[Number(group.label)]),value,100,
      `${number(value)}${value==null?'':'%'} (${group.orphans}/${group.total})`);
  }
}
function historyCharts() {
  const available=['collected','partially_collected'].includes(context.data.metadata.history?.status??'');
  $('history-section')!.hidden=!available;
  if(!available)return;
  const report=distribution('creator_continuity_months_before_creation-chart',
    'creator_continuity_months_before_creation','Mesi consecutivi',null);
  $('creator_continuity_months_before_creation-note')!.textContent=t(
    'Adattamento della stabilità Vital Signs: mesi completi consecutivi prima della creazione con almeno cinque modifiche. Sono escluse le modifiche nel mese di creazione. Cronologia pubblica ricostruita; disponibile per {n}/{total} voci.',
    {n:number(report.known),total:number(context.precomputed.census_page_count)});
  $('creator-groups-note')!.textContent=t(
    'Gruppi storici ricostruiti da MediaWiki History alla prima revisione. Una voce può comparire in più gruppi; i gruppi assenti restano sconosciuti. Ogni barra mostra orfane/tutte le voci del gruppo.');
  const groups=context.precomputed.reports.creator_groups;
  const box=$('creator-groups')!;box.replaceChildren();
  for(const group of groups) {
    const groupLabel=group.label==='__missing__'?t('Gruppo non disponibile'):group.label;
    appendBarRow(box,groupLabel,group.percent,100,
      `${number(group.percent)}${group.percent==null?'':'%'} (${group.orphans}/${group.total})`);
  }
}
return {paperProfiles, categories, distribution, creatorCharts, historyCharts};
}
