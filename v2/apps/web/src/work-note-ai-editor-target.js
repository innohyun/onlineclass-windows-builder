import { Fragment } from "@tiptap/pm/model";
import { findWorkNoteHandleBlock, workNoteNestedBlockTextRange } from "./work-notes-nested-blocks.js";

function topLevelRecords(editor) {
  const records = [];
  editor.state.doc.forEach((node, pos, index) => records.push({ node, pos, index }));
  return records;
}

export function captureWorkNoteAiTarget({ editor, pageId, editable, scope = "auto", blockRange = null, blockId = "" }) {
  if (!editor || !editable) return null;
  const selection = editor.state.selection;
  if ((scope === "selection" || scope === "auto") && !selection.empty && selection.$from.sameParent(selection.$to)
    && selection.$from.parent.isTextblock) {
    const text = editor.state.doc.textBetween(selection.from, selection.to, "\n");
    return text ? { kind: "text", text, nodes: [], snapshot: { pageId, kind: "text", from: selection.from, to: selection.to, source: text } } : null;
  }
  if (scope === "current" && blockId) {
    const record = findWorkNoteHandleBlock(editor.state.doc, blockId);
    const range = workNoteNestedBlockTextRange(editor.state.doc, blockId);
    if (!record) return null;
    if (range) {
      const source = editor.state.doc.textBetween(range.from, range.to, "\n");
      return { kind: "text", text: source, nodes: [], snapshot: { pageId, kind: "nested-text", blockId, source } };
    }
    const nodes = [record.node.toJSON()];
    return { kind: "blocks", text: "", nodes, snapshot: { pageId, kind: "nested-block", blockId, source: JSON.stringify(nodes) } };
  }
  const records = topLevelRecords(editor);
  let fromIndex = 0;
  let toIndex = records.length - 1;
  let kind = "page";
  if (scope !== "page") {
    if (blockRange && (scope === "blocks" || scope === "auto")) {
      fromIndex = blockRange.from;
      toIndex = blockRange.to;
      kind = "blocks";
    } else if ((scope === "selection" || scope === "auto") && !selection.empty) {
      fromIndex = selection.$from.index(0);
      toIndex = Math.min(records.length - 1, selection.$to.index(0));
      kind = "blocks";
    } else {
      fromIndex = selection.$from.index(0);
      toIndex = fromIndex;
      kind = scope === "insertion" ? "insertion" : "blocks";
    }
  }
  const nodes = records.slice(fromIndex, toIndex + 1).map(({ node }) => node.toJSON());
  return nodes.length ? { kind, text: "", nodes, snapshot: { pageId, kind, fromIndex, toIndex, source: JSON.stringify(nodes) } } : null;
}

export function applyWorkNoteAiProposal({ editor, pageId, editable, snapshot, proposal }) {
  if (!editor || !editable || snapshot?.pageId !== pageId) return false;
  if (snapshot.kind === "text") {
    const currentText = editor.state.doc.textBetween(snapshot.from, snapshot.to, "\n");
    if (currentText !== snapshot.source || typeof proposal?.text !== "string") return false;
    editor.view.dispatch(editor.state.tr.insertText(proposal.text, snapshot.from, snapshot.to).scrollIntoView());
    return true;
  }
  if (snapshot.kind === "nested-text") {
    const range = workNoteNestedBlockTextRange(editor.state.doc, snapshot.blockId);
    if (!range || editor.state.doc.textBetween(range.from, range.to, "\n") !== snapshot.source || typeof proposal?.text !== "string") return false;
    editor.view.dispatch(editor.state.tr.insertText(proposal.text, range.from, range.to).scrollIntoView());
    return true;
  }
  if (snapshot.kind === "nested-block") {
    const record = findWorkNoteHandleBlock(editor.state.doc, snapshot.blockId);
    if (!record || JSON.stringify([record.node.toJSON()]) !== snapshot.source || !Array.isArray(proposal?.nodes) || !proposal.nodes.length) return false;
    try {
      const nodes = proposal.nodes.map((node) => editor.schema.nodeFromJSON(node));
      editor.view.dispatch(editor.state.tr.replaceWith(record.pos, record.pos + record.node.nodeSize, Fragment.fromArray(nodes)).scrollIntoView());
      return true;
    } catch { return false; }
  }
  const records = topLevelRecords(editor);
  const currentNodes = records.slice(snapshot.fromIndex, snapshot.toIndex + 1).map(({ node }) => node.toJSON());
  if (JSON.stringify(currentNodes) !== snapshot.source || !Array.isArray(proposal?.nodes) || !proposal.nodes.length) return false;
  const first = records[snapshot.fromIndex];
  const last = records[snapshot.toIndex];
  if (!first || !last) return false;
  const nodes = proposal.nodes.map((node) => editor.schema.nodeFromJSON(node));
  editor.view.dispatch(editor.state.tr.replaceWith(first.pos, last.pos + last.node.nodeSize, Fragment.fromArray(nodes)).scrollIntoView());
  return true;
}
