const SAFE_LINK_PROTOCOLS = new Set(['http:', 'https:', 'mailto:', 'tel:']);

function blockId(prefix, sourceId, index) {
  return `${prefix}-${sourceId}-${index + 1}`;
}

function sameMarks(left = [], right = []) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function appendText(nodes, text, marks = []) {
  if (!text) return;
  const previous = nodes.at(-1);
  if (previous?.type === 'text' && sameMarks(previous.marks, marks)) {
    previous.text += text;
    return;
  }
  nodes.push({ type: 'text', text, ...(marks.length ? { marks } : {}) });
}

function closingIndex(source, marker, start) {
  const index = source.indexOf(marker, start + marker.length);
  return index > start + marker.length ? index : -1;
}

function safeLink(value) {
  try {
    const url = new URL(String(value || '').trim());
    return SAFE_LINK_PROTOCOLS.has(url.protocol) ? url.toString() : '';
  } catch {
    return '';
  }
}

function inlineContent(value, inheritedMarks = [], depth = 0) {
  const source = String(value || '');
  const nodes = [];
  if (!source) return nodes;
  if (depth > 8) {
    appendText(nodes, source, inheritedMarks);
    return nodes;
  }
  let index = 0;
  let plain = '';
  const flush = () => {
    appendText(nodes, plain, inheritedMarks);
    plain = '';
  };
  const appendNested = (text, marks) => nodes.push(...inlineContent(text, [...inheritedMarks, ...marks], depth + 1));

  while (index < source.length) {
    const rest = source.slice(index);
    if (rest[0] === '\\' && /[\\`*_[\]{}()#+.!>|~-]/u.test(rest[1] || '')) {
      plain += rest[1];
      index += 2;
      continue;
    }
    if (rest.startsWith('`')) {
      const close = source.indexOf('`', index + 1);
      if (close > index + 1) {
        flush();
        appendText(nodes, source.slice(index + 1, close), [...inheritedMarks, { type: 'code' }]);
        index = close + 1;
        continue;
      }
    }
    if (rest.startsWith('**') || rest.startsWith('__')) {
      const marker = rest.startsWith('**') ? '**' : '__';
      const close = closingIndex(source, marker, index);
      if (close > -1) {
        flush();
        appendNested(source.slice(index + marker.length, close), [{ type: 'bold' }]);
        index = close + marker.length;
        continue;
      }
    }
    if (rest.startsWith('~~')) {
      const close = closingIndex(source, '~~', index);
      if (close > -1) {
        flush();
        appendNested(source.slice(index + 2, close), [{ type: 'strike' }]);
        index = close + 2;
        continue;
      }
    }
    if (rest.startsWith('*') || rest.startsWith('_')) {
      const marker = rest[0];
      const close = closingIndex(source, marker, index);
      if (close > -1) {
        flush();
        appendNested(source.slice(index + 1, close), [{ type: 'italic' }]);
        index = close + 1;
        continue;
      }
    }
    if (rest.startsWith('[[')) {
      const close = source.indexOf(']]', index + 2);
      if (close > index + 2) {
        flush();
        const raw = source.slice(index + 2, close);
        const separator = raw.indexOf('|');
        const target = (separator < 0 ? raw : raw.slice(0, separator)).trim();
        const label = (separator < 0 ? raw : raw.slice(separator + 1)).trim() || '연결된 페이지';
        if (target.startsWith('worknote:') && target.slice('worknote:'.length)) {
          appendNested(label, [{
            type: 'link',
            attrs: {
              href: `worknote://${target.slice('worknote:'.length)}`,
              title: label,
              target: null,
              rel: null,
              class: 'internal-page-link',
            },
          }]);
        } else {
          appendNested(label, []);
        }
        index = close + 2;
        continue;
      }
    }
    if (rest.startsWith('[')) {
      const closeLabel = source.indexOf(']', index + 1);
      const openHref = closeLabel > -1 ? source.indexOf('(', closeLabel) : -1;
      let closeHref = -1;
      let nesting = 1;
      if (openHref > -1) for (let cursor = openHref + 1; cursor < source.length; cursor += 1) {
        if (source[cursor] === '\\') { cursor += 1; continue; }
        if (source[cursor] === '(') nesting += 1;
        if (source[cursor] === ')' && --nesting === 0) { closeHref = cursor; break; }
      }
      if (closeLabel > index + 1 && openHref === closeLabel + 1 && closeHref > openHref + 1) {
        flush();
        const label = source.slice(index + 1, closeLabel);
        const href = safeLink(source.slice(openHref + 1, closeHref));
        if (href) {
          appendNested(label, [{ type: 'link', attrs: { href, target: '_blank', rel: 'noopener noreferrer', class: null } }]);
        } else {
          appendNested(label, []);
        }
        index = closeHref + 1;
        continue;
      }
    }
    plain += source[index];
    index += 1;
  }
  flush();
  return nodes;
}

function inlineText(node) {
  if (!node) return '';
  if (node.type === 'text') return node.text || '';
  return (node.content || []).map(inlineText).join('');
}

function paragraph(text) {
  const content = inlineContent(text);
  return { type: 'paragraph', ...(content.length ? { content } : {}) };
}

function listLine(value) {
  const line = String(value || '').replace(/\t/gu, '  ');
  let match = line.match(/^(\s*)[-*+]\s+\[([ xX])\]\s*(.*)$/u);
  if (match) return {
    indent: match[1].length, kind: 'task', checked: match[2].toLowerCase() === 'x', text: match[3],
  };
  match = line.match(/^(\s*)[-*+]\s+(.*)$/u);
  if (match) return { indent: match[1].length, kind: 'bullet', text: match[2] };
  match = line.match(/^(\s*)(\d+)\.\s+(.*)$/u);
  return match ? { indent: match[1].length, kind: 'ordered', start: Number(match[2]), text: match[3] } : null;
}

function parseList(lines, startIndex, baseIndent, kind, strict = false) {
  const items = [];
  let index = startIndex;
  let start = 1;
  while (index < lines.length) {
    const item = listLine(lines[index]);
    if (!item || item.indent !== baseIndent || item.kind !== kind) break;
    if (!items.length && kind === 'ordered') start = item.start;
    const children = [paragraph(item.text)];
    index += 1;
    while (index < lines.length) {
      const child = listLine(lines[index]);
      if (!child) {
        if (strict && lines[index].trim() && (lines[index].match(/^\s*/u)?.[0].length || 0) > baseIndent) {
          children.push(paragraph(lines[index].trim()));
          index += 1;
          continue;
        }
        break;
      }
      if (child.indent <= baseIndent) break;
      const nested = parseList(lines, index, child.indent, child.kind, strict);
      children.push(nested.node);
      index = nested.index;
    }
    items.push(kind === 'task'
      ? { type: 'taskItem', attrs: { checked: item.checked }, content: children }
      : { type: 'listItem', content: children });
  }
  if (kind === 'task') return { node: { type: 'taskList', content: items }, index, blockType: 'todo' };
  if (kind === 'ordered') return {
    node: { type: 'orderedList', attrs: { start, type: null }, content: items }, index, blockType: 'number',
  };
  return { node: { type: 'bulletList', content: items }, index, blockType: 'bullet' };
}

function tableCells(value) {
  const line = String(value || '').trim();
  const cells = [];
  let current = '';
  let separatorCount = 0;
  for (let index = 0; index < line.length; index += 1) {
    if (line[index] === '\\' && line[index + 1] === '|') {
      current += '|';
      index += 1;
    } else if (line[index] === '|') {
      cells.push(current.trim());
      current = '';
      separatorCount += 1;
    } else current += line[index];
  }
  cells.push(current.trim());
  if (!separatorCount) return null;
  if (line.startsWith('|')) cells.shift();
  if (line.endsWith('|') && !line.endsWith('\\|')) cells.pop();
  return cells.length >= 2 ? cells : null;
}

function isTableDivider(cells) {
  return Array.isArray(cells) && cells.length >= 2
    && cells.every((cell) => /^:?-{3,}:?$/u.test(cell.replace(/\s/gu, '')));
}

function normalizedTableCells(cells, width) {
  return Array.from({ length: width }, (_, index) => cells[index] || '');
}

function markdownTableAt(lines, startIndex, strict = false) {
  const headerCells = tableCells(lines[startIndex]);
  const dividerCells = tableCells(lines[startIndex + 1]);
  if (!headerCells || dividerCells?.length !== headerCells.length || !isTableDivider(dividerCells)) return null;
  const alignments = strict ? dividerCells.map((value) => value.startsWith(':') ? value.endsWith(':') ? 'center' : 'left' : value.endsWith(':') ? 'right' : null) : [];
  const rows = [tableRow(headerCells, true, alignments)];
  let index = startIndex + 2;
  while (index < lines.length) {
    const cells = tableCells(lines[index]);
    if (!cells) break;
    if (strict && cells.length !== headerCells.length) markdownInvalid('표의 모든 행은 머리글과 같은 셀 수를 사용해야 합니다.');
    rows.push(tableRow(normalizedTableCells(cells, headerCells.length), false, alignments));
    index += 1;
  }
  return { rows, index };
}

function tableRow(cells, header = false, alignments = []) {
  return {
    type: 'tableRow',
    content: cells.map((cell, index) => ({
      type: header ? 'tableHeader' : 'tableCell',
      content: [{ ...paragraph(cell), ...(alignments[index] ? { attrs: { textAlign: alignments[index] } } : {}) }],
    })),
  };
}

function projectedBlock(prefix, sourceId, index, type, content, extras = {}) {
  const id = blockId(prefix, sourceId, index);
  const node = structuredClone(content);
  node.attrs = { ...(node.attrs || {}), blockId: id, indent: 0 };
  return { id, type, text: inlineText(node), ...extras, content: node };
}

function projectMarkdown(sourceIdValue, markdownValue, prefix) {
  const sourceId = String(sourceIdValue || '').trim();
  const markdown = String(markdownValue || '').replace(/\r/gu, '');
  const lines = markdown.split('\n');
  const blocks = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (/^```/u.test(line)) {
      const language = line.replace(/^```/u, '').trim() || null;
      const code = [];
      index += 1;
      while (index < lines.length && !/^```/u.test(lines[index])) {
        code.push(lines[index]);
        index += 1;
      }
      if (index < lines.length) index += 1;
      const text = code.join('\n');
      blocks.push(projectedBlock(prefix, sourceId, blocks.length, 'code', {
        type: 'codeBlock', attrs: { language }, ...(text ? { content: [{ type: 'text', text }] } : {}),
      }));
      continue;
    }
    const table = markdownTableAt(lines, index);
    if (table) {
      blocks.push(projectedBlock(prefix, sourceId, blocks.length, 'table', {
        type: 'table', content: table.rows,
      }));
      index = table.index;
      continue;
    }
    let match;
    if ((match = line.match(/^(#{1,6})\s+(.*)$/u))) {
      const level = Math.min(3, match[1].length);
      blocks.push(projectedBlock(prefix, sourceId, blocks.length, `h${level}`, {
        type: 'heading', attrs: { level }, content: inlineContent(match[2]),
      }));
    } else if ((match = line.match(/^\s*[-*+]\s+\[([ xX])\]\s*(.*)$/u))) {
      const done = match[1].toLowerCase() === 'x';
      blocks.push(projectedBlock(prefix, sourceId, blocks.length, 'todo', {
        type: 'taskList',
        content: [{ type: 'taskItem', attrs: { checked: done }, content: [paragraph(match[2])] }],
      }, { done }));
    } else if ((match = line.match(/^\s*[-*+]\s+(.*)$/u))) {
      blocks.push(projectedBlock(prefix, sourceId, blocks.length, 'bullet', {
        type: 'bulletList', content: [{ type: 'listItem', content: [paragraph(match[1])] }],
      }));
    } else if ((match = line.match(/^\s*\d+\.\s+(.*)$/u))) {
      blocks.push(projectedBlock(prefix, sourceId, blocks.length, 'number', {
        type: 'orderedList', attrs: { start: 1, type: null }, content: [{ type: 'listItem', content: [paragraph(match[1])] }],
      }));
    } else if ((match = line.match(/^\s*>\s?(.*)$/u))) {
      blocks.push(projectedBlock(prefix, sourceId, blocks.length, 'quote', {
        type: 'blockquote', content: [paragraph(match[1])],
      }));
    } else if (/^\s*(?:[-*_]\s*){3,}$/u.test(line)) {
      blocks.push(projectedBlock(prefix, sourceId, blocks.length, 'divider', { type: 'horizontalRule' }));
    } else if (line.trim()) {
      blocks.push(projectedBlock(prefix, sourceId, blocks.length, 'text', paragraph(line)));
    }
    index += 1;
  }
  if (!blocks.length) blocks.push(projectedBlock(prefix, sourceId, 0, 'text', paragraph('')));
  return { blocks, markdown };
}

export function projectWorkNoteMarkdown(sourceIdValue, markdownValue, { strict = false } = {}) {
  const sourceId = String(sourceIdValue || '').trim();
  const markdown = String(markdownValue || '').replace(/\r/gu, '');
  const lines = markdown.split('\n');
  const blocks = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (/^```/u.test(line)) {
      const language = line.slice(3).trim() || null;
      const code = [];
      index += 1;
      while (index < lines.length && !/^```\s*$/u.test(lines[index])) {
        code.push(lines[index]);
        index += 1;
      }
      if (strict && index >= lines.length) markdownInvalid('코드 블록의 닫는 ```가 필요합니다.');
      if (index < lines.length) index += 1;
      const text = code.join('\n');
      blocks.push(projectedBlock('work-note-body', sourceId, blocks.length, 'code', {
        type: 'codeBlock', attrs: { language }, ...(text ? { content: [{ type: 'text', text }] } : {}),
      }));
      continue;
    }
    if (strict && line.trim() === '<details>') {
      const summary = lines[index + 1]?.match(/^<summary>(.*)<\/summary>$/u);
      if (!summary) markdownInvalid('toggle은 <details> 다음 줄에 <summary>제목</summary>이 필요합니다.');
      let end = index + 2;
      let depth = 1;
      for (; end < lines.length; end += 1) {
        if (lines[end].trim() === '<details>') depth += 1;
        if (lines[end].trim() === '</details>' && --depth === 0) break;
      }
      if (end >= lines.length) markdownInvalid('toggle의 닫는 </details>가 필요합니다.');
      const children = projectWorkNoteMarkdown(`${sourceId}-${blocks.length}`, lines.slice(index + 2, end).join('\n'), { strict });
      blocks.push(projectedBlock('work-note-body', sourceId, blocks.length, 'toggle', {
        type: 'details', attrs: { open: true }, content: [
          { type: 'detailsSummary', content: inlineContent(summary[1]) },
          { type: 'detailsContent', content: children.blocks.map((block) => block.content) },
        ],
      }));
      index = end + 1;
      continue;
    }
    if (strict && /^\s*>/u.test(line)) {
      const quoted = [];
      while (index < lines.length && /^\s*>/u.test(lines[index])) {
        quoted.push(lines[index++].replace(/^\s*> ?/u, ''));
      }
      const callout = quoted[0].match(/^\[!NOTE\](?:\s+(.*))?$/u);
      if (callout) quoted.shift();
      const children = projectWorkNoteMarkdown(`${sourceId}-${blocks.length}`, quoted.join('\n'), { strict });
      blocks.push(projectedBlock('work-note-body', sourceId, blocks.length, callout ? 'callout' : 'quote', {
        type: callout ? 'callout' : 'blockquote',
        ...(callout ? { attrs: { icon: callout[1] || '💡', tone: 'purple' } } : {}),
        content: children.blocks.map((block) => block.content),
      }));
      continue;
    }
    if (strict && (/<\/?[A-Za-z][^>]*>/u.test(line) || /!\[[^\]]*\]\(/u.test(line))) {
      markdownInvalid('HTML/이미지는 자동 변환하지 않습니다. 기존 보호 block을 보존하거나 전용 이미지 도구를 사용해 주세요.');
    }
    if (strict && /\]\((?:local-attachment|cloud-asset):\/\//u.test(line)) {
      markdownInvalid('첨부 URL을 재작성하지 말고 기존 보호 block을 보존해 주세요.');
    }
    const table = markdownTableAt(lines, index, strict);
    if (table) {
      blocks.push(projectedBlock('work-note-body', sourceId, blocks.length, 'table', {
        type: 'table', content: table.rows,
      }));
      index = table.index;
      continue;
    }
    const firstListItem = listLine(line);
    if (firstListItem) {
      const parsed = parseList(lines, index, firstListItem.indent, firstListItem.kind, strict);
      blocks.push(projectedBlock('work-note-body', sourceId, blocks.length, parsed.blockType, parsed.node));
      index = parsed.index;
      continue;
    }
    let match;
    if ((match = line.match(/^(#{1,6})\s+(.*)$/u))) {
      if (strict && match[1].length > 3) markdownInvalid('편집기가 지원하는 제목은 #, ##, ###입니다. 제목 단계를 명시적으로 조정해 주세요.');
      const level = Math.min(3, match[1].length);
      blocks.push(projectedBlock('work-note-body', sourceId, blocks.length, `h${level}`, {
        type: 'heading', attrs: { level }, content: inlineContent(match[2]),
      }));
    } else if ((match = line.match(/^\s*>\s?(.*)$/u))) {
      blocks.push(projectedBlock('work-note-body', sourceId, blocks.length, 'quote', {
        type: 'blockquote', content: [paragraph(match[1])],
      }));
    } else if (/^\s*(?:[-*_]\s*){3,}$/u.test(line)) {
      blocks.push(projectedBlock('work-note-body', sourceId, blocks.length, 'divider', { type: 'horizontalRule' }));
    } else if (line.trim()) {
      blocks.push(projectedBlock('work-note-body', sourceId, blocks.length, 'text', paragraph(line)));
    }
    index += 1;
  }
  if (!blocks.length) blocks.push(projectedBlock('work-note-body', sourceId, 0, 'text', paragraph('')));
  return { blocks, markdown };
}

function markdownInvalid(message) {
  const error = new Error(message);
  error.code = 'DOCUMENT_MARKDOWN_UNSUPPORTED';
  error.status = 400;
  throw error;
}

// Complete authored content is never summarized, clipped, or converted through
// the approval-card summary. Legacy callers keep their permissive import path.
export function projectDocumentMarkdown(sourceId, markdown) {
  if (typeof markdown !== 'string' || !markdown.trim() || markdown.length > 200_000) {
    markdownInvalid('완성 본문 Markdown은 비어 있지 않은 200,000자 이하 문자열이어야 합니다. 분할 편집을 사용해 주세요.');
  }
  let fenced = false;
  for (const line of markdown.replace(/\r/gu, '').split('\n')) {
    if (/^```/u.test(line)) { fenced = !fenced; continue; }
    if (fenced) continue;
    const syntax = line.trim().replace(/^>\s*/u, '');
    if (/^#{4,}\s/u.test(syntax) || /^~~~/u.test(syntax)
      || /!\[[^\]]*\]\(/u.test(syntax)
      || /\[[^\]]*\]\[(?:[^\]]*)\]/u.test(syntax)
      || /^\[[^\]]+\]:/u.test(syntax)) markdownInvalid('지원하지 않는 Markdown 구조입니다. 제목 #~###와 명시적인 본문/링크/전용 이미지 도구를 사용해 주세요.');
    if (/<\/?[A-Za-z][^>]*>/u.test(syntax)
      && !/^(?:<details>|<\/details>|<summary>.*<\/summary>)$/u.test(syntax)) {
      markdownInvalid('지원하지 않는 HTML 구조를 일반 문단으로 바꾸지 않습니다.');
    }
    for (const link of syntax.matchAll(/\[[^\]]*\]\(([^\s)]*)/gu)) {
      if (!/^(?:https?:\/\/|mailto:|tel:)/iu.test(link[1])) markdownInvalid('새 Markdown 링크는 http/https/mailto/tel만 지원합니다. 내부 문서는 [[worknote:ID|제목]] 또는 기존 보호 block을 유지해 주세요.');
    }
  }
  const result = projectWorkNoteMarkdown(sourceId, markdown, { strict: true });
  if (result.blocks.length > 5_000 || new TextEncoder().encode(JSON.stringify(result.blocks)).byteLength > 524_288) {
    markdownInvalid('편집 문서 구조는 5,000 block과 512 KiB 이하여야 합니다. 잘라 저장하지 않고 분할 편집을 요청합니다.');
  }
  return result;
}

export function projectTeamHubTaskMarkdown(taskIdValue, markdownValue) {
  return projectMarkdown(taskIdValue, markdownValue, 'task-body');
}

function plainLegacyTableBlock(block) {
  if (!block || block.type !== 'text' || typeof block.text !== 'string') return false;
  if (!block.content) return true;
  if (block.content.type !== 'paragraph') return false;
  const nodes = block.content.content || [];
  return nodes.every((node) => node.type === 'text' && !(node.marks || []).length)
    && inlineText(block.content) === block.text;
}

export function normalizeLegacyMarkdownTableBlocks(blocksValue) {
  const source = Array.isArray(blocksValue) ? blocksValue : [];
  const blocks = [];
  let changed = false;
  let index = 0;
  while (index < source.length) {
    const header = source[index];
    const divider = source[index + 1];
    const headerCells = plainLegacyTableBlock(header) ? tableCells(header.text) : null;
    const dividerCells = plainLegacyTableBlock(divider) ? tableCells(divider.text) : null;
    if (!headerCells || dividerCells?.length !== headerCells.length || !isTableDivider(dividerCells)) {
      blocks.push(header);
      index += 1;
      continue;
    }
    const rows = [tableRow(headerCells, true)];
    index += 2;
    while (index < source.length && plainLegacyTableBlock(source[index])) {
      const cells = tableCells(source[index].text);
      if (!cells) break;
      rows.push(tableRow(normalizedTableCells(cells, headerCells.length)));
      index += 1;
    }
    const content = {
      type: 'table',
      attrs: {
        blockId: header.id || blockId('legacy-table', 'block', blocks.length),
        indent: Number(header.indent ?? header.content?.attrs?.indent) || 0,
      },
      content: rows,
    };
    blocks.push({ id: content.attrs.blockId, type: 'table', text: inlineText(content), content });
    changed = true;
  }
  return { blocks, changed };
}
