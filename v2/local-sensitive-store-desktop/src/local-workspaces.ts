import { invoke } from '@tauri-apps/api/core';
import { byteText, escapeHtml } from './data-explorer';
import { openWorkspaceWorkNoteReader } from './work-note-reader';
import { pinnedDocumentIds, toggleDocumentFavorite } from './desk-document-preferences';
import { isDeskRestoreBlocked } from './desk-restore-lock';
import type { LocalDocument, DocumentAttachment } from './document-repository';

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

type LocalWorkspaceOptions = { getTenantId: () => string; openDocument?: (input: { pageId: string; workspace: 'workNotes' | 'lessonMaterials'; tenantId: string }) => void | Promise<void> };

const CONFIGS: WorkspaceConfig[] = [
  { view: 'lesson-materials', kind: 'lesson_materials', title: '수업자료', empty: '첫 수업자료를 만들어 보세요. 이 PC에 저장하며 인터넷 연결 없이도 작성할 수 있습니다.', tutorialKey: 'localLessonMaterialsTutorial:v3' },
  { view: 'work-materials', kind: 'work_materials', title: '업무 노트', empty: '첫 업무 노트를 만들어 보세요. 새 문서를 만들거나 서식함에서 시작할 수 있습니다.', tutorialKey: 'localWorkMaterialsTutorial:v3' },
  { view: 'student-learning-materials', kind: 'student_learning_materials', title: '학생 학습자료', empty: '이 PC에 이전 보관본이 없습니다. 학생 학습자료 작성·공개는 온라인 교사 홈에서 진행합니다.', tutorialKey: 'localStudentLearningMaterialsTutorial:v3' },
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

function initWorkspace(config: WorkspaceConfig, options: LocalWorkspaceOptions) {
  const id = suffix(config); const view = required<HTMLElement>(`localWorkspace${id}`); const input = required<HTMLInputElement>(`localWorkspace${id}Query`);
  const list=required<HTMLElement>(`localWorkspace${id}Tree`);list.setAttribute('role','list');const panel=list.closest<HTMLElement>('.local-workspace-panel')!;
  const search=required(`localWorkspace${id}Search`);const metrics=view.querySelector<HTMLElement>('.local-workspace-summary')!;
  const folders=document.createElement('aside');folders.className='local-workspace-folders';folders.setAttribute('aria-label',`${config.title} 폴더`);
  const folderTree=document.createElement('nav');folders.append(search,folderTree);
  const detail=document.createElement('aside');detail.className='local-workspace-detail';detail.setAttribute('aria-label','선택한 자료 미리보기');
  const columns=document.createElement('div');columns.className='local-workspace-columns';panel.before(columns);columns.append(folders,panel,detail);
  panel.querySelector('header h2')!.textContent='전체 자료';
  panel.querySelector('header')!.insertAdjacentHTML('afterbegin',`<div class="local-workspace-filters"><button type="button" data-workspace-filter="all" aria-pressed="true">전체 자료</button><button type="button" data-workspace-filter="favorites" aria-pressed="false">☆ 즐겨찾기</button></div>`);
  panel.querySelector('header')!.insertAdjacentHTML('beforeend','<span class="local-workspace-sort">최신순</span>');
  metrics.classList.add('local-workspace-totals');panel.querySelector('footer')!.prepend(metrics);
  const isStudent=config.kind==='student_learning_materials';
  const headingCopy=view.querySelector<HTMLElement>('.local-workspace-heading p');if(headingCopy)headingCopy.textContent=isStudent?'이전에 이 PC에 보관한 자료를 읽습니다. 학생 공개 원본은 온라인 교사 홈에서 관리합니다.':`이 PC의 ${config.title}를 폴더와 검색으로 찾으세요.`;
  const readonly=view.querySelector<HTMLElement>('.local-workspace-readonly');if(readonly)readonly.innerHTML=isStudent?'<p><strong>이 PC의 이전 보관본 · 읽기 전용</strong>학생에게 공개되는 원본이 아닙니다. 작성·수정·공개는 온라인 교사 홈에서 진행합니다.</p>':'<p>문서 저장과 백업·동기화는 별도로 진행됩니다.</p>';
  type ListState={selected:string;folder:string;favorites:boolean;query:string;treeScroll:number;listScroll:number;detailClosed:boolean};
  const states=new Map<string,ListState>();const initial=():ListState=>({selected:'',folder:'',favorites:false,query:'',treeScroll:0,listScroll:0,detailClosed:false});
  let state=initial();let selectedPageId='';let loadedTenant='';let hasLoaded=false;let generation=0;let previewGeneration=0;let currentPages:WorkspacePage[]=[];let treePages:WorkspacePage[]=[];let tutorialIndex=-1;let resultTotal=0;let resultTruncated=false;
  const editable=Boolean(options.openDocument) && !isStudent;
  const remember=()=>{state.selected=selectedPageId;state.query=input.value;state.treeScroll=folderTree.scrollTop;state.listScroll=list.scrollTop;if(loadedTenant)states.set(loadedTenant,{...state});};
  const isFolder=(item:WorkspacePage)=>Boolean(treePages.some(child=>child.parentId===item.pageId) || item.systemKind?.endsWith('_folder'));
  function renderFolders(){
    const ordered=orderPages(treePages);folderTree.innerHTML=`<button type="button" data-workspace-folder="" aria-pressed="${!state.folder}">▱ 모든 폴더</button>`+ordered.pages.filter(isFolder).map(item=>`<button type="button" data-workspace-folder="${escapeHtml(item.pageId)}" aria-pressed="${state.folder===item.pageId}" style="--workspace-depth:${depth(item,ordered.byId)}" title="${escapeHtml(item.title)}">▱ ${escapeHtml(item.title)}</button>`).join('');folderTree.scrollTop=state.treeScroll;
  }
  function render(){
    const pinned=pinnedDocumentIds(options.getTenantId());const belongs=(item:WorkspacePage)=>{if(!state.folder)return true;let parent=item.parentId;const seen=new Set<string>();while(parent && !seen.has(parent)){if(parent===state.folder)return true;seen.add(parent);parent=treePages.find(candidate=>candidate.pageId===parent)?.parentId;}return false;};
    const visible=currentPages.filter(item=>!isFolder(item) && belongs(item) && (!state.favorites || pinned.has(item.pageId))).sort((a,b)=>b.updatedAtMs-a.updatedAtMs || a.title.localeCompare(b.title,'ko'));
    required(`localWorkspace${id}Count`).textContent=`${visible.length}개 자료${resultTotal>currentPages.length?' · 일부 표시':''}`;
    required(`localWorkspace${id}AttachmentSummary`).textContent=`첨부 ${visible.reduce((sum,item)=>sum+item.attachmentCount,0)}개 · ${byteText(visible.reduce((sum,item)=>sum+item.attachmentBytes,0))}`;
    view.querySelectorAll<HTMLElement>('[data-workspace-filter]').forEach(button=>button.setAttribute('aria-pressed',String((button.dataset.workspaceFilter==='favorites')===state.favorites)));
    if(!visible.length){list.innerHTML=`<div class="local-workspace-empty"><i class="fa-solid fa-folder-open" aria-hidden="true"></i><strong>${state.favorites?'즐겨찾기한 자료가 없습니다.':input.value?'검색 결과가 없습니다.':config.title+' 0개'}</strong><p>${escapeHtml(state.favorites?'목록의 별표를 눌러 자주 사용하는 자료를 모으세요.':input.value?'검색어를 수정하거나 필터를 초기화해 주세요.':config.empty)}</p>${!input.value && !state.favorites && editable?`<button type="button" data-desk-create="${config.kind==='lesson_materials'?'lessonMaterials':'workNotes'}">＋ 새 ${config.title}</button><button type="button" data-app-view-target="templates">서식함에서 시작</button>`:''}</div>`;}
    else list.innerHTML=visible.map(item=>`<div role="listitem" class="local-workspace-row ${item.pageId===selectedPageId?'is-selected':''}"><button type="button" data-workspace-page-id="${escapeHtml(item.pageId)}" aria-pressed="${item.pageId===selectedPageId}" title="${escapeHtml(item.title)}"><span class="local-workspace-page-icon">${escapeHtml(item.emoji || '▤')}</span><span class="local-workspace-page-copy"><strong>${escapeHtml(item.title || '제목 없음')}</strong><small>${isStudent?'이 PC 이전 보관본 · 읽기 전용':'이 PC 저장본'} · 첨부 ${item.attachmentCount}개</small></span><time>${escapeHtml(dateText(item.updatedAtMs))}</time><span aria-hidden="true">›</span></button><button type="button" data-workspace-pin="${escapeHtml(item.pageId)}" class="local-workspace-pin" aria-label="${pinned.has(item.pageId)?'즐겨찾기 해제':'즐겨찾기 추가'}" aria-pressed="${pinned.has(item.pageId)}">${pinned.has(item.pageId)?'★':'☆'}</button></div>`).join('');
    required<HTMLButtonElement>(`localWorkspace${id}Open`).disabled=!visible.some(item=>item.pageId===selectedPageId);
    list.scrollTop=state.listScroll;renderFolders();
    required(`localWorkspace${id}Status`).textContent=resultTruncated?'일부 자료만 표시했습니다. 검색어로 범위를 좁혀 주세요.':isStudent?'이 PC의 이전 보관본입니다. 학생 공개 원본은 온라인 교사 홈에서 확인하세요.':'문서 저장과 백업·동기화는 별도로 진행됩니다.';
    if(!selectedPageId)detail.innerHTML=`<div class="local-workspace-empty"><strong>${editable?'왼쪽에서 문서를 선택하세요.':'이전 보관본을 선택하세요.'}</strong><p>${escapeHtml(treePages.some(item=>!isFolder(item)) ? '왼쪽에서 자료를 선택하면 내용과 첨부파일을 확인할 수 있습니다.' : config.empty)}</p></div>`;
  }
  async function preview(){
    const item=currentPages.find(candidate=>candidate.pageId===selectedPageId) || treePages.find(candidate=>candidate.pageId===selectedPageId);const requestGeneration=++previewGeneration;detail.classList.toggle('is-collapsed',state.detailClosed);
    if(!item){detail.innerHTML='<p class="local-workspace-empty">왼쪽에서 문서를 선택하세요.</p>';return;}
    const tenantId=options.getTenantId();const pageId=item.pageId;detail.innerHTML=`<button type="button" data-detail-toggle aria-expanded="${!state.detailClosed}">${state.detailClosed?'패널 펼치기':'패널 접기'}</button><h2>${escapeHtml(item.title)}</h2><p role="status">원문을 불러오는 중입니다.</p>`;
    try{
      const response=await invoke<{ok?:boolean;page?:LocalDocument;attachments?:DocumentAttachment[];error?:string}>('get_local_workspace_page',{tenantId,workspace:config.kind,pageId});
      if(requestGeneration!==previewGeneration || tenantId!==options.getTenantId() || pageId!==selectedPageId)return;
      if(response.ok===false || !response.page || response.page.pageId!==pageId)throw new Error(response.error || '문서의 확인 결과가 일치하지 않습니다.');
      const page=response.page;const files=response.attachments || [];
      detail.innerHTML=`<button type="button" data-detail-toggle aria-expanded="${!state.detailClosed}">${state.detailClosed?'패널 펼치기':'패널 접기'}</button><h2>${escapeHtml(page.title)}</h2><span class="local-workspace-location">${isStudent?'이 PC 이전 보관본 · 읽기 전용':'이 PC 저장본'}</span><dl><dt>마지막 수정</dt><dd>${escapeHtml(dateText(page.updatedAtMs || item.updatedAtMs))}</dd></dl>${page.markdown?`<h3>${isStudent?'자료 내용':'본문 미리보기'}</h3><pre class="local-workspace-preview">${escapeHtml(page.markdown)}</pre>`:'<p>본문이 비어 있습니다.</p>'}<h3>첨부파일 ${files.length}개</h3>${files.map(file=>`<article><strong>${escapeHtml(file.fileName)}</strong><small>${escapeHtml(byteText(file.size))} · 첨부 등록됨</small><button type="button" data-workspace-file="${escapeHtml(file.attachmentId)}">파일 열기 ↗</button></article>`).join('')}<button type="button" data-preview-open class="is-primary">${isStudent?'원문 크게 보기':'문서 열기'} ›</button>${isStudent?'<p>학생에게 공개되는 원본이 아닙니다.</p><button type="button" data-desk-action="student-materials">온라인 교사 홈에서 작성·공개 ↗</button>':''}`;
    }catch{if(requestGeneration===previewGeneration)detail.innerHTML=`<h2>${escapeHtml(item.title)}</h2><p role="alert">원문을 불러오지 못했습니다. 선택과 필터는 유지됩니다.</p><button type="button" data-preview-retry>다시 시도</button>`;}
  }
  const tutorialSteps = [
    { target: document.querySelector<HTMLElement>(`[data-app-view-target="${config.view}"]`), title: `${config.title} 작업공간`, copy: `${config.title}만 따로 모아 보지만 실제 로컬 DB와 백업은 하나입니다.` },
    { target: input, title: '작업공간 안에서 검색', copy: '제목·본문·첨부파일 이름을 검색하며 다른 작업공간 자료는 섞이지 않습니다.' },
    { target: required<HTMLElement>(`localWorkspace${id}Tree`), title: '가벼운 자료 구조', copy: '폴더와 즐겨찾기로 범위를 좁히세요. 선택한 문서의 원문과 첨부만 오른쪽에서 불러옵니다.' },
    { target: required<HTMLElement>(`localWorkspace${id}Open`), title: editable ? '본문에서 바로 편집' : '기존 자료 열람', copy: editable ? '문서를 열어 기존 서식과 첨부를 유지하며 편집합니다. 보호된 시스템 폴더는 읽기 전용입니다. 로컬 저장과 학생 공개는 별도입니다.' : '이 목록은 기존 로컬 자료입니다. 새 학생 학습자료는 교사 홈에서 작성·검토·공개하며 인터넷 연결이 필요합니다.' },
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
    const requestGeneration=++generation;const tenantId=options.getTenantId();
    if(loadedTenant!==tenantId){remember();state=states.get(tenantId) || initial();selectedPageId=state.selected;input.value=state.query;query=state.query;currentPages=[];treePages=[];loadedTenant=tenantId;hasLoaded=false;}
    const isPreview=DESIGN_PREVIEW===config.view;
    if(!tenantId && !isPreview){currentPages=[];selectedPageId='';render();return;}
    required(`localWorkspace${id}Status`).textContent=query?'검색 중입니다.':'자료 구조를 불러오는 중입니다.';
    try{
      const previewPages=PREVIEW_PAGES[config.kind].filter(item=>!query || item.title.includes(query));
      const result=isPreview?{ok:true,total:previewPages.length,pages:previewPages}:query?await invoke<WorkspaceResult>('search_local_workspace',{input:{tenantId,workspace:config.kind,query,offset:0,limit:100}}):await invoke<WorkspaceResult>('get_local_workspace_tree',{tenantId,workspace:config.kind});
      if(requestGeneration!==generation || tenantId!==options.getTenantId())return;
      if(result?.ok===false)throw new Error(result.error || 'local_workspace_failed');
      hasLoaded=true;currentPages=result.pages || [];if(!query)treePages=currentPages;resultTotal=Number(result.total ?? currentPages.length);resultTruncated=result.truncated===true;
      if(!query && state.folder && !treePages.some(item=>item.pageId===state.folder))state.folder='';
      render();remember();if(selectedPageId)void preview();
    }catch(reason){if(requestGeneration!==generation || tenantId!==options.getTenantId())return;required(`localWorkspace${id}Status`).textContent='자료를 불러오지 못했습니다. 선택·검색·필터를 유지합니다. 새로고침으로 다시 시도해 주세요.';if(!currentPages.length)list.innerHTML='<p class="local-workspace-error">자료를 불러오지 못했습니다. 다시 시도해 주세요.</p>';}
  };
  const openSelected = async () => {
    if (!selectedPageId) return;
    const tenantId = options.getTenantId();
    if(isDeskRestoreBlocked() && editable){required(`localWorkspace${id}Status`).textContent='자료함 복원 상태를 확인하는 동안 편집을 시작할 수 없습니다.';return;}
    if (tenantId !== loadedTenant) { await load(input.value.trim()); return; }
    remember();required<HTMLButtonElement>(`localWorkspace${id}Open`).disabled = true;
    try {
      const selected = currentPages.find((page) => page.pageId === selectedPageId) || treePages.find(page=>page.pageId===selectedPageId);
      const protectedFolder = ['lesson_materials_folder', 'student_learning_materials_folder'].includes(selected?.systemKind || '');
      if (editable && !protectedFolder) await options.openDocument?.({ tenantId, pageId: selectedPageId, workspace: config.kind === 'lesson_materials' ? 'lessonMaterials' : 'workNotes' });
      else await openWorkspaceWorkNoteReader(tenantId, config.kind, selectedPageId);
    }
    catch { required(`localWorkspace${id}Status`).textContent = '선택한 원문을 열지 못했습니다.'; }
    finally { render(); }
  };

  required<HTMLFormElement>(`localWorkspace${id}Search`).addEventListener('submit', (event) => { event.preventDefault(); void load(input.value.trim()); });
  required(`localWorkspace${id}Clear`).addEventListener('click', () => {input.value='';state.folder='';state.favorites=false;void load();});
  required(`localWorkspace${id}Refresh`).addEventListener('click', () => void load(input.value.trim()));
  required(`localWorkspace${id}Open`).addEventListener('click', () => void openSelected());
  required(`localWorkspace${id}Help`).addEventListener('click', openTutorial);
  required(`localWorkspace${id}TutorialClose`).addEventListener('click', () => closeTutorial(false));
  required(`localWorkspace${id}TutorialNext`).addEventListener('click', () => {
    if (tutorialIndex >= tutorialSteps.length - 1) closeTutorial(true); else { tutorialIndex += 1; renderTutorial(); }
  });
  view.addEventListener('click',event=>{
    const target=(event.target as Element).closest<HTMLElement>('button');if(!target)return;
    if(target.dataset.workspaceFilter){remember();state.favorites=target.dataset.workspaceFilter==='favorites';state.listScroll=0;render();remember();}
    else if(target.dataset.workspaceFolder!==undefined){remember();state.folder=target.dataset.workspaceFolder;state.listScroll=0;render();remember();}
    else if(target.dataset.workspacePin){try{toggleDocumentFavorite(options.getTenantId(),target.dataset.workspacePin);render();}catch(reason){required(`localWorkspace${id}Status`).textContent=String((reason as Error).message);}}
    else if(target.dataset.workspacePageId){remember();selectedPageId=target.dataset.workspacePageId;render();remember();void preview();}
    else if(target.hasAttribute('data-preview-open'))void openSelected();
    else if(target.hasAttribute('data-preview-retry'))void preview();
    else if(target.hasAttribute('data-detail-toggle')){state.detailClosed=!state.detailClosed;detail.classList.toggle('is-collapsed',state.detailClosed);target.textContent=state.detailClosed?'패널 펼치기':'패널 접기';target.setAttribute('aria-expanded',String(!state.detailClosed));remember();}
    else if(target.dataset.workspaceFile)void invoke<{ok?:boolean;error?:string}>('open_local_data_attachment',{tenantId:options.getTenantId(),mediaId:target.dataset.workspaceFile,attachmentKind:'work-note'}).then(result=>{if(result.ok===false)throw new Error(result.error);}).catch(()=>{required(`localWorkspace${id}Status`).textContent='첨부파일을 열지 못했습니다. 파일 위치와 연결 프로그램을 확인해 주세요.';});
  });
  list.addEventListener('dblclick',event=>{if((event.target as Element).closest('[data-workspace-page-id]'))void openSelected();});
  list.addEventListener('scroll',()=>{state.listScroll=list.scrollTop;});folderTree.addEventListener('scroll',()=>{state.treeScroll=folderTree.scrollTop;});
  window.addEventListener('desk:favorites-changed',event=>{if((event as CustomEvent).detail?.tenantId===options.getTenantId()){remember();render();}});
  window.addEventListener('desk:restore-lock-changed',()=>{view.querySelectorAll<HTMLButtonElement>('[data-desk-create]').forEach(button=>button.disabled=isDeskRestoreBlocked());});

  return {
    async open() {
      if (!hasLoaded || !loadedTenant || loadedTenant !== options.getTenantId()) await load();else{render();if(selectedPageId)void preview();}
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
