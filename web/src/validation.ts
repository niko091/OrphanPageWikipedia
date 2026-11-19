export function isRecord(value: unknown): value is Record<string,unknown> {
  return value!==null&&typeof value==='object'&&!Array.isArray(value);
}

export function validEditorialRanking(candidate: Record<string,unknown>): boolean {
  const {local_mention,mention_revision,reference_count}=candidate;
  return (reference_count==null||(typeof reference_count==='number'&&Number.isSafeInteger(reference_count)&&reference_count>=0))&&
    (local_mention==null?mention_revision==null:typeof local_mention==='boolean'&&
      typeof mention_revision==='number'&&Number.isSafeInteger(mention_revision)&&mention_revision>0);
}
