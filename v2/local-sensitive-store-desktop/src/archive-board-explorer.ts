import { invoke } from '@tauri-apps/api/core';

export type ArchiveBoardSummary = {
  archiveId: string;
  title: string;
  recordCount: number;
  postCount: number;
  fileCount: number;
  totalFileBytes: number;
  importedAt: number;
  studentViewMode: 'gallery' | 'detail' | 'shelf';
};

type ArchiveBoardSearchResult = { ok?: boolean; total?: number; boards?: ArchiveBoardSummary[]; error?: string };
type ArchiveBoardFile = { ordinal?: number; originalName?: string; contentType?: string; byteSize?: number; purpose?: string; unavailable?: boolean };
type ArchiveBoardComment = { id: string; authorDisplayName: string; content: string; parentCommentId?: string | null; parentUnavailable?: boolean; depth: number; createdAt: number };
type ArchiveBoardPost = {
  id: string; title: string; content: string; linkUrl: string; authorDisplayName: string; status: string;
  moderationReason: string; backgroundId: string; isPinned: boolean; shelfId?: string | null;
  shelfUnavailable?: boolean; createdAt: number; updatedAt: number; comments: ArchiveBoardComment[];
  reactions: Record<string, number>; attachments: ArchiveBoardFile[];
  recordSubmission?: { formTitle?: string; submittedAt?: number; answers?: unknown } | null;
};
type ArchiveBoardView = {
  meta: { archiveId: string; tenantId: string; title: string; subject: string; studentViewMode: 'gallery' | 'detail' | 'shelf'; importedAt: number; postCount: number; fileCount: number };
  shelves: Array<{ id: string; name: string; sortOrder: number }>;
  posts: ArchiveBoardPost[];
};
type ArchiveBoardViewResult = { ok?: boolean; board?: ArchiveBoardView; error?: string };

const tutorialVersion = 'archive-board-reader-v1';
let currentTenantId = '';
let currentBoard: ArchiveBoardView | null = null;

const el = <T extends HTMLElement>(id: string) => {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element: ${id}`);
  return node as T;
};
const escapeHtml = (value: unknown) => String(value ?? '').replace(/&/gu, '&amp;').replace(/</gu, '&lt;').replace(/>/gu, '&gt;').replace(/"/gu, '&quot;');
const dateText = (value: number) => value ? new Intl.DateTimeFormat('ko-KR', { year: 'numeric', month: 'long', day: 'numeric', hour: 'numeric', minute: '2-digit' }).format(new Date(value)) : '-';
const byteText = (value = 0) => value >= 1024 ** 2 ? `${(value / 1024 ** 2).toFixed(1)} MB` : value >= 1024 ? `${Math.round(value / 1024)} KB` : `${value} B`;
const statusLabel = (status: string) => ({ approved: '게시됨', pending: '승인 대기', rejected: '반려' } as Record<string, string>)[status] || status || '상태 미확인';
const modeLabel = (mode: string) => ({ gallery: '갤러리', detail: '상세 목록', shelf: '선반' } as Record<string, string>)[mode] || '갤러리';
const safeHttpsUrl = (value: string) => {
  try {
    const url = new URL(value);
    return url.protocol === 'https:' ? url.href : '';
  } catch {
    return '';
  }
};

export async function searchArchiveBoards(tenantId: string, query: string, limit = 100) {
  const result = await invoke<ArchiveBoardSearchResult>('search_shared_archive_boards', { tenantId, query, limit });
  if (result?.ok === false) throw new Error(result.error || 'archive_board_search_failed');
  return { total: Number(result.total || 0), boards: result.boards || [] };
}

function answerRows(value: unknown) {
  if (!value || typeof value !== 'object') return [];
  if (Array.isArray(value)) return value.map((answer, index) => {
    const row = answer && typeof answer === 'object' ? answer as Record<string, unknown> : {};
    return [String(row.label || row.fieldLabel || `항목 ${index + 1}`), String(row.valueLabel || row.value || row.answer || '')];
  });
  return Object.entries(value as Record<string, unknown>).map(([key, answer]) => [key, Array.isArray(answer) ? answer.join(', ') : String(answer ?? '')]);
}

function renderComments(comments: ArchiveBoardComment[]) {
  if (!comments.length) return '';
  return `<section class="archive-board-comments"><h4>댓글 ${comments.length}개</h4>${comments.map((comment) => `<article class="archive-board-comment${comment.depth ? ' is-reply' : ''}"><strong>${escapeHtml(comment.authorDisplayName || '작성자 미확인')}</strong><span>${escapeHtml(dateText(comment.createdAt))}</span><p>${escapeHtml(comment.content)}</p>${comment.parentUnavailable ? '<small>원댓글을 보관본에서 찾을 수 없습니다.</small>' : ''}</article>`).join('')}</section>`;
}

function renderFiles(files: ArchiveBoardFile[]) {
  if (!files.length) return '';
  return `<section class="archive-board-files"><h4>첨부파일</h4>${files.map((file) => file.unavailable
    ? '<div class="archive-board-file is-unavailable"><i class="fa-solid fa-triangle-exclamation"></i><span>연결된 첨부파일을 보관본에서 찾을 수 없습니다.</span></div>'
    : `<button class="archive-board-file" type="button" data-archive-board-file="${Number(file.ordinal)}"><i class="fa-solid fa-paperclip"></i><span><strong>${escapeHtml(file.originalName || '첨부파일')}</strong><small>${escapeHtml(byteText(Number(file.byteSize || 0)))}</small></span><b>열기</b></button>`).join('')}</section>`;
}

function renderPost(post: ArchiveBoardPost) {
  const reactions = Object.entries(post.reactions || {}).filter(([, count]) => Number(count) > 0)
    .map(([kind, count]) => `<span>${escapeHtml(kind)} ${Number(count)}</span>`).join('');
  const answers = answerRows(post.recordSubmission?.answers);
  const linkUrl = safeHttpsUrl(post.linkUrl);
  return `<article class="archive-board-post${post.isPinned ? ' is-pinned' : ''}" data-post-id="${escapeHtml(post.id)}">
    <header><span class="archive-board-status status-${escapeHtml(post.status)}">${escapeHtml(statusLabel(post.status))}</span>${post.isPinned ? '<span class="archive-board-pin"><i class="fa-solid fa-thumbtack"></i> 고정</span>' : ''}<time>${escapeHtml(dateText(post.createdAt))}</time></header>
    <h3>${escapeHtml(post.title || '제목 없음')}</h3><p class="archive-board-author">${escapeHtml(post.authorDisplayName || '작성자 미확인')}</p>
    <div class="archive-board-content">${escapeHtml(post.content || '본문 없음').replace(/\n/gu, '<br>')}</div>
    ${linkUrl ? `<a href="${escapeHtml(linkUrl)}" target="_blank" rel="noopener noreferrer">연결 주소 열기</a>` : ''}
    ${post.status === 'rejected' && post.moderationReason ? `<p class="archive-board-moderation"><strong>반려 사유</strong> ${escapeHtml(post.moderationReason)}</p>` : ''}
    ${post.shelfUnavailable ? '<p class="archive-board-unavailable">연결된 선반을 보관본에서 찾을 수 없습니다.</p>' : ''}
    ${answers.length ? `<section class="archive-board-answers"><h4>${escapeHtml(post.recordSubmission?.formTitle || '기록 답변')}</h4><dl>${answers.map(([label, value]) => `<div><dt>${escapeHtml(label)}</dt><dd>${escapeHtml(value)}</dd></div>`).join('')}</dl></section>` : ''}
    ${renderFiles(post.attachments || [])}${reactions ? `<div class="archive-board-reactions">${reactions}</div>` : ''}${renderComments(post.comments || [])}
  </article>`;
}

function renderBoard() {
  if (!currentBoard) return;
  const { meta, posts, shelves } = currentBoard;
  el('archiveBoardViewerTitle').textContent = meta.title;
  el('archiveBoardViewerMeta').textContent = `${modeLabel(meta.studentViewMode)} 보기 · 게시글 ${meta.postCount}개 · 첨부 ${meta.fileCount}개 · ${dateText(meta.importedAt)} 보관`;
  el('archiveBoardViewerMode').textContent = `${modeLabel(meta.studentViewMode)} · 읽기 전용`;
  const wall = el('archiveBoardWall');
  wall.className = `archive-board-wall mode-${meta.studentViewMode}`;
  if (!posts.length) {
    wall.innerHTML = '<p class="archive-board-empty">보관본에 표시할 게시글이 없습니다.</p>';
    return;
  }
  if (meta.studentViewMode === 'shelf') {
    const known = new Set(shelves.map((shelf) => shelf.id));
    const lanes = [...shelves.sort((a, b) => a.sortOrder - b.sortOrder), { id: '', name: '선반 없음', sortOrder: 99 }];
    wall.innerHTML = lanes.map((shelf) => {
      const lanePosts = posts.filter((post) => shelf.id ? post.shelfId === shelf.id : !post.shelfId || !known.has(post.shelfId));
      if (!lanePosts.length) return '';
      return `<section class="archive-board-lane"><h2>${escapeHtml(shelf.name)}</h2><div>${lanePosts.map(renderPost).join('')}</div></section>`;
    }).join('');
    return;
  }
  wall.innerHTML = posts.map(renderPost).join('');
}

function closeViewer() {
  el<HTMLElement>('archiveBoardViewer').hidden = true;
  currentBoard = null;
  closeTutorial();
}

const tutorialSteps = [
  { target: 'archiveBoardViewerMode', title: '원래 보드 보기 유지', copy: '보관 당시의 갤러리·상세 목록·선반 보기를 그대로 적용합니다.' },
  { target: 'archiveBoardWall', title: '게시글과 댓글 확인', copy: '게시글, 상태, 댓글·답글, 반응, 기록 답변을 인터넷 없이 읽을 수 있습니다.' },
  { target: 'archiveBoardViewerClose', title: '자료 탐색으로 돌아가기', copy: '읽기 전용 확인을 마치면 자료 탐색 결과로 안전하게 돌아갑니다.' },
];
let tutorialIndex = -1;
function renderTutorial() {
  const step = tutorialSteps[tutorialIndex];
  if (!step) return closeTutorial();
  document.querySelectorAll('.archive-board-tutorial-target').forEach((node) => node.classList.remove('archive-board-tutorial-target'));
  el(step.target).classList.add('archive-board-tutorial-target');
  el('archiveBoardTutorialStep').textContent = `${tutorialIndex + 1} / ${tutorialSteps.length}`;
  el('archiveBoardTutorialTitle').textContent = step.title;
  el('archiveBoardTutorialCopy').textContent = step.copy;
  el('archiveBoardTutorialNext').textContent = tutorialIndex === tutorialSteps.length - 1 ? '완료' : '다음';
  el<HTMLElement>('archiveBoardTutorial').hidden = false;
}
function openTutorial() { tutorialIndex = 0; renderTutorial(); }
function closeTutorial() {
  tutorialIndex = -1;
  document.querySelectorAll('.archive-board-tutorial-target').forEach((node) => node.classList.remove('archive-board-tutorial-target'));
  const panel = document.getElementById('archiveBoardTutorial');
  if (panel) panel.hidden = true;
}

export async function openArchiveBoardViewer(tenantId: string, archiveId: string) {
  const result = await invoke<ArchiveBoardViewResult>('get_shared_archive_board_view', { tenantId, archiveId });
  if (result?.ok === false || !result.board) throw new Error(result.error || 'archive_board_open_failed');
  currentTenantId = tenantId;
  currentBoard = result.board;
  el<HTMLElement>('archiveBoardViewer').hidden = false;
  renderBoard();
  if (localStorage.getItem(tutorialVersion) !== 'done') openTutorial();
}

export function initArchiveBoardExplorer() {
  el('archiveBoardViewerClose').addEventListener('click', closeViewer);
  el('archiveBoardViewerHelp').addEventListener('click', openTutorial);
  el('archiveBoardWall').addEventListener('click', (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>('[data-archive-board-file]');
    if (!button || !currentBoard) return;
    button.disabled = true;
    void invoke<{ ok?: boolean; error?: string }>('open_shared_archive_file', {
      tenantId: currentTenantId, archiveId: currentBoard.meta.archiveId, ordinal: Number(button.dataset.archiveBoardFile),
    }).then((result) => {
      if (result?.ok === false) el('archiveBoardViewerStatus').textContent = '첨부파일을 열지 못했습니다.';
      else el('archiveBoardViewerStatus').textContent = '첨부파일을 이 PC의 기본 프로그램으로 열었습니다.';
    }).finally(() => { button.disabled = false; });
  });
  el('archiveBoardTutorialClose').addEventListener('click', closeTutorial);
  el('archiveBoardTutorialNext').addEventListener('click', () => {
    if (tutorialIndex >= tutorialSteps.length - 1) {
      localStorage.setItem(tutorialVersion, 'done');
      closeTutorial();
    } else { tutorialIndex += 1; renderTutorial(); }
  });
}
