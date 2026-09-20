import { normalizeLegacyWorkNoteTableBlocks } from "./work-notes-markdown-table-normalization.js";

export const blockId = () => globalThis.crypto?.randomUUID?.() || `block_${Date.now()}_${Math.random().toString(36).slice(2)}`;
export const textNode = (text = "") => text ? [{ type: "text", text }] : [];
export const paragraph = (text = "") => ({ type: "paragraph", content: textNode(text) });
const listItem = (text = "") => ({ type: "listItem", content: [paragraph(text)] });
export const indentValue = (value) => Math.max(0, Math.min(3, Number.parseInt(value, 10) || 0));
const previewKinds = new Set(["image", "pdf", "video", "audio"]);
const displayMode = (value, kind) => value === "file" || value === "preview" ? (value === "preview" && !previewKinds.has(kind) ? "file" : value) : (previewKinds.has(kind) ? "preview" : "file");

export function inlineText(node) {
  if (!node) return "";
  if (node.type === "text") return node.text || "";
  if (node.type === 'userMention') return `@${String(node.attrs?.label || '').trim()}`;
  return (node.content || []).map(inlineText).join(node.type === "paragraph" ? "" : " ").trim();
}

function legacyBlockToNode(block) {
  if (block.content?.type) {
    const content = structuredClone(block.content);
    content.attrs = { ...(content.attrs || {}), blockId: block.id || content.attrs?.blockId || blockId(), indent: indentValue(block.indent ?? content.attrs?.indent) };
    if (content.type === "attachmentBlock" && String(block.attachmentId || block.localAttachmentId || "").trim()) content.attrs.attachmentId = block.attachmentId || block.localAttachmentId;
    return content;
  }
  const attrs = { blockId: block.id || blockId(), indent: indentValue(block.indent) };
  const text = block.text || "";
  if (block.type === "h1" || block.type === "h2" || block.type === "h3") return { type: "heading", attrs: { ...attrs, level: Number(block.type.slice(1)) }, content: textNode(text) };
  if (block.type === "todo") return { type: "taskList", attrs, content: [{ type: "taskItem", attrs: { checked: Boolean(block.done) }, content: [paragraph(text)] }] };
  if (block.type === "bullet") return { type: "bulletList", attrs, content: [listItem(text)] };
  if (block.type === "number") return { type: "orderedList", attrs: { ...attrs, start: 1, type: null }, content: [listItem(text)] };
  if (block.type === "toggle") return { type: "details", attrs: { ...attrs, open: true }, content: [{ type: "detailsSummary", content: textNode(text || "토글") }, { type: "detailsContent", content: [paragraph("")] }] };
  if (block.type === "quote") return { type: "blockquote", attrs, content: [paragraph(text)] };
  if (block.type === "callout") return { type: "callout", attrs: { ...attrs, icon: block.icon || "💡", tone: block.tone || "purple" }, content: [paragraph(text)] };
  if (block.type === "code") return { type: "codeBlock", attrs: { ...attrs, language: null }, content: textNode(text) };
  if (block.type === "divider") return { type: "horizontalRule", attrs };
  if (block.type === "page") return { type: "pageLinkBlock", attrs: { ...attrs, pageId: block.target || "", title: text || "제목 없음" } };
  if (block.type === "image") return { type: "studentMaterialImage", attrs: { ...attrs, attachmentId: block.assetId || block.attachmentId || "", alt: block.alt || text || "학습자료 이미지" } };
  if (block.type === "attachment") { const kind = block.kind || block.attachmentType || "file"; return { type: "attachmentBlock", attrs: { ...attrs, attachmentId: block.attachmentId || block.localAttachmentId || "", fileName: block.fileName || text || "첨부파일", contentType: block.contentType || "application/octet-stream", size: Number(block.size || 0), kind, displayMode: displayMode(block.displayMode, kind) } }; }
  return { type: "paragraph", attrs, content: textNode(text) };
}

export function blocksToTiptapDocument(blocks = []) {
  const normalized = normalizeLegacyWorkNoteTableBlocks(blocks).blocks;
  const content = normalized.length ? normalized.map(legacyBlockToNode) : [paragraph("")];
  return { type: "doc", content };
}

function nodeToBlock(node) {
  const id = node.attrs?.blockId || blockId();
  const text = inlineText(node);
  const content = structuredClone(node);
  content.attrs = { ...(content.attrs || {}), blockId: id };
  const base = { id, type: "text", text, content };
  const indent = indentValue(node.attrs?.indent);
  if (indent) base.indent = indent;
  if (node.type === "heading") base.type = `h${node.attrs?.level || 1}`;
  else if (node.type === "taskList") {
    base.type = "todo";
    base.done = Boolean(node.content?.[0]?.attrs?.checked);
  } else if (node.type === "bulletList") base.type = "bullet";
  else if (node.type === "orderedList") base.type = "number";
  else if (node.type === "details") base.type = "toggle";
  else if (node.type === "blockquote") base.type = "quote";
  else if (node.type === "callout") {
    base.type = "callout";
    base.icon = node.attrs?.icon || "💡";
    base.tone = node.attrs?.tone || "purple";
  } else if (node.type === "codeBlock") base.type = "code";
  else if (node.type === "horizontalRule") base.type = "divider";
  else if (node.type === "pageLinkBlock") {
    base.type = "page";
    base.target = node.attrs?.pageId || "";
    base.text = node.attrs?.title || "제목 없음";
  } else if (node.type === "table") base.type = "table";
  else if (node.type === "studentMaterialImage") {
    base.type = "image";
    base.text = node.attrs?.alt || "학습자료 이미지";
    base.assetId = node.attrs?.attachmentId || "";
    base.alt = node.attrs?.alt || "학습자료 이미지";
  }
  else if (node.type === "attachmentBlock") {
    base.type = "attachment";
    base.text = node.attrs?.fileName || "첨부파일";
    base.attachmentId = node.attrs?.attachmentId || "";
    base.fileName = node.attrs?.fileName || "첨부파일";
    base.contentType = node.attrs?.contentType || "application/octet-stream";
    base.size = Number(node.attrs?.size || 0);
    base.kind = node.attrs?.kind || "file";
    base.displayMode = displayMode(node.attrs?.displayMode, base.kind);
  }
  return base;
}

export function tiptapDocumentToBlocks(document) {
  return (document?.content || []).filter((node) => node.type !== "attachmentBlock" || !node.attrs?.uploadState || node.attrs.uploadState === "stored").map(nodeToBlock);
}

function marksAround(text, marks = []) {
  let value = text;
  for (const mark of marks) {
    if (mark.type === "bold") value = `**${value}**`;
    else if (mark.type === "italic") value = `*${value}*`;
    else if (mark.type === "strike") value = `~~${value}~~`;
    else if (mark.type === "code") value = `\`${value}\``;
    else if (mark.type === "link") {
      const id = String(mark.attrs?.href || "").replace(/^worknote:\/\//u, "");
      value = id !== mark.attrs?.href ? `[[worknote:${id}|${text}]]` : `[${text}](${mark.attrs?.href})`;
    }
  }
  return value;
}

function inlineMarkdown(node) {
  if (node.type === "text") return marksAround(node.text || "", node.marks);
  if (node.type === "hardBreak") return "  \n";
  if (node.type === 'userMention') return `@${String(node.attrs?.label || '').trim()}`;
  return (node.content || []).map(inlineMarkdown).join("");
}

function nodeMarkdown(node, depth = 0) {
  if (node.type === "paragraph") return inlineMarkdown(node);
  if (node.type === "heading") return `${"#".repeat(node.attrs?.level || 1)} ${inlineMarkdown(node)}`;
  if (node.type === "blockquote") return node.content.map((child) => nodeMarkdown(child, depth)).join("\n").split("\n").map((line) => `> ${line}`).join("\n");
  if (node.type === "codeBlock") return `\`\`\`${node.attrs?.language || ""}\n${inlineText(node)}\n\`\`\``;
  if (node.type === "horizontalRule") return "---";
  if (node.type === "bulletList" || node.type === "orderedList") return (node.content || []).map((item, index) => `${"  ".repeat(depth)}${node.type === "bulletList" ? "-" : `${index + (node.attrs?.start || 1)}.`} ${item.content.map((child) => nodeMarkdown(child, depth + 1)).join("\n")}`).join("\n");
  if (node.type === "taskList") return (node.content || []).map((item) => `${"  ".repeat(depth)}- [${item.attrs?.checked ? "x" : " "}] ${item.content.map((child) => nodeMarkdown(child, depth + 1)).join("\n")}`).join("\n");
  if (node.type === "details") {
    const summary = inlineText(node.content?.find((child) => child.type === "detailsSummary"));
    const body = node.content?.find((child) => child.type === "detailsContent");
    return `<details>\n<summary>${summary}</summary>\n\n${(body?.content || []).map((child) => nodeMarkdown(child, depth)).join("\n\n")}\n</details>`;
  }
  if (node.type === "callout") return `> [!NOTE] ${node.attrs?.icon || "💡"}\n${(node.content || []).map((child) => `> ${nodeMarkdown(child, depth)}`).join("\n")}`;
  if (node.type === "pageLinkBlock") return `[[worknote:${node.attrs?.pageId || ""}|${node.attrs?.title || "제목 없음"}]]`;
  if (node.type === "attachmentBlock") {
    const name = String(node.attrs?.fileName || "첨부파일").replaceAll("]", "\\]");
    const target = `local-attachment://${node.attrs?.attachmentId || "missing"}`;
    return node.attrs?.kind === "image" ? `![${name}](${target})` : `[${name}](${target})`;
  }
  if (node.type === "studentMaterialImage") {
    const alt = String(node.attrs?.alt || "학습자료 이미지").replaceAll("]", "\\]");
    return `![${alt}](cloud-asset://${node.attrs?.attachmentId || "missing"})`;
  }
  if (node.type === "table") {
    const rows = (node.content || []).map((row) => row.content.map((cell) => inlineMarkdown(cell).replaceAll("|", "\\|")).join(" | "));
    if (!rows.length) return "";
    return [`| ${rows[0]} |`, `| ${node.content[0].content.map(() => "---").join(" | ")} |`, ...rows.slice(1).map((row) => `| ${row} |`)].join("\n");
  }
  return (node.content || []).map((child) => nodeMarkdown(child, depth)).join("\n");
}

export function tiptapDocumentToMarkdown(document) {
  return (document?.content || []).filter((node) => node.type !== "attachmentBlock" || !node.attrs?.uploadState || node.attrs.uploadState === "stored").map((node) => {
    const markdown = nodeMarkdown(node);
    const prefix = "    ".repeat(indentValue(node.attrs?.indent));
    return prefix ? markdown.split("\n").map((line) => `${prefix}${line}`).join("\n") : markdown;
  }).filter(Boolean).join("\n\n").trim();
}
