import { Extension } from "@tiptap/core";
import { Fragment } from "@tiptap/pm/model";
import { Plugin, PluginKey } from "@tiptap/pm/state";
import { Decoration, DecorationSet } from "@tiptap/pm/view";

const blockRangeKey = new PluginKey("workNoteBlockRange");

export const clampBlockIndent = (value) => Math.max(0, Math.min(3, Number.parseInt(value, 10) || 0));

export function calculateBlockIndentUpdates(indents, from, to, direction) {
  const next = indents.map(clampBlockIndent);
  for (let index = from; index <= to; index += 1) {
    if (direction < 0) next[index] = Math.max(0, next[index] - 1);
    else {
      const previous = index > 0 ? next[index - 1] : -1;
      next[index] = Math.min(3, next[index] + 1, previous + 1);
    }
  }
  return next;
}

export function expandBlockRangeForDescendants(indents, from, to) {
  const base = clampBlockIndent(indents[to]);
  let end = to;
  while (end + 1 < indents.length && clampBlockIndent(indents[end + 1]) > base) end += 1;
  return { from, to: end };
}

function topLevelBlocks(doc) {
  const blocks = [];
  doc.forEach((node, pos, index) => blocks.push({ index, node, pos, indent: clampBlockIndent(node.attrs.indent) }));
  return blocks;
}

function rangeFromState(state) {
  const value = blockRangeKey.getState(state);
  if (!value) return null;
  const blocks = topLevelBlocks(state.doc);
  const anchor = blocks.findIndex(({ node }) => node.attrs.blockId === value.anchorId);
  const head = blocks.findIndex(({ node }) => node.attrs.blockId === value.headId);
  if (anchor < 0 || head < 0) return null;
  return { anchor, head, from: Math.min(anchor, head), to: Math.max(anchor, head), blocks };
}

export function createWorkNoteBlockRangeExtension(onChange) {
  return Extension.create({
    name: "workNoteBlockRange",
    addProseMirrorPlugins() {
      return [new Plugin({
        key: blockRangeKey,
        state: {
          init: () => null,
          apply(transaction, current, _oldState, newState) {
            const meta = transaction.getMeta(blockRangeKey);
            if (meta?.type === "clear") return null;
            const next = meta?.type === "set" ? { anchorId: meta.anchorId, headId: meta.headId } : current;
            if (!next) return null;
            const ids = new Set();
            newState.doc.forEach((node) => ids.add(node.attrs.blockId));
            return ids.has(next.anchorId) && ids.has(next.headId) ? next : null;
          },
        },
        props: {
          decorations(state) {
            const range = rangeFromState(state);
            if (!range) return null;
            const decorations = range.blocks.slice(range.from, range.to + 1).map(({ node, pos }, offset, selected) => Decoration.node(pos, pos + node.nodeSize, {
              class: "worknote-block-range-selected",
              "data-range-edge": selected.length === 1 ? "both" : offset === 0 ? "start" : offset === selected.length - 1 ? "end" : "middle",
            }));
            return DecorationSet.create(state.doc, decorations);
          },
        },
        view(view) {
          onChange?.(rangeFromState(view.state));
          return { update(nextView, previousState) {
            if (blockRangeKey.getState(nextView.state) !== blockRangeKey.getState(previousState)) onChange?.(rangeFromState(nextView.state));
          } };
        },
      })];
    },
  });
}

export function setWorkNoteBlockRange(editor, anchorIndex, headIndex = anchorIndex) {
  const blocks = topLevelBlocks(editor.state.doc);
  const anchor = blocks[anchorIndex];
  const head = blocks[headIndex];
  if (!anchor?.node.attrs.blockId || !head?.node.attrs.blockId) return false;
  editor.view.dispatch(editor.state.tr.setMeta(blockRangeKey, { type: "set", anchorId: anchor.node.attrs.blockId, headId: head.node.attrs.blockId }));
  return true;
}

export function clearWorkNoteBlockRange(editor) {
  if (!rangeFromState(editor.state)) return false;
  editor.view.dispatch(editor.state.tr.setMeta(blockRangeKey, { type: "clear" }));
  return true;
}

export const getWorkNoteBlockRange = (editor) => rangeFromState(editor.state);

function preserveRange(transaction, range, anchorId = range.blocks[range.anchor].node.attrs.blockId, headId = range.blocks[range.head].node.attrs.blockId) {
  return transaction.setMeta(blockRangeKey, { type: "set", anchorId, headId });
}

export function changeWorkNoteBlockIndent(editor, direction) {
  const range = rangeFromState(editor.state);
  if (!range) return false;
  const indents = range.blocks.map(({ indent }) => indent);
  const next = calculateBlockIndentUpdates(indents, range.from, range.to, direction);
  const transaction = editor.state.tr;
  for (let index = range.from; index <= range.to; index += 1) {
    if (next[index] === indents[index]) continue;
    const { node, pos } = range.blocks[index];
    transaction.setNodeMarkup(pos, undefined, { ...node.attrs, indent: next[index] }, node.marks);
  }
  if (!transaction.docChanged) return false;
  editor.view.dispatch(preserveRange(transaction, range).scrollIntoView());
  return true;
}

function cloneNodeWithNewIds(schema, node, createId) {
  const json = structuredClone(node.toJSON());
  const renew = (value) => {
    if (value.attrs?.blockId) value.attrs.blockId = createId();
    for (const child of value.content || []) renew(child);
  };
  renew(json);
  return schema.nodeFromJSON(json);
}

export function duplicateWorkNoteBlockRange(editor, createId) {
  const range = rangeFromState(editor.state);
  if (!range) return false;
  const expanded = expandBlockRangeForDescendants(range.blocks.map(({ indent }) => indent), range.from, range.to);
  const source = range.blocks.slice(expanded.from, expanded.to + 1);
  const clones = source.map(({ node }) => cloneNodeWithNewIds(editor.schema, node, createId));
  const insertAt = source.at(-1).pos + source.at(-1).node.nodeSize;
  const transaction = editor.state.tr.insert(insertAt, Fragment.fromArray(clones));
  editor.view.dispatch(transaction.setMeta(blockRangeKey, {
    type: "set",
    anchorId: clones[0].attrs.blockId,
    headId: clones.at(-1).attrs.blockId,
  }).scrollIntoView());
  return true;
}

export function deleteWorkNoteBlockRange(editor, createId) {
  const range = rangeFromState(editor.state);
  if (!range) return false;
  const first = range.blocks[range.from];
  const last = range.blocks[range.to];
  const orphanParentIndent = last.indent;
  const transaction = editor.state.tr.delete(first.pos, last.pos + last.node.nodeSize);
  if (!transaction.doc.childCount) transaction.insert(0, editor.schema.nodeFromJSON({ type: "paragraph", attrs: { blockId: createId(), indent: 0 } }));
  else {
    const remaining = topLevelBlocks(transaction.doc);
    for (let index = range.from; index < remaining.length && remaining[index].indent > orphanParentIndent; index += 1) {
      const { node, pos, indent } = remaining[index];
      transaction.setNodeMarkup(pos, undefined, { ...node.attrs, indent: Math.max(0, indent - 1) }, node.marks);
    }
  }
  editor.view.dispatch(transaction.setMeta(blockRangeKey, { type: "clear" }).scrollIntoView());
  return true;
}

export function moveWorkNoteBlockRange(editor, direction) {
  const range = rangeFromState(editor.state);
  if (!range) return false;
  const indents = range.blocks.map(({ indent }) => indent);
  const expanded = expandBlockRangeForDescendants(indents, range.from, range.to);
  const first = range.blocks[expanded.from];
  const last = range.blocks[expanded.to];
  let target = null;
  if (direction < 0) {
    if (expanded.from === 0) return false;
    const baseIndent = first.indent;
    let previous = expanded.from - 1;
    while (previous > 0 && range.blocks[previous].indent > baseIndent) previous -= 1;
    target = range.blocks[previous].pos;
  } else {
    if (expanded.to >= range.blocks.length - 1) return false;
    const nextStart = expanded.to + 1;
    const nextRange = expandBlockRangeForDescendants(indents, nextStart, nextStart);
    const nextLast = range.blocks[nextRange.to];
    target = nextLast.pos + nextLast.node.nodeSize;
  }
  const from = first.pos;
  const to = last.pos + last.node.nodeSize;
  const slice = editor.state.doc.slice(from, to);
  const transaction = editor.state.tr.delete(from, to);
  const insertAt = direction < 0 ? target : target - (to - from);
  transaction.insert(insertAt, slice.content);
  editor.view.dispatch(preserveRange(transaction, range).scrollIntoView());
  return true;
}

export function canMoveWorkNoteBlockRangeToIndex(editor, targetIndex) {
  const range = rangeFromState(editor.state);
  if (!range) return false;
  const indents = range.blocks.map(({ indent }) => indent);
  const expanded = expandBlockRangeForDescendants(indents, range.from, range.to);
  const target = Math.max(0, Math.min(range.blocks.length, Number.parseInt(targetIndex, 10) || 0));
  return target < expanded.from || target > expanded.to + 1;
}

export function moveWorkNoteBlockRangeToIndex(editor, targetIndex) {
  if (!canMoveWorkNoteBlockRangeToIndex(editor, targetIndex)) return false;
  const range = rangeFromState(editor.state);
  const indents = range.blocks.map(({ indent }) => indent);
  const expanded = expandBlockRangeForDescendants(indents, range.from, range.to);
  const target = Math.max(0, Math.min(range.blocks.length, Number.parseInt(targetIndex, 10) || 0));
  const moved = range.blocks.slice(expanded.from, expanded.to + 1);
  const first = moved[0];
  const last = moved.at(-1);
  const from = first.pos;
  const to = last.pos + last.node.nodeSize;
  const transaction = editor.state.tr.delete(from, to);
  const remaining = topLevelBlocks(transaction.doc);
  const insertIndex = target > expanded.to ? target - moved.length : target;
  const previousIndent = insertIndex > 0 ? remaining[insertIndex - 1].indent : -1;
  const allowedBaseIndent = insertIndex === 0 ? 0 : Math.min(3, previousIndent + 1);
  const indentShift = Math.min(0, allowedBaseIndent - first.indent);
  const nodes = moved.map(({ node, indent }) => node.type.create({
    ...node.attrs,
    indent: clampBlockIndent(indent + indentShift),
  }, node.content, node.marks));
  const insertAt = insertIndex >= remaining.length
    ? transaction.doc.content.size
    : remaining[insertIndex].pos;
  transaction.insert(insertAt, Fragment.fromArray(nodes));
  editor.view.dispatch(preserveRange(transaction, range).scrollIntoView());
  return true;
}
