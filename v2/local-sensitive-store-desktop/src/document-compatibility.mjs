const legacy = new Set(['text','h1','h2','h3','h4','h5','h6','todo','bullet','number','toggle','quote','callout','code','divider','page','attachment','table']);
const nodes = new Set(['doc','paragraph','text','heading','hardBreak','horizontalRule','blockquote','codeBlock','bulletList','orderedList','listItem','taskList','taskItem','details','detailsSummary','detailsContent','callout','pageLinkBlock','attachmentBlock','userMention','table','tableRow','tableCell','tableHeader']);
const marks = new Set(['bold','italic','underline','strike','code','link','textStyle','highlight']);
export function unsupportedDocumentNodes(blocks) {
  const unsupported = new Set();
  function visit(node) {
    if (!node || typeof node !== 'object') return;
    if (!nodes.has(node.type)) unsupported.add(String(node.type || '알 수 없는 블록'));
    for (const mark of node.marks || []) if (!marks.has(mark.type)) unsupported.add(String(mark.type));
    for (const child of node.content || []) visit(child);
  }
  for (const block of blocks || []) {
    if (block.content?.type) visit(block.content);
    else if (!legacy.has(block.type)) unsupported.add(String(block.type || '알 수 없는 블록'));
  }
  return [...unsupported];
}
export function retainDocumentBlockMetadata(blocks, originals) {
  const source = new Map((originals || []).map(block => [block.id, block]));
  return (blocks || []).map(block => ({ ...structuredClone(source.get(block.id) || {}), ...block }));
}
