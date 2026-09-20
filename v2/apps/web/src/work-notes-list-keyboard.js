import { TextSelection } from "@tiptap/pm/state";

import {
  changeWorkNoteNestedBlockIndent,
  findWorkNoteHandleBlock,
  workNoteNestedBlockTextRange,
} from "./work-notes-nested-blocks.js";

const listItemTypes = new Set(["listItem", "taskItem"]);

function selectedListItemId(state) {
  if (!state?.selection?.empty) return "";
  const { $from } = state.selection;
  for (let depth = $from.depth; depth > 0; depth -= 1) {
    const node = $from.node(depth);
    if (listItemTypes.has(node.type.name)) return node.attrs?.blockId || "";
  }
  return "";
}

export function handleWorkNoteListTabKey(editor, event, createId) {
  if (event.key !== "Tab" || event.altKey || event.ctrlKey || event.metaKey || event.isComposing) return false;
  const itemId = selectedListItemId(editor?.state);
  const record = itemId ? findWorkNoteHandleBlock(editor.state.doc, itemId) : null;
  if (!record) return false;

  const range = workNoteNestedBlockTextRange(editor.state.doc, itemId);
  const cursorOffset = range ? editor.state.selection.from - range.from : 0;
  if (!changeWorkNoteNestedBlockIndent(editor, itemId, event.shiftKey ? -1 : 1, createId)) return false;

  const nextRange = workNoteNestedBlockTextRange(editor.state.doc, itemId);
  if (nextRange) {
    const cursor = Math.max(nextRange.from, Math.min(nextRange.to, nextRange.from + cursorOffset));
    editor.view.dispatch(editor.state.tr.setSelection(TextSelection.create(editor.state.doc, cursor)).scrollIntoView());
  }
  event.preventDefault();
  return true;
}
