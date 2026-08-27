import { invoke } from '@tauri-apps/api/core';

type WorkNotePage = { pageId: string; parentId?: string | null; title: string; emoji: string; position: number; properties?: Record<string, unknown>; blocks?: Block[]; markdown?: string; updatedAtMs?: number };
type WorkNoteAttachment = { attachmentId: string; mediaId: string; pageId: string; blockId: string; fileName: string; contentType: string; size: number };
type WorkNoteView = { ok?: boolean; rootPageId?: string; selectedPageId?: string; pages?: WorkNotePage[]; attachments?: WorkNoteAttachment[]; error?: string };
type LocalWorkspace = 'lesson_materials' | 'work_materials';
type WorkspaceTree = { ok?: boolean; pages?: WorkNotePage[]; error?: string };
type WorkspacePage = { ok?: boolean; page?: WorkNotePage; attachments?: WorkNoteAttachment[]; error?: string };
type Block = Record<string, any>;

let tenantId = '';
let pages = new Map<string, WorkNotePage>();
let attachments: WorkNoteAttachment[] = [];
let currentPageId = '';
let workspaceMode: LocalWorkspace | '' = '';
const tutorialVersion = 'local-work-note-reader-v1';
const tutorialSteps = [
  { target: 'workNoteReaderTree', title: '문서와 하위 문서', copy: '왼쪽에서 로컬로 옮긴 전체 문서 구조를 확인하고 페이지를 선택합니다.' },
  { target: 'workNoteReaderBody', title: '업무 노트 원문', copy: '제목·목록·할 일·표·토글·콜아웃·링크·첨부를 저장 당시 형식으로 읽습니다.' },
  { target: 'workNoteReaderClose', title: '읽기 전용 확인', copy: '로컬 자료함에서는 안전하게 읽고, 내용을 바꿀 때는 교사 홈의 업무 노트를 사용합니다.' },
];
let tutorialIndex = -1;

const el = <T extends HTMLElement>(id: string) => {
  const value = document.getElementById(id);
  if (!value) throw new Error(`missing element: ${id}`);
  return value as T;
};
const text = (value: unknown) => String(value ?? '');
const byteText = (value = 0) => value >= 1024 ** 2 ? `${(value / 1024 ** 2).toFixed(1)} MB` : value >= 1024 ? `${Math.round(value / 1024)} KB` : `${value} B`;

function children(parentId: string | null) {
  return [...pages.values()].filter((page) => (page.parentId || null) === parentId).sort((a, b) => Number(a.position) - Number(b.position) || a.title.localeCompare(b.title, 'ko'));
}

function pathFor(page: WorkNotePage) {
  const result = [page.title]; let cursor = page;
  while (cursor.parentId && pages.has(cursor.parentId)) { cursor = pages.get(cursor.parentId)!; result.unshift(cursor.title); }
  return result.join(' / ');
}

function appendTree(container: HTMLElement, page: WorkNotePage, depth: number) {
  const button = document.createElement('button');
  button.type = 'button'; button.dataset.pageId = page.pageId; button.className = page.pageId === currentPageId ? 'is-active' : '';
  button.style.setProperty('--depth', String(depth));
  const icon = document.createElement('span'); icon.textContent = page.emoji || '📄';
  const label = document.createElement('span'); label.textContent = page.title || '제목 없음';
  button.append(icon, label); container.append(button);
  for (const child of children(page.pageId)) appendTree(container, child, depth + 1);
}

function markedText(node: Block): Node {
  let current: Node = document.createTextNode(text(node.text));
  for (const mark of [...(node.marks || [])].reverse()) {
    let wrapper: HTMLElement | null = null;
    if (mark.type === 'bold') wrapper = document.createElement('strong');
    else if (mark.type === 'italic') wrapper = document.createElement('em');
    else if (mark.type === 'underline') wrapper = document.createElement('u');
    else if (mark.type === 'strike') wrapper = document.createElement('s');
    else if (mark.type === 'code') wrapper = document.createElement('code');
    else if (mark.type === 'highlight') wrapper = document.createElement('mark');
    else if (mark.type === 'link') {
      const href = text(mark.attrs?.href).trim();
      if (href.startsWith('worknote://')) {
        const parts = href.replace(/^worknote:\/\/(?:local\/)?/u, '').split('/');
        const pageId = parts[parts.length - 1] || '';
        if (pages.has(pageId)) { wrapper = document.createElement('button'); wrapper.className = 'work-note-inline-link'; wrapper.dataset.pageId = pageId; }
      } else try {
        const url = new URL(href); if (['https:', 'http:', 'mailto:', 'tel:'].includes(url.protocol)) { wrapper = document.createElement('a'); wrapper.setAttribute('href', url.href); wrapper.setAttribute('target', '_blank'); wrapper.setAttribute('rel', 'noopener noreferrer'); }
      } catch { /* 안전하지 않은 링크는 일반 텍스트로 표시 */ }
    }
    if (wrapper) { wrapper.append(current); current = wrapper; }
  }
  return current;
}

function appendChildren(target: HTMLElement, node: Block) {
  for (const child of node.content || []) { const rendered = renderNode(child); if (rendered) target.append(rendered); }
}

function tag(tagName: string, node: Block) {
  const target = document.createElement(tagName); appendChildren(target, node); return target;
}

function inlineText(node: Block): string {
  if (node?.type === 'text') return text(node.text);
  return (node?.content || []).map(inlineText).join(node?.type === 'paragraph' ? '' : ' ').trim();
}

function renderAttachment(attrs: Block) {
  const attachmentId = text(attrs.attachmentId); const record = attachments.find((item) => item.attachmentId === attachmentId);
  const button = document.createElement('button'); button.type = 'button'; button.className = 'work-note-attachment';
  if (record) button.dataset.attachmentId = record.attachmentId;
  const icon = document.createElement('i'); icon.className = 'fa-solid fa-paperclip';
  const copy = document.createElement('span'); const title = document.createElement('strong'); title.textContent = record?.fileName || text(attrs.fileName) || '첨부파일';
  const meta = document.createElement('small'); meta.textContent = record ? `${record.contentType} · ${byteText(record.size)}` : '첨부파일 정보';
  copy.append(title, meta); const action = document.createElement('b'); action.textContent = record ? '열기' : '파일 없음'; button.disabled = !record;
  button.append(icon, copy, action); return button;
}

function renderNode(node: Block): Node | null {
  if (!node || typeof node !== 'object') return null;
  if (node.type === 'text') return markedText(node);
  if (node.type === 'hardBreak') return document.createElement('br');
  if (node.type === 'paragraph') return tag('p', node);
  if (node.type === 'heading') return tag(`h${Math.max(2, Math.min(6, Number(node.attrs?.level) || 2))}`, node);
  if (node.type === 'blockquote') return tag('blockquote', node);
  if (node.type === 'bulletList') return tag('ul', node);
  if (node.type === 'orderedList') { const list = tag('ol', node) as HTMLOListElement; list.start = Math.max(1, Number(node.attrs?.start) || 1); return list; }
  if (node.type === 'listItem') return tag('li', node);
  if (node.type === 'taskList') { const list = tag('ul', node); list.className = 'work-note-task-list'; return list; }
  if (node.type === 'taskItem') { const item = tag('li', node); const checkbox = document.createElement('input'); checkbox.type = 'checkbox'; checkbox.checked = Boolean(node.attrs?.checked); checkbox.disabled = true; item.prepend(checkbox); return item; }
  if (node.type === 'codeBlock') { const pre = document.createElement('pre'); const code = document.createElement('code'); code.textContent = inlineText(node); pre.append(code); return pre; }
  if (node.type === 'horizontalRule') return document.createElement('hr');
  if (node.type === 'table') { const wrap = document.createElement('div'); wrap.className = 'work-note-table'; wrap.append(tag('table', node)); return wrap; }
  if (node.type === 'tableRow') return tag('tr', node);
  if (node.type === 'tableHeader') return tag('th', node);
  if (node.type === 'tableCell') return tag('td', node);
  if (node.type === 'details') { const value = tag('details', node) as HTMLDetailsElement; value.open = node.attrs?.open !== false; return value; }
  if (node.type === 'detailsSummary') return tag('summary', node);
  if (node.type === 'detailsContent') return tag('div', node);
  if (node.type === 'callout') { const aside = tag('aside', node); aside.className = 'work-note-callout'; const icon = document.createElement('span'); icon.textContent = text(node.attrs?.icon) || '💡'; aside.prepend(icon); return aside; }
  if (node.type === 'pageLinkBlock') { const button = document.createElement('button'); button.type = 'button'; button.className = 'work-note-page-link'; button.dataset.pageId = text(node.attrs?.pageId); button.textContent = text(node.attrs?.title) || '연결된 페이지'; button.disabled = !pages.has(button.dataset.pageId); return button; }
  if (node.type === 'attachmentBlock') return renderAttachment(node.attrs || {});
  const fallback = document.createElement('div'); appendChildren(fallback, node); if (!fallback.childNodes.length) fallback.textContent = inlineText(node); return fallback;
}

function legacyNode(block: Block): Block {
  if (block.content?.type) {
    const content = structuredClone(block.content);
    if (content.type === 'attachmentBlock' && text(block.attachmentId).trim()) {
      content.attrs = { ...(content.attrs || {}), attachmentId: text(block.attachmentId) };
    }
    return content;
  }
  const content = [{ type: 'text', text: text(block.text) }];
  if (/^h[1-6]$/u.test(block.type)) return { type: 'heading', attrs: { level: Number(block.type.slice(1)) }, content };
  if (block.type === 'bullet' || block.type === 'number') return { type: block.type === 'bullet' ? 'bulletList' : 'orderedList', content: [{ type: 'listItem', content: [{ type: 'paragraph', content }] }] };
  if (block.type === 'todo') return { type: 'taskList', content: [{ type: 'taskItem', attrs: { checked: Boolean(block.done) }, content: [{ type: 'paragraph', content }] }] };
  if (block.type === 'quote') return { type: 'blockquote', content: [{ type: 'paragraph', content }] };
  if (block.type === 'code') return { type: 'codeBlock', content };
  if (block.type === 'divider') return { type: 'horizontalRule' };
  if (block.type === 'callout') return { type: 'callout', attrs: { icon: block.icon }, content: [{ type: 'paragraph', content }] };
  if (block.type === 'toggle') return { type: 'details', attrs: { open: true }, content: [{ type: 'detailsSummary', content }, { type: 'detailsContent', content: [] }] };
  if (block.type === 'page') return { type: 'pageLinkBlock', attrs: { pageId: block.target, title: block.text } };
  if (block.type === 'attachment') return { type: 'attachmentBlock', attrs: block };
  return { type: 'paragraph', content };
}

function renderMarkdownFallback(markdown: string, target: HTMLElement) {
  const lines = markdown.split(/\r?\n/u); let code: string[] | null = null;
  for (const line of lines) {
    if (line.startsWith('```')) { if (code) { const pre = document.createElement('pre'); pre.textContent = code.join('\n'); target.append(pre); code = null; } else code = []; continue; }
    if (code) { code.push(line); continue; }
    const heading = line.match(/^(#{1,6})\s+(.+)$/u); const quote = line.match(/^>\s?(.*)$/u); const task = line.match(/^[-*]\s+\[([ xX])\]\s+(.+)$/u); const list = line.match(/^[-*]\s+(.+)$/u);
    const node = document.createElement(heading ? `h${heading[1].length}` : quote ? 'blockquote' : task || list ? 'p' : 'p');
    node.textContent = heading?.[2] || quote?.[1] || task?.[2] || list?.[1] || line; if (task) node.prepend(Object.assign(document.createElement('input'), { type: 'checkbox', checked: task[1].toLowerCase() === 'x', disabled: true }));
    if (line || heading || quote || task || list) target.append(node);
  }
}

function renderPage(pageId: string) {
  const page = pages.get(pageId); if (!page) return; currentPageId = pageId;
  el('workNoteReaderTitle').textContent = pages.get([...pages.values()].find((item) => !item.parentId)?.pageId || '')?.title || page.title;
  el('workNoteReaderPageTitle').textContent = page.title; el('workNoteReaderEmoji').textContent = page.emoji || '📄'; el('workNoteReaderPath').textContent = pathFor(page);
  el('workNoteReaderMeta').textContent = `${pages.size}개 페이지 · ${new Intl.DateTimeFormat('ko-KR', { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(Number(page.updatedAtMs || Date.now())))}`;
  const tree = el('workNoteReaderTree'); tree.replaceChildren(); for (const root of children(null)) appendTree(tree, root, 0);
  const properties = el('workNoteReaderProperties'); properties.replaceChildren();
  for (const [key, value] of Object.entries(page.properties || {})) { const chip = document.createElement('span'); chip.textContent = `${key}: ${Array.isArray(value) ? value.join(', ') : text(value)}`; properties.append(chip); }
  const body = el('workNoteReaderBody'); body.replaceChildren(); const blocks = Array.isArray(page.blocks) ? page.blocks : [];
  if (blocks.length) for (const block of blocks) { const rendered = renderNode(legacyNode(block)); if (rendered) body.append(rendered); }
  else if (page.markdown) renderMarkdownFallback(page.markdown, body);
  else { const empty = document.createElement('p'); empty.textContent = '저장된 본문이 없습니다.'; body.append(empty); }
}

function renderTutorial() {
  document.querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target'));
  const step = tutorialSteps[tutorialIndex]; if (!step) { closeTutorial(); return; }
  el(step.target).classList.add('local-reader-tutorial-target'); el('workNoteReaderTutorialStep').textContent = `${tutorialIndex + 1} / ${tutorialSteps.length}`;
  el('workNoteReaderTutorialTitle').textContent = step.title; el('workNoteReaderTutorialCopy').textContent = step.copy;
  el('workNoteReaderTutorialNext').textContent = tutorialIndex === tutorialSteps.length - 1 ? '완료' : '다음'; el('workNoteReaderTutorial').hidden = false;
}
function openTutorial() { tutorialIndex = 0; renderTutorial(); }
function closeTutorial() { tutorialIndex = -1; document.querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target')); const panel = document.getElementById('workNoteReaderTutorial'); if (panel) panel.hidden = true; }

export async function openWorkNoteReader(nextTenantId: string, pageId: string) {
  const result = await invoke<WorkNoteView>('get_local_work_note_view', { tenantId: nextTenantId, pageId });
  if (result?.ok === false) throw new Error(result.error || 'work_note_reader_failed');
  workspaceMode = ''; tenantId = nextTenantId; pages = new Map((result.pages || []).map((page) => [page.pageId, page])); attachments = result.attachments || [];
  el('workNoteReader').hidden = false; renderPage(result.selectedPageId || result.rootPageId || pageId);
  if (localStorage.getItem(tutorialVersion) !== 'done') openTutorial();
}

async function loadWorkspacePage(pageId: string) {
  if (!workspaceMode || !pageId) return;
  el('workNoteReaderStatus').textContent = '선택한 원문과 첨부 정보를 불러오는 중입니다.';
  const result = await invoke<WorkspacePage>('get_local_workspace_page', { tenantId, workspace: workspaceMode, pageId });
  if (result?.ok === false || !result.page) throw new Error(result.error || 'local_workspace_page_failed');
  pages.set(result.page.pageId, result.page); attachments = result.attachments || []; renderPage(result.page.pageId);
  el('workNoteReaderStatus').textContent = '로컬 DB 원문 · 인터넷 없이 열람 가능 · 편집은 교사 홈에서 합니다.';
}

export async function openWorkspaceWorkNoteReader(nextTenantId: string, workspace: LocalWorkspace, requestedPageId = '') {
  const result = await invoke<WorkspaceTree>('get_local_workspace_tree', { tenantId: nextTenantId, workspace });
  if (result?.ok === false) throw new Error(result.error || 'local_workspace_tree_failed');
  workspaceMode = workspace; tenantId = nextTenantId; pages = new Map((result.pages || []).map((page) => [page.pageId, page])); attachments = [];
  const firstRoot = [...pages.values()].find((page) => !page.parentId) || [...pages.values()][0];
  const pageId = requestedPageId && pages.has(requestedPageId) ? requestedPageId : firstRoot?.pageId || '';
  if (!pageId) throw new Error('local_workspace_empty');
  el('workNoteReader').hidden = false;
  await loadWorkspacePage(pageId);
}

export function initWorkNoteReader() {
  el('workNoteReaderClose').addEventListener('click', () => { el('workNoteReader').hidden = true; pages.clear(); attachments = []; workspaceMode = ''; closeTutorial(); });
  el('workNoteReaderHelp').addEventListener('click', openTutorial);
  el('workNoteReaderTutorialClose').addEventListener('click', closeTutorial);
  el('workNoteReaderTutorialNext').addEventListener('click', () => { if (tutorialIndex >= tutorialSteps.length - 1) { localStorage.setItem(tutorialVersion, 'done'); closeTutorial(); } else { tutorialIndex += 1; renderTutorial(); } });
  el('workNoteReaderTree').addEventListener('click', (event) => { const button = (event.target as HTMLElement).closest<HTMLButtonElement>('[data-page-id]'); if (!button?.dataset.pageId) return; if (workspaceMode) void loadWorkspacePage(button.dataset.pageId).catch(() => { el('workNoteReaderStatus').textContent = '선택한 페이지를 불러오지 못했습니다.'; }); else renderPage(button.dataset.pageId); });
  el('workNoteReaderBody').addEventListener('click', (event) => {
    const page = (event.target as HTMLElement).closest<HTMLButtonElement>('[data-page-id]'); if (page?.dataset.pageId) { if (workspaceMode) void loadWorkspacePage(page.dataset.pageId); else renderPage(page.dataset.pageId); return; }
    const file = (event.target as HTMLElement).closest<HTMLButtonElement>('[data-attachment-id]'); if (!file?.dataset.attachmentId) return; file.disabled = true;
    void invoke<{ ok?: boolean; error?: string }>('open_local_data_attachment', { tenantId, mediaId: file.dataset.attachmentId, attachmentKind: 'work-note' })
      .then((result) => { el('workNoteReaderStatus').textContent = result?.ok === false ? '첨부파일을 열지 못했습니다.' : '첨부파일을 이 PC의 기본 프로그램으로 열었습니다.'; })
      .finally(() => { file.disabled = false; });
  });
}
