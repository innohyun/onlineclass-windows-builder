type DocumentReference = {kind:'local'|'cloud';pageId:string;documentId?:string;openedAt?:number};

// Keep the existing web/desktop reference-only contract. Never persist document text.
function references(value:unknown,recent:boolean):DocumentReference[] {
  if(!Array.isArray(value))return [];
  const seen=new Set<string>();const result:DocumentReference[]=[];
  for(const ref of value){
    if(!ref || !['local','cloud'].includes(ref.kind) || typeof ref.pageId!=='string' || !ref.pageId.trim() || ref.pageId.length>160)continue;
    if(ref.kind==='cloud' && (typeof ref.documentId!=='string' || !ref.documentId.trim() || ref.documentId.length>160))continue;
    if(recent && (!Number.isFinite(Number(ref.openedAt)) || Number(ref.openedAt)<=0))continue;
    const key=`${ref.kind}:${ref.documentId || ''}:${ref.pageId}`;if(seen.has(key))continue;seen.add(key);
    result.push({kind:ref.kind,pageId:ref.pageId,...(ref.kind==='cloud'?{documentId:ref.documentId}:{}),...(recent?{openedAt:Number(ref.openedAt)}:{})});
  }
  return result.slice(0,recent?20:200);
}
function key(tenantId:string){return `classaimate:work-notes:tabs:v2:${tenantId}`;}
function preferences(tenantId:string){
  try{const value=JSON.parse(localStorage.getItem(key(tenantId)) || 'null');return {pinned:references(value?.pinned,false),recent:references(value?.recent,true)};}
  catch{return {pinned:[] as DocumentReference[],recent:[] as DocumentReference[]};}
}
export function pinnedDocumentIds(tenantId:string):Set<string>{
  return new Set(preferences(tenantId).pinned.filter(ref=>ref.kind==='local').map(ref=>ref.pageId));
}
export function toggleDocumentFavorite(tenantId:string,pageId:string):boolean{
  if(!tenantId.trim() || !pageId.trim() || pageId.length>160)throw new Error('학급과 문서를 먼저 선택해 주세요.');
  const value=preferences(tenantId);const pinned=!value.pinned.some(ref=>ref.kind==='local' && ref.pageId===pageId);
  value.pinned=value.pinned.filter(ref=>ref.kind!=='local' || ref.pageId!==pageId);
  if(pinned){if(value.pinned.length>=200)throw new Error('즐겨찾기는 최대 200개입니다. 기존 즐겨찾기를 해제한 뒤 추가해 주세요.');value.pinned.push({kind:'local',pageId});}
  try{localStorage.setItem(key(tenantId),JSON.stringify(value));}
  catch{throw new Error('즐겨찾기를 저장하지 못했습니다. 저장공간을 확인한 뒤 다시 시도해 주세요.');}
  window.dispatchEvent(new CustomEvent('desk:favorites-changed',{detail:{tenantId,pageId,pinned}}));
  return pinned;
}
