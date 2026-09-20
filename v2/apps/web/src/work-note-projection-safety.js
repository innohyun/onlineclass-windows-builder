const RECEIPT_TOKEN = /^[A-Za-z0-9_-]{43}$/u;

function projectionError(code, message) {
  return Object.assign(new Error(message), { code });
}

export function isWorkNoteProjectionReceipt(receipt, { requireActorId = false } = {}) {
  return Boolean(receipt && RECEIPT_TOKEN.test(String(receipt.commitToken || ''))
    && String(receipt.roomEpoch || '').trim()
    && Number.isSafeInteger(receipt.roomRevision) && receipt.roomRevision >= 0
    && (!requireActorId || String(receipt.actorId || '').trim()));
}

export function assertWorkNoteProjectionReceipt(receipt, { requireActorId = false,
  code = 'WORK_NOTE_PROJECTION_RECEIPT_REQUIRED' } = {}) {
  if (!isWorkNoteProjectionReceipt(receipt, { requireActorId })) {
    const message = code === 'WORK_NOTE_PROJECTION_RECEIPT_REQUIRED'
      ? '안전 저장 방식이 업데이트되었습니다. 현재 본문을 복사한 뒤 화면 전체를 새로고침해 주세요.'
      : '본문 실시간 저장 확인 정보가 올바르지 않습니다. 화면 전체를 새로고침해 주세요.';
    throw projectionError(code, message);
  }
  return receipt;
}

export function hasMeaningfulWorkNoteBlocks(blocks) {
  const keys = new Set(['text', 'title', 'fileName', 'attachmentId', 'pageId', 'href', 'src', 'url']);
  const visit = (value, key = '') => {
    if (Array.isArray(value)) return value.some((item) => visit(item));
    if (!value || typeof value !== 'object') return keys.has(key) && String(value || '').trim().length > 0;
    return Object.entries(value).some(([nestedKey, nested]) => visit(nested, nestedKey));
  };
  return Array.isArray(blocks) && visit(blocks);
}

export function recordExplicitEmptyIntent(intentVersions, { pageId, dirtyVersion, blocks, change = {} }) {
  if (hasMeaningfulWorkNoteBlocks(blocks) || change.origin === 'remote') {
    intentVersions.delete(pageId);
    return;
  }
  const preservesLocalDelete = intentVersions.has(pageId)
    && (change.origin === 'editor' || change.origin === 'serialization');
  if ((change.origin === 'editor' && change.explicitEmpty === true) || preservesLocalDelete) {
    intentVersions.set(pageId, dirtyVersion);
  }
}

export function allowsExplicitEmptyProjection(intentVersions, { pageId, dirtyVersion, blocks }) {
  return !hasMeaningfulWorkNoteBlocks(blocks) && intentVersions.get(pageId) === dirtyVersion;
}

export function isRetryableWorkNoteProjectionError(error) {
  if (error?.name === 'AbortError') return false;
  const status = Number(error?.status || 0);
  if (status === 429 || status >= 500) return true;
  if (status >= 400) return false;
  if (String(error?.code || '').startsWith('WORK_NOTE_')) return false;
  return error instanceof TypeError;
}
