import { attachmentNode } from "./work-notes-attachment-node.js";
import { blockId } from "./work-notes-tiptap-serialization.js";

const uploadFailure = (name) => new Error(`‘${name || "첨부파일"}’ 업로드를 완료하지 못했습니다. 다시 시도하거나 취소해 주세요.`);
const unknownCopy = "첨부파일 저장 결과를 아직 확인하지 못했습니다. 다시 시도하면 같은 파일을 확인하며, 취소해도 저장된 파일을 삭제하지 않습니다.";
const outcomeUnknown = (error) => error?.outcomeUnknown === true || error?.code === "local_store_outcome_unknown";

export function createWorkNoteAttachmentUploadController(options) {
  const pending = new Map();
  const failed = new Map();

  const findRange = (token) => {
    const editor = options.getEditor();
    let found = null;
    editor?.state.doc.forEach((node, offset) => {
      if (node.attrs?.blockId === token || node.attrs?.uploadToken === token) found = { from: offset, to: offset + node.nodeSize };
    });
    return found;
  };
  const replace = (token, content, focus = false) => {
    const editor = options.getEditor();
    const range = findRange(token);
    if (!editor || !range) return false;
    const chain = editor.chain();
    if (focus) chain.focus();
    return chain.deleteRange(range).insertContentAt(range.from, content).run();
  };
  const immutableContext = (choice) => Object.freeze({
    token: blockId(),
    attachmentId: blockId(),
    pageId: options.getPageId(),
    documentId: options.getDocumentId?.() || "",
    editorGeneration: options.getGeneration(),
    file: choice.file,
    kind: choice.kind,
    displayMode: choice.displayMode,
  });
  const currentContext = (context) => context.pageId === options.getPageId()
    && context.documentId === (options.getDocumentId?.() || "")
    && context.editorGeneration === options.getGeneration();

  async function execute(context) {
    try {
      options.onStatus?.(`‘${context.file.name || "첨부파일"}’ 저장 중…`);
      const record = await options.uploadAttachment(context.pageId, context.token, context.file, context);
      if (!currentContext(context) || !findRange(context.token)) {
        await options.removeAttachment(record.attachmentId, context);
        throw new Error("첨부 위치가 바뀌어 업로드 파일을 안전하게 취소했습니다.");
      }
      replace(context.token, attachmentNode({ ...record, blockId: context.token }, context.kind, context.displayMode));
      failed.delete(context.token);
      options.onStatus?.(options.storedCopy?.(record) || `‘${record.fileName}’을 저장했습니다.`);
      return true;
    } catch (error) {
      if (outcomeUnknown(error) || context.outcomeUnknown) error = Object.assign(new Error(unknownCopy), {
        cause: error, code: error?.code, outcomeUnknown: true, mutationState: "unknown",
      });
      if (currentContext(context) && findRange(context.token)) {
        failed.set(context.token, { ...context, error });
        replace(context.token, attachmentNode({
          attachmentId: "", blockId: context.token, fileName: context.file.name || "첨부파일",
          contentType: context.file.type || "application/octet-stream", size: context.file.size || 0,
          uploadState: "failed", uploadToken: context.token, uploadError: error?.message || "업로드하지 못했습니다.",
        }, context.kind, context.displayMode));
      }
      options.onError?.(error);
      return false;
    } finally {
      pending.delete(context.token);
      if (!failed.has(context.token)) options.onSettled?.(context.pageId);
    }
  }

  function prepare(context) {
    let resolve;let reject;const promise=new Promise((ok,fail)=>{resolve=ok;reject=fail});const deferred={promise,resolve,reject};
    pending.set(context.token, { context, promise: deferred.promise, deferred });
    return deferred.promise;
  }
  async function runPrepared(context) {
    const entry = pending.get(context.token);
    try { const result = await execute(context); entry?.deferred.resolve(result); return result; }
    catch (error) { entry?.deferred.reject(error); throw error; }
  }
  function start(context) {
    const promise = prepare(context);
    queueMicrotask(() => { void runPrepared(context); });
    return promise;
  }

  async function insert(choices, position = null) {
    const selected = [...choices].filter((choice) => choice?.file);
    if (!selected.length || !options.canEdit()) return [];
    const contexts = selected.map(immutableContext);
    const tasks = new Map(contexts.map((context) => [context.token, prepare(context)]));
    const placeholders = contexts.map((context) => attachmentNode({
      attachmentId: "", blockId: context.token, fileName: context.file.name || "첨부파일",
      contentType: context.file.type || "application/octet-stream", size: context.file.size || 0,
      uploadState: "pending", uploadToken: context.token,
    }, context.kind, context.displayMode));
    const editor = options.getEditor();
    const insertion = Number.isFinite(position) ? position : editor.state.selection.from;
    editor.chain().focus().insertContentAt(insertion, placeholders).run();
    let cursor = 0;
    const workers = Array.from({ length: Math.min(3, contexts.length) }, async () => {
      while (cursor < contexts.length) {
        const context = contexts[cursor];
        cursor += 1;
        await runPrepared(context);
      }
    });
    await Promise.all(workers);
    return Promise.all(contexts.map((context) => tasks.get(context.token)));
  }

  return {
    insert,
    async retry(token) {
      const record = failed.get(token);
      if (!record || !currentContext(record)) return false;
      failed.delete(token);
      const task = start(Object.freeze({ ...record, retry: true, outcomeUnknown: outcomeUnknown(record.error), error: undefined }));
      replace(token, attachmentNode({ attachmentId: "", blockId: token, fileName: record.file.name || "첨부파일",
        contentType: record.file.type || "application/octet-stream", size: record.file.size || 0,
        uploadState: "pending", uploadToken: token }, record.kind, record.displayMode), true);
      return task;
    },
    async cancel(token) {
      const record = failed.get(token);
      if (!record) return false;
      failed.delete(token);
      const editor = options.getEditor();
      const range = findRange(token);
      if (range) editor.chain().focus().deleteRange(range).run();
      options.onSettled?.(record.pageId);
      return true;
    },
    hasBlocking(pageId = "") {
      return [...pending.values()].some((entry) => !pageId || entry.context.pageId === pageId)
        || [...failed.values()].some((entry) => !pageId || entry.pageId === pageId);
    },
    async waitForPending(pageId = "") {
      while (true) {
        const tasks = [...pending.values()].filter((entry) => !pageId || entry.context.pageId === pageId).map((entry) => entry.promise);
        if (!tasks.length) break;
        await Promise.all(tasks);
      }
      const failure = [...failed.values()].find((entry) => !pageId || entry.pageId === pageId);
      if (failure) throw outcomeUnknown(failure.error) ? failure.error : uploadFailure(failure.file?.name);
    },
  };
}
