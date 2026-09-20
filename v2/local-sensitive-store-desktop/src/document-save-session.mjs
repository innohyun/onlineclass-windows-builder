// A session owns one confirmed revision. New edits never adopt another writer's revision.
export function createDocumentSaveSession({ tenantId, page, repository, draftGeneration = 0, onState, onSaved, schedule = setTimeout, cancel = clearTimeout }) {
  let current = structuredClone(page);
  let revision = Number(page.revision ?? page.updatedAtMs ?? 0);
  let generation = Math.max(Date.now(), Number(draftGeneration) || 0);
  let savedGeneration = generation;
  let timer = null;
  let running = null;
  let draftQueue = Promise.resolve();
  let draftError = null;
  let composing = false;
  let failed = false;
  let closed = false;
  const emit = (state, error = null) => onState?.({ state, error, dirty: generation > savedGeneration, generation });
  function cancelTimer() { if (timer !== null) cancel(timer); timer = null; }
  function persistDraft() {
    const draft = { ...structuredClone(current), baseRevision: revision, generation };
    draftQueue = draftQueue.catch(() => {}).then(async () => {
      try { await repository.saveDraft(tenantId, draft); draftError = null; }
      catch (error) { draftError = error; emit('draft_error', error); throw error; }
    });
    void draftQueue.catch(() => {});
  }
  async function drain() {
    cancelTimer();
    if (running) { await running; if (generation > savedGeneration && !composing) return drain(); return; }
    if (composing) throw new Error('한글 입력을 마친 뒤 저장해 주세요.');
    if (generation <= savedGeneration) return;
    const snapshot = structuredClone(current);
    const savingGeneration = generation;
    emit('saving');
    running = (async () => {
      try {
        const saved = await repository.save(tenantId, snapshot, revision);
        if (!saved || saved.pageId !== page.pageId) throw new Error('저장 결과가 현재 문서와 일치하지 않습니다.');
        revision = Number(saved.revision ?? saved.updatedAtMs);
        if (!Number.isFinite(revision) || revision <= 0) throw new Error('저장 버전을 확인하지 못했습니다.');
        current.updatedAtMs = saved.updatedAtMs;
        current.revision = revision;
        if(generation===savingGeneration)current={...current,...structuredClone(saved)};
        savedGeneration = savingGeneration;
        failed = false;
        onSaved?.(saved, { latest: generation === savingGeneration });
        await draftQueue.catch(() => {});
        if (generation === savingGeneration) {
          try { await repository.discardDraft(tenantId, page.pageId, savingGeneration); } catch { /* A retained draft remains recoverable. */ }
          emit('saved');
        } else {
          // Rebasing a durable draft changes its payload even when its text is unchanged.
          generation += 1;
          persistDraft();
          emit('dirty');
        }
      } catch (error) { failed = true; emit('error', error); throw error; }
    })();
    try { await running; } finally { running = null; }
    if (generation > savedGeneration && !composing) return drain();
  }
  return {
    change(next) {
      if (closed) return;
      current = { ...structuredClone(next), revision, updatedAtMs: current.updatedAtMs };
      generation += 1;
      cancelTimer();
      if (composing) { emit('composing'); return; }
      persistDraft();
      emit(failed ? 'error' : 'dirty');
      // Conflicts must be resolved explicitly; autosave never retries against a fetched revision.
      if (!failed) timer = schedule(() => { timer = null; void drain().catch(() => {}); }, 650);
    },
    composition(active) {
      composing = active;
      cancelTimer();
      if (active) emit('composing');
      else if (generation > savedGeneration) {
        persistDraft(); emit('dirty');
        if (!failed) timer = schedule(() => { timer = null; void drain().catch(() => {}); }, 650);
      }
    },
    flush: drain,
    async checkpoint() { await draftQueue; if (draftError) throw draftError; },
    snapshot: () => structuredClone(current),
    revision: () => revision,
    dirty: () => generation > savedGeneration,
    isComposing: () => composing,
    generation: () => generation,
    close() { closed = true; cancelTimer(); },
  };
}
