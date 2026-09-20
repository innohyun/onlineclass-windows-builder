function inlineText(node) {
  if (!node) return "";
  if (node.type === "text") return node.text || "";
  return (node.content || []).map(inlineText).join("");
}

export function markdownTableCells(value) {
  const line = String(value || "").trim(), cells = [];
  let current = "", separatorCount = 0;
  for (let index = 0; index < line.length; index += 1) {
    if (line[index] === "\\" && line[index + 1] === "|") { current += "|"; index += 1; }
    else if (line[index] === "|") { cells.push(current.trim()); current = ""; separatorCount += 1; }
    else current += line[index];
  }
  cells.push(current.trim());
  if (!separatorCount) return null;
  if (line.startsWith("|")) cells.shift();
  if (line.endsWith("|") && !line.endsWith("\\|")) cells.pop();
  return cells.length >= 2 ? cells : null;
}

export function isMarkdownTableDivider(cells) {
  return Array.isArray(cells) && cells.length >= 2 && cells.every((cell) => /^:?-{3,}:?$/u.test(cell.replace(/\s/gu, "")));
}

function paragraph(text) {
  return text ? { type: "paragraph", content: [{ type: "text", text }] } : { type: "paragraph" };
}
function tableRow(cells, width, header = false) {
  return { type: "tableRow", content: Array.from({ length: width }, (_, index) => ({ type: header ? "tableHeader" : "tableCell", content: [paragraph(cells[index] || "")] })) };
}
function plainLegacyBlock(block) {
  if (!block || block.type !== "text" || typeof block.text !== "string") return false;
  if (!block.content) return true;
  if (block.content.type !== "paragraph") return false;
  return (block.content.content || []).every((node) => node.type === "text" && !(node.marks || []).length) && inlineText(block.content) === block.text;
}

export function normalizeLegacyWorkNoteTableBlocks(blocksValue) {
  const source = Array.isArray(blocksValue) ? blocksValue : [], blocks = [];
  let changed = false, index = 0;
  while (index < source.length) {
    const headerBlock = source[index], dividerBlock = source[index + 1];
    const header = plainLegacyBlock(headerBlock) ? markdownTableCells(headerBlock.text) : null;
    const divider = plainLegacyBlock(dividerBlock) ? markdownTableCells(dividerBlock.text) : null;
    if (!header || divider?.length !== header.length || !isMarkdownTableDivider(divider)) { blocks.push(headerBlock); index += 1; continue; }
    const rows = [tableRow(header, header.length, true)];
    index += 2;
    while (index < source.length && plainLegacyBlock(source[index])) {
      const cells = markdownTableCells(source[index].text);
      if (!cells) break;
      rows.push(tableRow(cells, header.length));
      index += 1;
    }
    const indent = Number(headerBlock.indent ?? headerBlock.content?.attrs?.indent) || 0;
    const content = { type: "table", attrs: { blockId: headerBlock.id, indent }, content: rows };
    blocks.push({ id: headerBlock.id, type: "table", text: inlineText(content), content });
    changed = true;
  }
  return { blocks, changed };
}
