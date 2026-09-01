import { invoke } from '@tauri-apps/api/core';
import { byteText, escapeHtml } from './data-explorer';
import { openWorkspaceWorkNoteReader } from './work-note-reader';

type WorkspaceKind = 'lesson_materials' | 'work_materials' | 'student_learning_materials';
type WorkspacePage = {
  pageId: string;
  parentId?: string | null;
  title: string;
  emoji: string;
  position: number;
  updatedAtMs: number;
  systemKind?: string | null;
  attachmentCount: number;
  attachmentBytes: number;
};
type WorkspaceResult = { ok?: boolean; total?: number; truncated?: boolean; pages?: WorkspacePage[]; error?: string };

type WorkspaceConfig = {
  view: 'lesson-materials' | 'work-materials' | 'student-learning-materials';
  kind: WorkspaceKind;
  title: string;
  empty: string;
  tutorialKey: string;
};

type LocalWorkspaceOptions = { getTenantId: () => string };

const CONFIGS: WorkspaceConfig[] = [
  { view: 'lesson-materials', kind: 'lesson_materials', title: '수업자료', empty: '이 PC로 전환한 수업자료가 없습니다. 교사 홈의 수업계획에서 전체 수업자료를 로컬로 전환할 수 있습니다.', tutorialKey: 'localLessonMaterialsTutorial:v1' },
  { view: 'work-materials', kind: 'work_materials', title: '업무자료', empty: '이 PC에 저장된 업무자료가 없습니다. 교사 홈 업무노트에서 필요한 문서를 로컬로 전환할 수 있습니다.', tutorialKey: 'localWorkMaterialsTutorial:v1' },
  { view: 'student-learning-materials', kind: 'student_learning_materials', title: '학생 학습자료', empty: '이 PC에 저장된 학생 학습자료가 없습니다. 교사 홈에서 새 자료를 작성하거나 ChatGPT 초안을 저장할 수 있습니다.', tutorialKey: 'localStudentLearningMaterialsTutorial:v1' },
];

const DESIGN_PREVIEW = new URLSearchParams(window.location.search).get('designPreview');
const PREVIEW_PAGES: Record<WorkspaceKind, WorkspacePage[]> = {
  lesson_materials: [
    { pageId: 'lesson-materials-root', title: '수업자료', emoji: '📚', position: 0, updatedAtMs: Date.now(), systemKind: 'lesson_materials_folder', attachmentCount: 0, attachmentBytes: 0 },
    { pageId: 'lesson-science', parentId: 'lesson-materials-root', title: '과학 2단원 · 지층과 화석', emoji: '🪨', position: 0, updatedAtMs: Date.now() - 3_600_000, attachmentCount: 4, attachmentBytes: 184_000_000 },
    { pageId: 'lesson-korean', parentId: 'lesson-materials-root', title: '국어 3단원 · 의견을 조정해요', emoji: '💬', position: 1, updatedAtMs: Date.now() - 86_400_000, attachmentCount: 2, attachmentBytes: 12_500_000 },
  ],
  work_materials: [
    { pageId: 'work-meeting', title: '교무회의', emoji: '🗓️', position: 0, updatedAtMs: Date.now() - 7_200_000, attachmentCount: 3, attachmentBytes: 8_400_000 },
    { pageId: 'work-parent', title: '학부모 안내 자료', emoji: '📨', position: 1, updatedAtMs: Date.now() - 172_800_000, attachmentCount: 5, attachmentBytes: 36_700_000 },
    { pageId: 'work-safety', parentId: 'work-meeting', title: '현장체험학습 안전 점검', emoji: '🚌', position: 0, updatedAtMs: Date.now() - 259_200_000, attachmentCount: 1, attachmentBytes: 2_100_000 },
  ],
  student_learning_materials: [
    { pageId: 'student-learning-materials-root', title: '학생 학습자료', emoji: '🎒', position: 0, updatedAtMs: Date.now(), systemKind: 'student_learning_materials_folder', attachmentCount: 0, attachmentBytes: 0 },
    { pageId: 'student-science-activity', parentId: 'student-learning-materials-root', title: '태양계 조사 활동지', emoji: '📝', position: 0, updatedAtMs: Date.now() - 1_800_000, systemKind: 'student_learning_material', attachmentCount: 1, attachmentBytes: 640_000 },
    { pageId: 'student-science-problem', parentId: 'student-learning-materials-root', title: '행성 특징 확인 문제', emoji: '✅', position: 1, updatedAtMs: Date.now() - 43_200_000, systemKind: 'student_learning_material', attachmentCount: 0, attachmentBytes: 0 },
  ],
};

const required = <T extends HTMLElement>(id: string) => {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing local workspace element: ${id}`);
  return node as T;
};

function suffix(config: WorkspaceConfig) {
  if (config.kind === 'lesson_materials') return 'Lesson';
  if (config.kind === 'student_learning_materials') return 'Student';
  return 'Work';
}

function dateText(value: number) {
  if (!value) return '-';
  return new Intl.DateTimeFormat('ko-KR', { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(value));
}

function depth(page: WorkspacePage, byId: Map<string, WorkspacePage>) {
  let value = 0; let cursor = page; const seen = new Set<string>();
  while (cursor.parentId && byId.has(cursor.parentId) && !seen.has(cursor.parentId)) {
    seen.add(cursor.parentId); cursor = byId.get(cursor.parentId)!; value += 1;
  }
  return Math.min(value, 12);
}

function orderPages(pages: WorkspacePage[]) {
  const byId = new Map(pages.map((page) => [page.pageId, page]));
  const children = new Map<string, WorkspacePage[]>();
  for (const page of pages) {
    const parent = page.parentId && byId.has(page.parentId) ? page.parentId : '';
    const group = children.get(parent) || []; group.push(page); children.set(parent, group);
  }
  for (const group of children.values()) group.sort((a, b) => a.position - b.position || a.title.localeCompare(b.title, 'ko'));
  const result: WorkspacePage[] = []; const seen = new Set<string>();
  const append = (parent: string) => {
    for (const page of children.get(parent) || []) {
      if (seen.has(page.pageId)) continue; seen.add(page.pageId); result.push(page); append(page.pageId);
    }
  };
  append('');
  for (const page of pages) if (!seen.has(page.pageId)) result.push(page);
  return { pages: result, byId };
}

function render(config: WorkspaceConfig, result: WorkspaceResult, selectedPageId = '') {
  const id = suffix(config); const list = required<HTMLElement>(`localWorkspace${id}Tree`);
  const pages = Array.isArray(result.pages) ? result.pages : [];
  const totalAttachments = pages.reduce((sum, page) => sum + Number(page.attachmentCount || 0), 0);
  const totalBytes = pages.reduce((sum, page) => sum + Number(page.attachmentBytes || 0), 0);
  required(`localWorkspace${id}Count`).textContent = `${Number(result.total ?? pages.length)}개 페이지`;
  required(`localWorkspace${id}AttachmentSummary`).textContent = `${totalAttachments}개 · ${byteText(totalBytes)}`;
  required(`localWorkspace${id}Open`).toggleAttribute('disabled', !pages.length);
  if (!pages.length) {
    list.innerHTML = `<div class="local-workspace-empty"><i class="fa-solid fa-folder-open" aria-hidden="true"></i><strong>${escapeHtml(config.title)}가 비어 있습니다.</strong><p>${escapeHtml(config.empty)}</p></div>`;
    required(`localWorkspace${id}Status`).textContent = config.empty;
    return '';
  }
  const ordered = orderPages(pages); const selected = pages.some((page) => page.pageId === selectedPageId) ? selectedPageId : ordered.pages[0]?.pageId || '';
  list.innerHTML = ordered.pages.map((page) => {
    const protectedRoot = ['lesson_materials_folder', 'student_learning_materials_folder'].includes(page.systemKind || '');
    const attachment = Number(page.attachmentCount || 0) > 0
      ? `<small><i class="fa-solid fa-paperclip" aria-hidden="true"></i> ${Number(page.attachmentCount)}개 · ${escapeHtml(byteText(Number(page.attachmentBytes || 0)))}</small>`
      : '<small>첨부 없음</small>';
    return `<button type="button" data-workspace-page-id="${escapeHtml(page.pageId)}" class="${page.pageId === selected ? 'is-selected' : ''}" style="--workspace-depth:${depth(page, ordered.byId)}" aria-pressed="${page.pageId === selected}">
      <span class="local-workspace-page-icon">${escapeHtml(page.emoji || '📄')}</span>
      <span class="local-workspace-page-copy"><strong>${escapeHtml(page.title || '제목 없음')}${protectedRoot ? '<b>보호됨</b>' : ''}</strong>${attachment}</span>
      <time>${escapeHtml(dateText(page.updatedAtMs))}</time><i class="fa-solid fa-chevron-right" aria-hidden="true"></i>
    </button>`;
  }).join('');
  required(`localWorkspace${id}Status`).textContent = result.truncated
    ? '5,000개까지만 표시했습니다. 검색어로 범위를 좁혀 주세요.'
    : '로컬 DB의 분류 metadata만 불러왔습니다. 원문은 페이지를 열 때 읽습니다.';
  return selected;
}

function initWorkspace(config: WorkspaceConfig, options: LocalWorkspaceOptions) {
  const id = suffix(config); const view = required<HTMLElement>(`localWorkspace${id}`); const input = required<HTMLInputElement>(`localWorkspace${id}Query`);
  let selectedPageId = ''; let loaded = false; let tutorialIndex = -1;
  const tutorialSteps = [
    { target: document.querySelector<HTMLElement>(`[data-app-view-target="${config.view}"]`), title: `${config.title} 작업공간`, copy: `${config.title}만 따로 모아 보지만 실제 로컬 DB와 백업은 하나입니다.` },
    { target: input, title: '작업공간 안에서 검색', copy: '제목·본문·첨부파일 이름을 검색하며 다른 작업공간 자료는 섞이지 않습니다.' },
    { target: required<HTMLElement>(`localWorkspace${id}Tree`), title: '가벼운 자료 구조', copy: '먼저 제목·계층·첨부 개수와 크기만 읽고, 본문은 선택할 때 불러옵니다.' },
    { target: required<HTMLElement>(`localWorkspace${id}Open`), title: '읽기 전용으로 열기', copy: '원문과 첨부를 확인합니다. 수정·이동·삭제는 교사 홈에서 합니다.' },
  ];
  const tutorial = required<HTMLElement>(`localWorkspace${id}Tutorial`);
  const renderTutorial = () => {
    document.querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target'));
    const step = tutorialSteps[tutorialIndex];
    if (!step?.target) { tutorial.hidden = true; tutorialIndex = -1; return; }
    step.target.classList.add('local-reader-tutorial-target');
    required(`localWorkspace${id}TutorialStep`).textContent = `${tutorialIndex + 1} / ${tutorialSteps.length}`;
    required(`localWorkspace${id}TutorialTitle`).textContent = step.title;
    required(`localWorkspace${id}TutorialCopy`).textContent = step.copy;
    required(`localWorkspace${id}TutorialNext`).textContent = tutorialIndex === tutorialSteps.length - 1 ? '완료' : '다음';
    tutorial.hidden = false;
  };
  const closeTutorial = (complete = false) => {
    document.querySelectorAll('.local-reader-tutorial-target').forEach((node) => node.classList.remove('local-reader-tutorial-target'));
    tutorial.hidden = true; tutorialIndex = -1;
    if (complete) localStorage.setItem(config.tutorialKey, 'complete');
  };
  const openTutorial = () => { tutorialIndex = 0; renderTutorial(); };

  const load = async (query = '') => {
    const tenantId = options.getTenantId();
    const isPreview = DESIGN_PREVIEW === config.view;
    if (!tenantId && !isPreview) { selectedPageId = render(config, { ok: true, pages: [], total: 0 }); return; }
    required(`localWorkspace${id}Status`).textContent = query ? '검색 중입니다.' : '자료 구조를 불러오는 중입니다.';
    try {
      const previewPages = PREVIEW_PAGES[config.kind].filter((page) => !query || page.title.includes(query));
      const result = isPreview
        ? { ok: true, total: previewPages.length, pages: previewPages }
        : query
        ? await invoke<WorkspaceResult>('search_local_workspace', { input: { tenantId, workspace: config.kind, query, offset: 0, limit: 100 } })
        : await invoke<WorkspaceResult>('get_local_workspace_tree', { tenantId, workspace: config.kind });
      if (result?.ok === false) throw new Error(result.error || 'local_workspace_failed');
      selectedPageId = render(config, result, selectedPageId); loaded = true;
    } catch (error) {
      selectedPageId = ''; required(`localWorkspace${id}Tree`).innerHTML = '<p class="local-workspace-error">자료를 불러오지 못했습니다. 로컬 저장소 연결을 확인해 주세요.</p>';
      required(`localWorkspace${id}Status`).textContent = String((error as Error)?.message || error);
    }
  };
  const openSelected = async () => {
    if (!selectedPageId) return;
    const tenantId = options.getTenantId();
    required<HTMLButtonElement>(`localWorkspace${id}Open`).disabled = true;
    try { await openWorkspaceWorkNoteReader(tenantId, config.kind, selectedPageId); }
    catch { required(`localWorkspace${id}Status`).textContent = '선택한 원문을 열지 못했습니다.'; }
    finally { required<HTMLButtonElement>(`localWorkspace${id}Open`).disabled = false; }
  };

  required<HTMLFormElement>(`localWorkspace${id}Search`).addEventListener('submit', (event) => { event.preventDefault(); void load(input.value.trim()); });
  required(`localWorkspace${id}Clear`).addEventListener('click', () => { input.value = ''; void load(); });
  required(`localWorkspace${id}Refresh`).addEventListener('click', () => void load(input.value.trim()));
  required(`localWorkspace${id}Open`).addEventListener('click', () => void openSelected());
  required(`localWorkspace${id}Help`).addEventListener('click', openTutorial);
  required(`localWorkspace${id}TutorialClose`).addEventListener('click', () => closeTutorial(false));
  required(`localWorkspace${id}TutorialNext`).addEventListener('click', () => {
    if (tutorialIndex >= tutorialSteps.length - 1) closeTutorial(true); else { tutorialIndex += 1; renderTutorial(); }
  });
  required(`localWorkspace${id}Tree`).addEventListener('click', (event) => {
    const row = (event.target as HTMLElement).closest<HTMLButtonElement>('[data-workspace-page-id]'); if (!row?.dataset.workspacePageId) return;
    selectedPageId = row.dataset.workspacePageId;
    view.querySelectorAll<HTMLButtonElement>('[data-workspace-page-id]').forEach((button) => { const active = button.dataset.workspacePageId === selectedPageId; button.classList.toggle('is-selected', active); button.setAttribute('aria-pressed', String(active)); });
  });
  required(`localWorkspace${id}Tree`).addEventListener('dblclick', (event) => { if ((event.target as HTMLElement).closest('[data-workspace-page-id]')) void openSelected(); });

  return {
    async open() {
      if (!loaded) await load();
      if (localStorage.getItem(config.tutorialKey) !== 'complete') openTutorial();
    },
    refresh: () => load(input.value.trim()),
  };
}

export function initLocalWorkspaces(options: LocalWorkspaceOptions) {
  const controllers = new Map(CONFIGS.map((config) => [config.view, initWorkspace(config, options)]));
  return {
    open(view: string) { return controllers.get(view as WorkspaceConfig['view'])?.open(); },
    refresh() { return Promise.all([...controllers.values()].map((controller) => controller.refresh())); },
  };
}
