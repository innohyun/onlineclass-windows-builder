export function workNotePastedUrl(value, origin) {
  const text = String(value || '').trim();
  if (!/^https?:\/\/\S+$/iu.test(text)) return null;
  let url;
  try { url = new URL(text); } catch { return null; }
  if (!['http:', 'https:'].includes(url.protocol)) return null;
  const sameOrigin = url.origin === origin;
  if (sameOrigin && url.pathname === '/admin/work-notes/shared') {
    const documentId = String(url.searchParams.get('documentId') || '').trim();
    const pageId = String(url.searchParams.get('pageId') || '').trim();
    if (documentId && pageId && !documentId.includes('/') && !pageId.includes('/')) {
      return { href:`worknote://cloud/${documentId}/${pageId}`, label:'업무 노트 페이지', internal:true };
    }
  }
  const host = url.hostname.replace(/^www\./u, '');
  let readablePath = url.pathname;
  try { readablePath = decodeURIComponent(url.pathname); } catch { /* 인코딩이 깨진 경로는 URL 원문을 사용한다. */ }
  const path = readablePath.replace(/\/$/u, '').split('/').filter(Boolean).slice(0, 2).join(' › ');
  const label = `${host}${path ? ` · ${path}` : ''}`.slice(0, 90);
  return { href:url.href, label, internal:false };
}
