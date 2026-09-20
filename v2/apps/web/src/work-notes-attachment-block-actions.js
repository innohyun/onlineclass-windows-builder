import { NodeSelection } from "@tiptap/pm/state";

import { deleteWorkNoteBlockRange, getWorkNoteBlockRange } from "./work-notes-block-range.js";
import { deleteWorkNoteNestedBlock, findWorkNoteHandleBlock } from "./work-notes-nested-blocks.js";

function attachmentIds(nodes) {
  const ids = new Set();
  const visit = (node) => {
    if (node.type.name === "attachmentBlock" && node.attrs?.attachmentId) ids.add(node.attrs.attachmentId);
    node.forEach(visit);
  };
  nodes.forEach(visit);
  return [...ids];
}

export function deleteWorkNoteAttachmentBlock(editor, blockId, { createId, removeAttachment, confirmDelete = globalThis.confirm, onError } = {}) {
  const record = findWorkNoteHandleBlock(editor.state.doc, blockId);
  if (record?.node.type.name !== "attachmentBlock") return deleteWorkNoteNestedBlock(editor, blockId, createId);
  if (!confirmDelete?.(`‘${record.node.attrs.fileName || "첨부파일"}’ 첨부를 삭제할까요?`)) return false;
  Promise.resolve(removeAttachment(record.node.attrs.attachmentId))
    .then(() => deleteWorkNoteNestedBlock(editor, blockId, createId))
    .catch((error) => onError?.(error));
  return true;
}

export function handleWorkNoteAttachmentDeleteKey(editor, event, options) {
  if (!["Backspace", "Delete"].includes(event.key) || !(editor?.state.selection instanceof NodeSelection)
    || editor.state.selection.node.type.name !== "attachmentBlock") return false;
  const applied = deleteWorkNoteAttachmentBlock(editor, editor.state.selection.node.attrs?.blockId, options);
  if (applied) event.preventDefault();
  return applied;
}

export function deleteWorkNoteBlockRangeAttachments(editor, { createId, removeAttachment, confirmDelete = globalThis.confirm, onError } = {}) {
  const range = getWorkNoteBlockRange(editor);
  if (!range) return false;
  const ids = attachmentIds(range.blocks.slice(range.from, range.to + 1).map(({ node }) => node));
  if (!ids.length) return deleteWorkNoteBlockRange(editor, createId);
  if (!confirmDelete?.(`선택한 블록의 첨부파일 ${ids.length}개를 함께 삭제할까요?`)) return false;
  Promise.all(ids.map((attachmentId) => removeAttachment(attachmentId)))
    .then(() => deleteWorkNoteBlockRange(editor, createId))
    .catch((error) => onError?.(error));
  return true;
}
