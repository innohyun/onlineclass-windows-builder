import { invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';
import { isDeskRestoreBlocked } from './desk-restore-lock';
import { isStudentPrivacyEnabled, createStudentPrivacyToggle } from './desk-privacy';

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

const tutorialVersion = 'archive-board-reader-v2';
let currentTenantId = '';
let currentBoard: ArchiveBoardView | null = null;
let selectedPostId = '';
let boardEpoch = 0;
let returnFocus: HTMLElement | null = null;
let panelCollapsed = false;

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
  return `<section class="archive-board-comments"><h4>댓글 ${comments.length}개</h4>${comments.map((comment) => `<article class="archive-board-comment${comment.depth ? ' is-reply' : ''}"><strong>${isStudentPrivacyEnabled() ? '작성자 가림' : escapeHtml(comment.authorDisplayName || '작성자 미확인')}</strong><span>${escapeHtml(dateText(comment.createdAt))}</span><p>${isStudentPrivacyEnabled() ? '댓글 원문 가림' : escapeHtml(comment.content)}</p>${comment.parentUnavailable ? '<small>원댓글을 보관본에서 찾을 수 없습니다.</small>' : ''}</article>`).join('')}</section>`;
}

function renderFiles(files: ArchiveBoardFile[]) {
  if (!files.length) return '';
  return `<section class="archive-board-files"><h4>첨부파일</h4>${files.map((file) => file.unavailable
    ? '<div class="archive-board-file is-unavailable"><i class="fa-solid fa-triangle-exclamation"></i><span>연결된 첨부파일을 보관본에서 찾을 수 없습니다.</span></div>'
    : `<button class="archive-board-file" type="button" data-archive-board-file="${Number(file.ordinal)}"${isStudentPrivacyEnabled() || isDeskRestoreBlocked() ? ' disabled' : ''}><i class="fa-solid fa-paperclip"></i><span><strong>${isStudentPrivacyEnabled() ? '첨부파일 이름 가림' : escapeHtml(file.originalName || '첨부파일')}</strong><small>${escapeHtml(byteText(Number(file.byteSize || 0)))}</small></span><b>열기</b></button>`).join('')}</section>`;
}

function renderPost(post: ArchiveBoardPost, detailed = false) {
  const hidden = isStudentPrivacyEnabled();
  const reactions = Object.entries(post.reactions || {}).filter(([, count]) => Number(count) > 0)
    .map(([kind, count]) => `<span>${hidden ? '반응' : escapeHtml(kind)} ${Number(count)}</span>`).join('');
  const answers = hidden ? [] : answerRows(post.recordSubmission?.answers);
  const linkUrl = hidden ? '' : safeHttpsUrl(post.linkUrl);
  const reactionCount = Object.values(post.reactions || {}).reduce((sum, count) => sum + Math.max(0, Number(count) || 0), 0);
  return `<article class="archive-board-post${post.isPinned ? ' is-pinned' : ''}${post.id === selectedPostId ? ' is-selected' : ''}" data-post-id="${escapeHtml(post.id)}">
    <header><span class="archive-board-status status-${escapeHtml(post.status)}">${escapeHtml(statusLabel(post.status))}</span>${post.isPinned ? '<span class="archive-board-pin"><i class="fa-solid fa-thumbtack"></i> 고정</span>' : ''}<time>${escapeHtml(dateText(post.createdAt))}</time></header>
    <h3>${hidden ? '게시글 제목 가림' : escapeHtml(post.title || '제목 없음')}</h3><p class="archive-board-author">${hidden ? '작성자 가림' : escapeHtml(post.authorDisplayName || '작성자 미확인')}</p>
    <div class="archive-board-content">${hidden ? '학생 기록 가림이 켜져 있습니다.' : escapeHtml(post.content || '본문 없음').replace(/\n/gu, '<br>')}</div>
    ${linkUrl ? `<a href="${escapeHtml(linkUrl)}" target="_blank" rel="noopener noreferrer">연결 주소 열기</a>` : ''}
    ${!hidden && post.status === 'rejected' && post.moderationReason ? `<p class="archive-board-moderation"><strong>반려 사유</strong> ${escapeHtml(post.moderationReason)}</p>` : ''}
    ${post.shelfUnavailable ? '<p class="archive-board-unavailable">연결된 선반을 보관본에서 찾을 수 없습니다.</p>' : ''}
    ${answers.length ? `<section class="archive-board-answers"><h4>${escapeHtml(post.recordSubmission?.formTitle || '기록 답변')}</h4><dl>${answers.map(([label, value]) => `<div><dt>${escapeHtml(label)}</dt><dd>${escapeHtml(value)}</dd></div>`).join('')}</dl></section>` : ''}
    ${detailed ? `${renderFiles(post.attachments || [])}${reactions ? `<section class="archive-board-reaction-detail"><h4>반응 ${reactionCount}개</h4><div class="archive-board-reactions">${reactions}</div></section>` : ''}${renderComments(post.comments || [])}` : `<p class="archive-board-card-counts">첨부 ${post.attachments?.length || 0}개 · 반응 ${reactionCount}개 · 댓글 ${post.comments?.length || 0}개</p><button type="button" class="archive-board-select" data-archive-post-select="${escapeHtml(post.id)}" aria-pressed="${post.id === selectedPostId}">상세 보기 <i class="fa-solid fa-chevron-right" aria-hidden="true"></i></button>`}
  </article>`;
}

function renderBoard() {
  if (!currentBoard) return;
  const { meta, posts, shelves } = currentBoard;
  el('archiveBoardViewerTitle').textContent = isStudentPrivacyEnabled() ? '보관 보드 · 제목 가림' : meta.title;
  el('archiveBoardViewerMeta').textContent = `${modeLabel(meta.studentViewMode)} 보기 · 게시글 ${meta.postCount}개 · 첨부 ${meta.fileCount}개 · ${dateText(meta.importedAt)} 보관`;
  el('archiveBoardViewerMode').textContent = `${modeLabel(meta.studentViewMode)} · 읽기 전용`;
  const wall = el('archiveBoardWall');
  const scroll = wall.scrollTop;
  wall.className = `archive-board-wall mode-${meta.studentViewMode}`;
  if (!posts.length) {
    wall.innerHTML = '<p class="archive-board-empty">보관본에 표시할 게시글이 없습니다.</p>';
    renderSelectedPost();
    return;
  }
  if (meta.studentViewMode === 'shelf') {
    const known = new Set(shelves.map((shelf) => shelf.id));
    const lanes = [...shelves.sort((a, b) => a.sortOrder - b.sortOrder), { id: '', name: '선반 없음', sortOrder: 99 }];
    wall.innerHTML = lanes.map((shelf, index) => {
      const lanePosts = posts.filter((post) => shelf.id ? post.shelfId === shelf.id : !post.shelfId || !known.has(post.shelfId));
      if (!lanePosts.length) return '';
      return `<section class="archive-board-lane"><h2>${isStudentPrivacyEnabled() ? `선반 ${index + 1} · 이름 가림` : escapeHtml(shelf.name)}</h2><div>${lanePosts.map(post => renderPost(post)).join('')}</div></section>`;
    }).join('');
    wall.scrollTop = scroll;
    renderSelectedPost();
    return;
  }
  wall.innerHTML = posts.map(post => renderPost(post)).join('');
  wall.scrollTop = scroll;
  renderSelectedPost();
}

function renderSelectedPost() {
  const panel = document.getElementById('archiveBoardSelected');
  if (!panel) return;
  const post = currentBoard?.posts.find(item => item.id === selectedPostId) || currentBoard?.posts[0];
  if (post) selectedPostId = post.id;
  panel.innerHTML = `<header><strong>선택 게시글 · 읽기 전용</strong><button id="archiveBoardPanelToggle" type="button" aria-expanded="${!panelCollapsed}">${panelCollapsed ? '펼치기' : '접기'}</button></header><div${panelCollapsed ? ' hidden' : ''}>${post ? renderPost(post, true) : '<p class="archive-board-empty">표시할 게시글이 없습니다.</p>'}</div>`;
  panel.closest('.archive-board-workspace')?.classList.toggle('is-panel-collapsed', panelCollapsed);
}

function closeViewer() {
  if (isDeskRestoreBlocked()) return;
  hideViewer();
  returnFocus?.focus({ preventScroll: true });
}

function hideViewer() {
  boardEpoch += 1;
  el<HTMLElement>('archiveBoardViewer').hidden = true;
  currentBoard = null;
  closeTutorial();
}

const tutorialSteps = [
  { target: 'archiveBoardViewerMode', title: '원래 보드 보기 유지', copy: '보관 당시의 갤러리·상세 목록·선반 보기를 그대로 적용합니다.' },
  { target: 'archiveBoardSelected', title: '게시글과 댓글 확인', copy: '게시글을 선택하면 댓글·반응·첨부를 읽기 전용으로 확인합니다. 학생 기록 가림을 해제해야 이름과 원문을 볼 수 있습니다. 보조 패널은 접을 수 있습니다.' },
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
  el(step.target).scrollIntoView({ block: 'center', behavior: 'instant' });
}
function openTutorial() { tutorialIndex = 0; renderTutorial(); }
function closeTutorial() {
  tutorialIndex = -1;
  document.querySelectorAll('.archive-board-tutorial-target').forEach((node) => node.classList.remove('archive-board-tutorial-target'));
  const panel = document.getElementById('archiveBoardTutorial');
  if (panel) panel.hidden = true;
}

export async function openArchiveBoardViewer(tenantId: string, archiveId: string, options: { returnLabel?: string } = {}) {
  if (isDeskRestoreBlocked()) return;
  const token = ++boardEpoch;
  returnFocus = document.activeElement as HTMLElement | null;
  let result: ArchiveBoardViewResult;
  try { result = await invoke<ArchiveBoardViewResult>('get_shared_archive_board_view', { tenantId, archiveId }); }
  catch (error) { if (token !== boardEpoch || isDeskRestoreBlocked()) return; throw error; }
  if (token !== boardEpoch || isDeskRestoreBlocked()) return;
  if (result?.ok === false || !result.board) throw new Error(result.error || 'archive_board_open_failed');
  currentTenantId = tenantId;
  currentBoard = result.board;
  el('archiveBoardViewerClose').textContent = `‹ ${options.returnLabel || '전체 검색'}로 돌아가기`;
  selectedPostId = currentBoard.posts[0]?.id || '';
  el<HTMLElement>('archiveBoardViewer').hidden = false;
  renderBoard();
  try { if (localStorage.getItem(tutorialVersion) !== 'done') openTutorial(); } catch { openTutorial(); }
}

export function initArchiveBoardExplorer() {
  const wall = el('archiveBoardWall');
  const workspace = document.createElement('div');
  workspace.className = 'archive-board-workspace';
  wall.replaceWith(workspace); workspace.append(wall);
  workspace.insertAdjacentHTML('beforeend', '<aside id="archiveBoardSelected" class="archive-board-selected" aria-label="선택 게시글 상세"></aside>');
  document.querySelector('.archive-board-viewer-head')?.append(createStudentPrivacyToggle());
  el('archiveBoardViewerMode').insertAdjacentHTML('afterend', '<button id="archiveBoardExport" type="button">JSON 내보내기</button>');
  el('archiveBoardExport').addEventListener('click', () => { void (async () => {
    if (!currentBoard || isDeskRestoreBlocked() || isStudentPrivacyEnabled()) return;
    const archiveId = currentBoard.meta.archiveId;
    const targetPath = await save({ defaultPath: `${currentBoard.meta.title || '보드'}-보관본.json`, filters: [{ name: 'JSON', extensions: ['json'] }] });
    if (!targetPath || isDeskRestoreBlocked() || isStudentPrivacyEnabled() || currentBoard?.meta.archiveId !== archiveId) return;
    const result = await invoke<{ ok?: boolean }>('export_shared_archive', { archiveId, targetPath });
    el('archiveBoardViewerStatus').textContent = result?.ok === true ? '읽기 전용 보관본을 JSON으로 내보냈습니다.' : 'JSON 내보내기를 완료하지 못했습니다.';
  })().catch(() => { el('archiveBoardViewerStatus').textContent = 'JSON 내보내기를 완료하지 못했습니다.'; }); });
  workspace.addEventListener('click', (event) => {
    if (isDeskRestoreBlocked()) return;
    const target = event.target as HTMLElement;
    const select = target.closest<HTMLButtonElement>('[data-archive-post-select]');
    if (select) { selectedPostId = select.dataset.archivePostSelect || ''; panelCollapsed = false; renderBoard(); }
    if (target.closest('#archiveBoardPanelToggle')) { panelCollapsed = !panelCollapsed; renderSelectedPost(); }
  });
  const refreshPrivacy = () => { if (currentBoard) renderBoard(); el<HTMLButtonElement>('archiveBoardExport').disabled = isDeskRestoreBlocked() || isStudentPrivacyEnabled(); };
  window.addEventListener('desk:student-privacy-changed', refreshPrivacy);
  window.addEventListener('desk:restore-lock-changed', refreshPrivacy);
  window.addEventListener('desk:view-changed', hideViewer);
  refreshPrivacy();
  el('archiveBoardViewerClose').addEventListener('click', closeViewer);
  el('archiveBoardViewerHelp').addEventListener('click', openTutorial);
  workspace.addEventListener('click', (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>('[data-archive-board-file]');
    if (!button || !currentBoard || isDeskRestoreBlocked() || isStudentPrivacyEnabled()) return;
    button.disabled = true;
    void invoke<{ ok?: boolean; error?: string }>('open_shared_archive_file', {
      tenantId: currentTenantId, archiveId: currentBoard.meta.archiveId, ordinal: Number(button.dataset.archiveBoardFile),
    }).then((result) => {
      if (result?.ok === false) el('archiveBoardViewerStatus').textContent = '첨부파일을 열지 못했습니다.';
      else el('archiveBoardViewerStatus').textContent = '첨부파일을 이 PC의 기본 프로그램으로 열었습니다.';
    }).catch(() => { el('archiveBoardViewerStatus').textContent = '첨부파일을 열지 못했습니다.'; }).finally(() => { button.disabled = isDeskRestoreBlocked() || isStudentPrivacyEnabled(); });
  });
  el('archiveBoardTutorialClose').addEventListener('click', closeTutorial);
  el('archiveBoardTutorialNext').addEventListener('click', () => {
    if (tutorialIndex >= tutorialSteps.length - 1) {
      try { localStorage.setItem(tutorialVersion, 'done'); } catch { /* Guidance stays available without persistent storage. */ }
      closeTutorial();
    } else { tutorialIndex += 1; renderTutorial(); }
  });
}
