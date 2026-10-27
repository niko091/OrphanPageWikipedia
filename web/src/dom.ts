import type {UIElements} from './types.js';

export function $<K extends string>(id: K): (K extends keyof UIElements ? UIElements[K] : HTMLElement) | null {
  return document.getElementById(id) as (K extends keyof UIElements ? UIElements[K] : HTMLElement) | null;
}

export const creatorsPage = document.body.dataset.page === 'creators';
export const connectionsPage = document.body.dataset.page === 'connections';
export const deorphanizePage = document.body.dataset.page === 'deorphanize';

export function element<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string | null, className?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (text != null) node.textContent = text;
  if (className) node.className = className;
  return node;
}
export function appendBarRow(box: HTMLElement, label: string, value: number | null, maximum: number, display: string, title?: string) {
  const row=element('div',null,'bar-row'),track=element('div',null,'bar-bg'),bar=element('div',null,'bar');
  bar.style.width=value==null?'0%':`${value/maximum*100}%`;
  track.append(bar);
  if(title)row.title=title;
  row.append(element('span',label,'bar-label'),track,element('span',display,'bar-value'));
  box.append(row);
}
