import { Fragment } from "@tiptap/pm/model";

const directContainerTypes = new Set(["doc", "callout", "detailsContent"]);
const listTypes = new Set(["bulletList", "orderedList", "taskList"]);
const listItemTypes = new Set(["listItem", "taskItem"]);
const textBlockTypes = new Set(["paragraph", "heading"]);

const indent = (value) => Math.max(0, Math.min(3, Number.parseInt(value, 10) || 0));

export function isWorkNoteIdentityBlock(node, parent, index = 0) {
  if (!node || !parent) return false;
  if (listItemTypes.has(node.type.name)) return true;
  if (directContainerTypes.has(parent.type.name)) return node.isBlock;
  return listItemTypes.has(parent.type.name) && index > 0 && node.isBlock && !listTypes.has(node.type.name);
}

export function isWorkNoteHandleBlock(node, parent, index = 0) {
  return isWorkNoteIdentityBlock(node, parent, index)
    && !(directContainerTypes.has(parent?.type?.name) && listTypes.has(node?.type?.name));
}

export function workNoteHandleBlocks(doc) {
  const records = [];
  doc.descendants((node, pos, parent, index) => {
    if (!isWorkNoteHandleBlock(node, parent, index)) return;
    const resolved = doc.resolve(pos);
    records.push({
      id: node.attrs?.blockId || "",
      node,
      pos,
      parent,
      index,
      parentPos: resolved.depth ? resolved.before(resolved.depth) : -1,
    });
  });
  return records;
}

export function findWorkNoteHandleBlock(doc, blockId) {
  if (!blockId) return null;
  return workNoteHandleBlocks(doc).find((record) => record.id === blockId) || null;
}

export function workNoteBlockIdentityTransaction(state, createId) {
  const seen = new Set();
  const updates = [];
  state.doc.descendants((node, pos, parent, index) => {
    if (!isWorkNoteIdentityBlock(node, parent, index)) return;
    let id = String(node.attrs?.blockId || "");
    if (!id || seen.has(id)) {
      do id = String(createId()); while (!id || seen.has(id));
      updates.push({ id, node, pos });
    }
    seen.add(id);
  });
  if (!updates.length) return null;
  const transaction = state.tr;
  for (const update of updates) {
    transaction.setNodeMarkup(update.pos, undefined, { ...update.node.attrs, blockId: update.id }, update.node.marks);
  }
  return transaction;
}

function directSubtree(record) {
  if (!directContainerTypes.has(record.parent.type.name)) {
    return { from: record.pos, to: record.pos + record.node.nodeSize, nodes: [record.node], firstIndent: 0 };
  }
  const firstIndent = indent(record.node.attrs?.indent);
  const nodes = [record.node];
  let to = record.pos + record.node.nodeSize;
  for (let index = record.index + 1; index < record.parent.childCount; index += 1) {
    const node = record.parent.child(index);
    if (indent(node.attrs?.indent) <= firstIndent) break;
    nodes.push(node);
    to += node.nodeSize;
  }
  return { from: record.pos, to, nodes, firstIndent };
}

function sourceSlice(record) {
  if (!listItemTypes.has(record.node.type.name)) return { ...directSubtree(record), sourceListType: "", sourceListAttrs: null };
  if (record.parent.childCount > 1) {
    return { from: record.pos, to: record.pos + record.node.nodeSize, nodes: [record.node], firstIndent: 0, sourceListType: record.parent.type.name, sourceListAttrs: record.parent.attrs };
  }
  return {
    from: record.parentPos,
    to: record.parentPos + record.parent.nodeSize,
    nodes: [record.node],
    firstIndent: 0,
    sourceListType: record.parent.type.name,
    sourceListAttrs: record.parent.attrs,
  };
}

function renewIds(node, createId) {
  if (node.isText) return node;
  const attrs = node.attrs?.blockId ? { ...node.attrs, blockId: createId() } : node.attrs;
  const children = [];
  node.forEach((child) => children.push(renewIds(child, createId)));
  return node.type.create(attrs, children.length ? Fragment.fromArray(children) : node.content, node.marks);
}

function listAttrs(typeName, blockId, inherited = {}) {
  if (typeName === "orderedList") return { ...inherited, blockId, indent: indent(inherited.indent), start: inherited.start || 1, type: inherited.type ?? null };
  return { ...inherited, blockId, indent: indent(inherited.indent) };
}

function itemForList(schema, node, listTypeName) {
  const itemTypeName = listTypeName === "taskList" ? "taskItem" : "listItem";
  const itemType = schema.nodes[itemTypeName];
  if (!itemType) return null;
  if (listItemTypes.has(node.type.name)) {
    return itemType.create({ blockId: node.attrs?.blockId || null, ...(itemTypeName === "taskItem" ? { checked: node.type.name === "taskItem" ? Boolean(node.attrs?.checked) : false } : {}) }, node.content);
  }
  if (!textBlockTypes.has(node.type.name)) return null;
  const paragraph = schema.nodes.paragraph.create({}, node.content, node.marks);
  return itemType.create({ blockId: node.attrs?.blockId || null, ...(itemTypeName === "taskItem" ? { checked: false } : {}) }, paragraph);
}

function wrapItemForDirect(schema, item, sourceListType, sourceListAttrs, createId) {
  const typeName = listTypes.has(sourceListType) ? sourceListType : item.type.name === "taskItem" ? "taskList" : "bulletList";
  const type = schema.nodes[typeName];
  if (!type) return null;
  return type.create(listAttrs(typeName, sourceListAttrs?.blockId || createId(), sourceListAttrs || {}), item);
}

function shiftDirectIndents(nodes, nextBase) {
  const base = indent(nodes[0]?.attrs?.indent);
  return nodes.map((node) => node.type.create({ ...node.attrs, indent: indent(nextBase + indent(node.attrs?.indent) - base) }, node.content, node.marks));
}

function directInsertionNodes(editor, source, createId, nextIndent = null) {
  let nodes = source.nodes;
  if (source.sourceListType) {
    const wrapped = wrapItemForDirect(editor.schema, nodes[0], source.sourceListType, source.sourceListAttrs, createId);
    if (!wrapped) return null;
    nodes = [wrapped];
  }
  return nextIndent === null ? nodes : shiftDirectIndents(nodes, nextIndent);
}

function listInsertionNode(editor, source, listTypeName) {
  const first = source.nodes[0];
  let item = itemForList(editor.schema, first, listTypeName);
  if (!item && first?.isBlock) {
    const itemType = editor.schema.nodes[listTypeName === "taskList" ? "taskItem" : "listItem"];
    const paragraph = editor.schema.nodes.paragraph.create();
    item = itemType.create({ blockId: first.attrs?.blockId || null, ...(listTypeName === "taskList" ? { checked: false } : {}) }, [paragraph, first]);
  }
  if (!item || source.nodes.length === 1) return item;
  return item.type.create(item.attrs, item.content.append(Fragment.fromArray(source.nodes.slice(1))), item.marks);
}

function ancestorListDepth(doc, pos) {
  const resolved = doc.resolve(pos);
  let depth = 0;
  for (let index = 0; index <= resolved.depth; index += 1) if (listTypes.has(resolved.node(index).type.name)) depth += 1;
  return depth;
}

function nestedListDepth(node, depth = 0) {
  let maximum = depth;
  node.forEach((child) => { maximum = Math.max(maximum, nestedListDepth(child, depth + (listTypes.has(child.type.name) ? 1 : 0))); });
  return maximum;
}

function fitsListDepth(doc, pos, item, destinationDepth) {
  return destinationDepth + nestedListDepth(item) <= 3 && ancestorListDepth(doc, pos) <= 3;
}

function detailsContentEnd(record) {
  let found = -1;
  record.node.forEach((child, offset) => {
    if (child.type.name === "detailsContent") found = record.pos + 1 + offset + child.nodeSize - 1;
  });
  return found;
}

function targetSubtreeEnd(record) {
  if (!directContainerTypes.has(record.parent.type.name)) return record.pos + record.node.nodeSize;
  const base = indent(record.node.attrs?.indent);
  let end = record.pos + record.node.nodeSize;
  for (let index = record.index + 1; index < record.parent.childCount; index += 1) {
    const node = record.parent.child(index);
    if (indent(node.attrs?.indent) <= base) break;
    end += node.nodeSize;
  }
  return end;
}

function dispatch(editor, transaction) {
  if (!transaction.docChanged) return false;
  editor.view.dispatch(transaction.scrollIntoView());
  return true;
}

function workNoteNestedBlockMoveTransaction(editor, sourceId, targetId, mode, createId) {
  const sourceRecord = findWorkNoteHandleBlock(editor.state.doc, sourceId);
  const targetRecord = findWorkNoteHandleBlock(editor.state.doc, targetId);
  if (!sourceRecord || !targetRecord || sourceId === targetId || !["before", "inside", "after"].includes(mode)) return null;
  const source = sourceSlice(sourceRecord);
  if (targetRecord.pos >= source.from && targetRecord.pos < source.to) return null;

  const transaction = editor.state.tr.delete(source.from, source.to);
  const target = findWorkNoteHandleBlock(transaction.doc, targetId);
  if (!target) return null;

  if (mode === "inside" && listItemTypes.has(target.node.type.name)) {
    const listTypeName = target.parent.type.name;
    const item = listInsertionNode(editor, source, listTypeName);
    const destinationDepth = ancestorListDepth(transaction.doc, target.pos) + 1;
    if (!item || !fitsListDepth(transaction.doc, target.pos, item, destinationDepth)) return null;
    let nested = null;
    let nestedOffset = 0;
    target.node.forEach((child, offset) => {
      if (child.type.name === listTypeName) { nested = child; nestedOffset = offset; }
    });
    if (nested) transaction.insert(target.pos + 1 + nestedOffset + nested.nodeSize - 1, item);
    else {
      const list = editor.schema.nodes[listTypeName].create(listAttrs(listTypeName, createId()), item);
      transaction.insert(target.pos + target.node.nodeSize - 1, list);
    }
    return transaction;
  }

  if (mode === "inside" && target.node.type.name === "callout") {
    const nodes = directInsertionNodes(editor, source, createId, 0);
    if (!nodes) return null;
    transaction.insert(target.pos + target.node.nodeSize - 1, Fragment.fromArray(nodes));
    return transaction;
  }

  if (mode === "inside" && target.node.type.name === "details") {
    const at = detailsContentEnd(target);
    const nodes = directInsertionNodes(editor, source, createId, 0);
    if (at < 0 || !nodes) return null;
    transaction.insert(at, Fragment.fromArray(nodes));
    return transaction;
  }

  if (listTypes.has(target.parent.type.name)) {
    if (mode === "inside") return null;
    const item = listInsertionNode(editor, source, target.parent.type.name);
    const destinationDepth = ancestorListDepth(transaction.doc, target.pos);
    if (!item || !fitsListDepth(transaction.doc, target.pos, item, destinationDepth)) return null;
    transaction.insert(mode === "before" ? target.pos : target.pos + target.node.nodeSize, item);
    return transaction;
  }

  if (!directContainerTypes.has(target.parent.type.name) && !listItemTypes.has(target.parent.type.name)) return null;
  const nextIndent = mode === "inside" ? Math.min(3, indent(target.node.attrs?.indent) + 1) : indent(target.node.attrs?.indent);
  const nodes = directInsertionNodes(editor, source, createId, nextIndent);
  if (!nodes) return null;
  const at = mode === "before" ? target.pos : targetSubtreeEnd(target);
  transaction.insert(at, Fragment.fromArray(nodes));
  return transaction;
}

export function canMoveWorkNoteNestedBlock(editor, sourceId, targetId, mode, createId) {
  return Boolean(workNoteNestedBlockMoveTransaction(editor, sourceId, targetId, mode, createId));
}

export function moveWorkNoteNestedBlock(editor, sourceId, targetId, mode, createId) {
  const transaction = workNoteNestedBlockMoveTransaction(editor, sourceId, targetId, mode, createId);
  return transaction ? dispatch(editor, transaction) : false;
}

export function moveWorkNoteNestedBlockByDirection(editor, blockId, direction, createId) {
  if (![-1, 1].includes(direction)) return false;
  const record = findWorkNoteHandleBlock(editor.state.doc, blockId);
  if (!record) return false;
  const siblings = workNoteHandleBlocks(editor.state.doc).filter((candidate) => candidate.parent === record.parent);
  const index = siblings.findIndex((candidate) => candidate.id === blockId);
  const target = siblings[index + direction];
  if (!target) return false;
  return moveWorkNoteNestedBlock(editor, blockId, target.id, direction < 0 ? "before" : "after", createId);
}

function previousAdjacentListItemId(editor, record) {
  if (record.index > 0) return record.parent.child(record.index - 1).attrs?.blockId || "";
  const resolved = editor.state.doc.resolve(record.parentPos);
  const wrapperIndex = resolved.index();
  if (wrapperIndex < 1) return "";
  const previousWrapper = resolved.parent.child(wrapperIndex - 1);
  if (previousWrapper.type !== record.parent.type
    || indent(previousWrapper.attrs?.indent) !== indent(record.parent.attrs?.indent)
    || previousWrapper.childCount === 0) return "";
  return previousWrapper.lastChild?.attrs?.blockId || "";
}

export function canWorkNoteNestedBlockAction(editor, blockId, action, createId) {
  const record = findWorkNoteHandleBlock(editor.state.doc, blockId);
  if (!record) return false;
  if (record.node.type.name === "attachmentBlock" && ["ai", "duplicate"].includes(action)) return false;
  if (["ai", "move", "duplicate", "delete"].includes(action)) return true;
  if (action === "up" || action === "down") {
    const direction = action === "up" ? -1 : 1;
    const siblings = workNoteHandleBlocks(editor.state.doc).filter((candidate) => candidate.parent === record.parent);
    const index = siblings.findIndex((candidate) => candidate.id === blockId);
    const target = siblings[index + direction];
    return Boolean(target && canMoveWorkNoteNestedBlock(editor, blockId, target.id, direction < 0 ? "before" : "after", createId));
  }
  if (action === "indent" || action === "outdent") {
    const direction = action === "indent" ? 1 : -1;
    if (listItemTypes.has(record.node.type.name)) {
      if (direction > 0) {
        const previousId = previousAdjacentListItemId(editor, record);
        return Boolean(previousId && canMoveWorkNoteNestedBlock(editor, blockId, previousId, "inside", createId));
      }
      const resolved = editor.state.doc.resolve(record.pos);
      for (let depth = resolved.depth - 1; depth > 0; depth -= 1) {
        const ancestor = resolved.node(depth);
        if (listItemTypes.has(ancestor.type.name) && ancestor.attrs?.blockId) {
          return canMoveWorkNoteNestedBlock(editor, blockId, ancestor.attrs.blockId, "after", createId);
        }
      }
      return false;
    }
    const current = indent(record.node.attrs?.indent);
    return direction > 0 ? record.index > 0 && current < 3 : current > 0;
  }
  const listTypeName = convertedListType(action);
  if (listItemTypes.has(record.node.type.name)) return Boolean(listTypeName || action === "text" || /^h[1-3]$/u.test(action));
  if (textBlockTypes.has(record.node.type.name)) return Boolean(listTypeName || action === "text" || /^h[1-3]$/u.test(action));
  return false;
}

export function duplicateWorkNoteNestedBlock(editor, blockId, createId) {
  const record = findWorkNoteHandleBlock(editor.state.doc, blockId);
  if (!record) return false;
  const source = listItemTypes.has(record.node.type.name)
    ? { from: record.pos, to: record.pos + record.node.nodeSize, nodes: [record.node] }
    : sourceSlice(record);
  const clones = source.nodes.map((node) => renewIds(node, createId));
  return dispatch(editor, editor.state.tr.insert(source.to, Fragment.fromArray(clones)));
}

export function deleteWorkNoteNestedBlock(editor, blockId, createId) {
  const record = findWorkNoteHandleBlock(editor.state.doc, blockId);
  if (!record) return false;
  const source = sourceSlice(record);
  const onlyDirectChild = directContainerTypes.has(record.parent.type.name) && record.parent.childCount === source.nodes.length;
  if (onlyDirectChild) {
    const replacement = editor.schema.nodes.paragraph.create({ blockId: createId(), indent: 0 });
    return dispatch(editor, editor.state.tr.replaceWith(source.from, source.to, replacement));
  }
  const transaction = editor.state.tr.delete(source.from, source.to);
  if (!transaction.doc.childCount) transaction.insert(0, editor.schema.nodes.paragraph.create({ blockId: createId(), indent: 0 }));
  return dispatch(editor, transaction);
}

export function insertWorkNoteNestedBlockAfter(editor, blockId, createId) {
  const record = findWorkNoteHandleBlock(editor.state.doc, blockId);
  if (!record) return null;
  const id = createId();
  let node;
  let at;
  if (listTypes.has(record.parent.type.name)) {
    const paragraph = editor.schema.nodes.paragraph.create({}, editor.schema.text("/"));
    const type = record.parent.type.name === "taskList" ? editor.schema.nodes.taskItem : editor.schema.nodes.listItem;
    node = type.create({ blockId: id, ...(record.parent.type.name === "taskList" ? { checked: false } : {}) }, paragraph);
    at = record.pos + record.node.nodeSize;
  } else {
    node = editor.schema.nodes.paragraph.create({ blockId: id, indent: indent(record.node.attrs?.indent) }, editor.schema.text("/"));
    at = targetSubtreeEnd(record);
  }
  return dispatch(editor, editor.state.tr.insert(at, node)) ? id : null;
}

export function changeWorkNoteNestedBlockIndent(editor, blockId, direction, createId) {
  const record = findWorkNoteHandleBlock(editor.state.doc, blockId);
  if (!record || ![-1, 1].includes(direction)) return false;
  if (listItemTypes.has(record.node.type.name)) {
    if (direction > 0) {
      const previousId = previousAdjacentListItemId(editor, record);
      return previousId ? moveWorkNoteNestedBlock(editor, blockId, previousId, "inside", createId) : false;
    }
    const resolved = editor.state.doc.resolve(record.pos);
    for (let depth = resolved.depth - 1; depth > 0; depth -= 1) {
      const ancestor = resolved.node(depth);
      if (!listItemTypes.has(ancestor.type.name) || !ancestor.attrs?.blockId) continue;
      return moveWorkNoteNestedBlock(editor, blockId, ancestor.attrs.blockId, "after", createId);
    }
    return false;
  }
  const source = directSubtree(record);
  const current = source.nodes.map((node) => indent(node.attrs?.indent));
  const nextBase = indent(current[0] + direction);
  if (nextBase === current[0] || (direction > 0 && record.index === 0)) return false;
  const transaction = editor.state.tr;
  let pos = source.from;
  source.nodes.forEach((node, index) => {
    transaction.setNodeMarkup(pos, undefined, { ...node.attrs, indent: indent(nextBase + current[index] - current[0]) }, node.marks);
    pos += node.nodeSize;
  });
  return dispatch(editor, transaction);
}

function standaloneNode(editor, item, typeName) {
  const first = item.firstChild;
  const content = first?.content || Fragment.empty;
  if (typeName === "text") return editor.schema.nodes.paragraph.create({ blockId: item.attrs?.blockId || null, indent: 0 }, content);
  if (/^h[1-3]$/u.test(typeName)) return editor.schema.nodes.heading.create({ blockId: item.attrs?.blockId || null, indent: 0, level: Number(typeName.slice(1)) }, content);
  return null;
}

function convertedListType(action) {
  return ({ bullet: "bulletList", number: "orderedList", todo: "taskList" })[action] || "";
}

export function transformWorkNoteNestedBlock(editor, blockId, action, createId) {
  const record = findWorkNoteHandleBlock(editor.state.doc, blockId);
  if (!record) return false;
  const listTypeName = convertedListType(action);

  if (listItemTypes.has(record.node.type.name)) {
    if (listTypeName) {
      const item = itemForList(editor.schema, record.node, listTypeName);
      if (!item) return false;
      if (record.parent.childCount === 1) {
        const wrapper = editor.schema.nodes[listTypeName].create(listAttrs(listTypeName, record.parent.attrs?.blockId || createId(), record.parent.attrs), item);
        return dispatch(editor, editor.state.tr.replaceWith(record.parentPos, record.parentPos + record.parent.nodeSize, wrapper));
      }
      const replacements = [];
      const before = [];
      const after = [];
      record.parent.forEach((child, _offset, index) => (index < record.index ? before : index > record.index ? after : null)?.push(child));
      if (before.length) replacements.push(record.parent.type.create(record.parent.attrs, Fragment.fromArray(before)));
      replacements.push(editor.schema.nodes[listTypeName].create(listAttrs(listTypeName, createId()), item));
      if (after.length) replacements.push(record.parent.type.create(listAttrs(record.parent.type.name, createId(), record.parent.attrs), Fragment.fromArray(after)));
      return dispatch(editor, editor.state.tr.replaceWith(record.parentPos, record.parentPos + record.parent.nodeSize, Fragment.fromArray(replacements)));
    }
    const standalone = standaloneNode(editor, record.node, action);
    if (!standalone) return false;
    const replacements = [];
    const before = [];
    const after = [];
    record.parent.forEach((child, _offset, index) => (index < record.index ? before : index > record.index ? after : null)?.push(child));
    if (before.length) replacements.push(record.parent.type.create(record.parent.attrs, Fragment.fromArray(before)));
    replacements.push(standalone);
    record.node.forEach((child, _offset, index) => { if (index > 0) replacements.push(child); });
    if (after.length) replacements.push(record.parent.type.create(listAttrs(record.parent.type.name, createId(), record.parent.attrs), Fragment.fromArray(after)));
    return dispatch(editor, editor.state.tr.replaceWith(record.parentPos, record.parentPos + record.parent.nodeSize, Fragment.fromArray(replacements)));
  }

  if (listTypes.has(record.node.type.name) && listTypeName) {
    const items = [];
    record.node.forEach((child) => {
      const item = itemForList(editor.schema, child, listTypeName);
      if (item) items.push(item);
    });
    if (!items.length) return false;
    const wrapper = editor.schema.nodes[listTypeName].create(listAttrs(listTypeName, record.node.attrs?.blockId || createId(), record.node.attrs), Fragment.fromArray(items));
    return dispatch(editor, editor.state.tr.replaceWith(record.pos, record.pos + record.node.nodeSize, wrapper));
  }

  if (listTypeName && textBlockTypes.has(record.node.type.name)) {
    const item = itemForList(editor.schema, record.node, listTypeName);
    const wrapper = item && editor.schema.nodes[listTypeName].create(listAttrs(listTypeName, createId(), { indent: record.node.attrs?.indent }), item);
    return wrapper ? dispatch(editor, editor.state.tr.replaceWith(record.pos, record.pos + record.node.nodeSize, wrapper)) : false;
  }

  if ((action === "text" || /^h[1-3]$/u.test(action)) && textBlockTypes.has(record.node.type.name)) {
    const type = action === "text" ? editor.schema.nodes.paragraph : editor.schema.nodes.heading;
    const attrs = action === "text"
      ? { blockId: record.node.attrs?.blockId || null, indent: indent(record.node.attrs?.indent) }
      : { blockId: record.node.attrs?.blockId || null, indent: indent(record.node.attrs?.indent), level: Number(action.slice(1)) };
    return dispatch(editor, editor.state.tr.setNodeMarkup(record.pos, type, attrs, record.node.marks));
  }
  return false;
}

export function workNoteNestedBlockTextRange(doc, blockId) {
  const record = findWorkNoteHandleBlock(doc, blockId);
  if (!record) return null;
  let found = null;
  record.node.descendants((node, relativePos) => {
    if (!found && node.isTextblock) found = { from: record.pos + 1 + relativePos + 1, to: record.pos + 1 + relativePos + 1 + node.content.size };
  });
  return found;
}
