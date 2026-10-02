import { createDocumentRepository, DocumentUserError, documentError, documentRevision, type LocalDocument, type NativeRequest, type DocumentDraft, type HistoryCursor } from './document-repository';
import { createDocumentSaveSession, type SaveState } from './document-save-session.mjs';
import { createWorkNotesTiptapEditor, projectDocumentMarkdown } from './document-editor-bridge.mjs';
import { createDocumentWorkspaceView, escapeDocumentHtml as html, selectDocumentFiles, downloadDocument } from './document-workspace-view';
import { pinnedDocumentIds, toggleDocumentFavorite } from './desk-document-preferences';
import { isDeskRestoreBlocked } from './desk-restore-lock';
import { documentTemplates } from './document-templates';
import { unsupportedDocumentNodes, retainDocumentBlockMetadata } from './document-compatibility.mjs';
import './document-workspace.css';
import { open as chooseNativeFiles } from '@tauri-apps/plugin-dialog';

export type DocumentWorkspaceInput={tenantId?:string;workspace?:'workNotes'|'lessonMaterials';pageId?:string;templateId?:string};
export type WorkspaceOptions = {getTenantId:()=>string; isNative?:()=>boolean; onChanged?:()=>void; openTeacherHome?:(path:string)=>void; request?:NativeRequest;mount?:HTMLElement;onOpen?:(input:DocumentWorkspaceInput)=>boolean|void|Promise<boolean|void>;onClose?:(workspace:'workNotes'|'lessonMaterials')=>void|Promise<void>};
type SaveSession = ReturnType<typeof createDocumentSaveSession>;
export function initDocumentWorkspace(options: WorkspaceOptions) {
  const view = createDocumentWorkspaceView({mount:options.mount});
  const repository = createDocumentRepository(options.request);
  let page: LocalDocument | null = null;
  let tenant = '';
  let pages: LocalDocument[] = [];
  let session: SaveSession | null = null;
  let workspace:'workNotes'|'lessonMaterials' = 'workNotes';
  let attachments:Awaited<ReturnType<typeof repository.attachments>>=[];
  let attachmentGeneration=0;
  let confirmation:(()=>Promise<unknown>)|null=null;
  let preservedDraft:DocumentDraft|null=null;
  const collapsedFolders=new Set<string>();
  const navigationStates=new Map<string,{query:string;scroll:number}>();
  let pendingRecovery: DocumentDraft | null = null;
  let readOnly = false;
  let editingLocked = false;
  let nativeAttachmentPending = false;
  let originalBlocks: LocalDocument['blocks'] = [];
  let historyCursor:HistoryCursor|null=null;
  let operation: Promise<unknown> = Promise.resolve();
  let searchGeneration = 0;
  const error = (reason: unknown) => {view.alert.textContent=documentError(reason);view.alert.hidden=false;view.show();if(session?.dirty() && !readOnly){for(const [name,label] of [['save','다시 저장'],['compare-draft','저장본·초안 비교']]){const button=document.createElement('button');button.type='button';button.dataset.docAction=name;button.textContent=label;view.alert.append(' ',button);}}};
  const clearError = () => { view.alert.hidden = true; view.alert.textContent = ''; };
  const run = (task:()=>Promise<unknown>) => { operation = operation.catch(()=>{}).then(task).catch(error); return operation; };
  function status(state: SaveState) {
    view.status.dataset.state = state.state;
    view.status.textContent = ({saved:'내 PC에 저장됨',saving:'저장 중…',dirty:'변경 내용 저장 대기',composing:'입력 중…',error:'저장 확인 필요',draft_error:'초안 저장 확인 필요'} as Record<string,string>)[state.state] || '내 PC 문서';
    if (state.error) {
      error(state.error);
    }
    if (state.state === 'saved') clearError();
  }
  const path = (id: string) => pages.find(item=>item.pageId===id)?.title || '문서';
  const editor = createWorkNotesTiptapEditor({
    element:view.editor, getPage:()=>page, getPages:()=>pages, pagePath:path, escapeHtml:html,
    isEditable:()=>Boolean(page && !pendingRecovery && !readOnly && !editingLocked && !isDeskRestoreBlocked()), canUseAi:()=>false,
    normalizeSerializedBlocks:(blocks:LocalDocument['blocks'])=>retainDocumentBlockMetadata(blocks,originalBlocks),
    onChange:(next:LocalDocument)=>{ if (session && !pendingRecovery && !readOnly && !isDeskRestoreBlocked()) session.change(next); },
    onStatus:error, flush:async()=>{ await session?.flush(); return page; },
    createPage:async(parentId:string|null,title:string,settings:{open?:boolean}={})=> {
      const created = await createPage(parentId,title);
      if (settings.open !== false) await openPage(created.pageId);
      return created;
    },
    openPage:(pageId:string)=>run(()=>openPage(pageId)),
    openLinkedPage:(target:{source:string;tenantId?:string;documentId?:string;pageId:string})=> {
      if (target.source === 'local' && (!target.tenantId || target.tenantId === tenant)) void run(()=>openPage(target.pageId));
      else if (target.source === 'cloud') void run(async()=>{await close();options.openTeacherHome?.(`/admin/work-notes/shared?tenantId=${encodeURIComponent(tenant)}&documentId=${encodeURIComponent(target.documentId || '')}&pageId=${encodeURIComponent(target.pageId)}`);});
      else error('다른 학급의 문서입니다. 해당 학급으로 전환한 뒤 열어 주세요.');
    },
    searchPages:(query:string)=>repository.list(tenant,query),
    pickAttachment:(kind:string)=>selectDocumentFiles(kind==='image'?'image/*':kind==='pdf'?'application/pdf':''),
    uploadAttachment:async(pageId:string,blockId:string,file:File,context:{attachmentId:string})=>repository.upload(tenant,pageId,blockId,file,context.attachmentId),
    getAttachmentBlob:(id:string)=>repository.attachmentBlob(tenant,id),
    openAttachment:options.request?undefined:(id:string)=>repository.openAttachment(tenant,id),
    maxAttachmentPreviewBytes:20*1024*1024,
    removeAttachment:(id:string)=>repository.removeAttachment(tenant,id),
    attachmentLocationLabel:'내 PC · 오프라인 사용 가능', attachmentStoredCopy:()=> '첨부파일을 내 PC에 보관했습니다.',
    onAttachmentError:error, onAttachmentStatus:(message:string)=>{view.status.textContent=message;},
    onAttachmentSettled:()=>{ if(page){session?.change(page);void refreshAttachments().catch(error);} },
  });
  function changed() { options.onChanged?.(); document.dispatchEvent(new CustomEvent('desk:documents-changed')); }
  async function refreshPages() {
    pages = await repository.list(tenant);
    renderPages();
  }
  function inWorkspace(item:LocalDocument){
    let cursor:LocalDocument|undefined=item;const visited=new Set<string>();let kind='';
    while(cursor && !visited.has(cursor.pageId)){visited.add(cursor.pageId);kind=cursor.systemKind || cursor.properties?.systemKind || kind;if(cursor.pageId==='student-learning-materials-root' || kind.startsWith('student_learning_material'))return false;if(cursor.pageId==='lesson-materials-root' || kind==='lesson_materials_folder')return workspace==='lessonMaterials';cursor=pages.find(candidate=>candidate.pageId===cursor?.parentId);}
    return workspace==='workNotes' && !(item.systemKind || item.properties?.systemKind);
  }
  function renderPages(records=pages) {
    const scroll=view.pages.scrollTop;
    const query=view.search.value.trim().toLocaleLowerCase('ko');
    const sorted = records.filter(inWorkspace).filter(item=>records!==pages || !query || `${item.title} ${item.markdown || ''}`.toLocaleLowerCase('ko').includes(query)).sort((a,b)=>(a.position||0)-(b.position||0)||a.title.localeCompare(b.title,'ko'));
    const ids=new Set(sorted.map(item=>item.pageId));const visited=new Set<string>();const ordered:LocalDocument[]=[];
    const append=(item:LocalDocument)=>{if(visited.has(item.pageId))return;visited.add(item.pageId);ordered.push(item);if(!collapsedFolders.has(item.pageId) || view.search.value)for(const child of sorted.filter(candidate=>candidate.parentId===item.pageId))append(child);};
    for(const item of sorted.filter(candidate=>!candidate.parentId || !ids.has(candidate.parentId)))append(item);
    const depth = (item:LocalDocument) => { let n=0; let id=item.parentId; const seen=new Set([item.pageId]); while(id && n<8 && !seen.has(id)) { seen.add(id); n++; id=pages.find(p=>p.pageId===id)?.parentId; } return n; };
    view.pages.innerHTML = ordered.length ? ordered.map(item=>`<div class="dw-page-row" style="--depth:${depth(item)}">${sorted.some(child=>child.parentId===item.pageId)?`<button class="dw-folder-toggle" type="button" data-folder="${html(item.pageId)}" aria-expanded="${!collapsedFolders.has(item.pageId)}" aria-label="${html(item.title)} 폴더 ${collapsedFolders.has(item.pageId)?'펼치기':'접기'}">${collapsedFolders.has(item.pageId)?'›':'⌄'}</button>`:''}<button type="button" data-open-page="${html(item.pageId)}" aria-current="${item.pageId===page?.pageId?'page':'false'}" title="${html(item.title)}"><span>${html(item.emoji || (item.properties?.nodeKind==='folder'?'▱':'▤'))}</span><span>${html(item.title || '제목 없음')}</span></button></div>`).join('') : '<p class="dw-empty">문서가 없습니다. 새 문서를 만들어 보세요.</p>';
    view.pages.scrollTop=scroll;editor.refreshPageLinks();
  }
  function updateControls(){
    const blocked=isDeskRestoreBlocked();const writable=Boolean(page && !pendingRecovery && !readOnly && !blocked);
    const structure=writable && page?.editPolicy?.structureEditable!==false && !page?.systemKind && !page?.properties?.systemKind;
    editor.setEditable(writable && !editingLocked);
    view.title.disabled=!writable || page?.editPolicy?.titleEditable===false || Boolean(page?.systemKind || page?.properties?.systemKind);
    view.dialog.querySelectorAll<HTMLButtonElement>('.dw-format button,[data-doc-action="attach"],[data-doc-action="save"]').forEach(button=>button.disabled=!writable);
    view.dialog.querySelectorAll<HTMLButtonElement>('[data-doc-action="history"],[data-doc-action="export"],[data-doc-action="export-json"]').forEach(button=>button.disabled=!page);
    view.dialog.querySelectorAll<HTMLButtonElement>('[data-doc-action="duplicate"],[data-doc-action="child"],[data-doc-action="move"],[data-doc-action="trash"]').forEach(button=>button.disabled=!structure);
    view.dialog.querySelectorAll<HTMLButtonElement>('[data-doc-action="new"],[data-doc-action="import"]').forEach(button=>button.disabled=blocked);
    view.dialog.querySelector<HTMLElement>('#dwStructureNotice')!.hidden=!(page?.editPolicy?.structureEditable===false);
    const pinned=page && pinnedDocumentIds(tenant).has(page.pageId);const favorite=view.dialog.querySelector<HTMLButtonElement>('[data-doc-action="favorite"]')!;favorite.disabled=!page;favorite.textContent=pinned?'★ 즐겨찾기 해제':'☆ 즐겨찾기 추가';favorite.setAttribute('aria-pressed',String(Boolean(pinned)));
  }
  function details(){
    if(!page){view.details.innerHTML='<h2>문서 정보</h2><p>문서를 선택해 주세요.</p>';return;}
    const binding=page.lessonBinding;
    const period=binding?(binding.startPeriod===binding.endPeriod?`${binding.startPeriod}교시`:`${binding.startPeriod}–${binding.endPeriod}교시`):'';
    view.details.innerHTML=`<h2>${binding?'수업 연결':'문서 정보'}</h2>${binding?`<p class="dw-binding-lock">♙ 시간표에서 연결됨</p><dl><dt>교시</dt><dd>${html(period)}</dd><dt>과목</dt><dd>${html(binding.subject || '미지정')}</dd><dt>수업일</dt><dd>${html(binding.dateKey)}</dd></dl><button type="button" data-doc-action="online-lesson">온라인 수업계획에서 관리 ↗</button>`:`<dl><dt>유형</dt><dd>${workspace==='lessonMaterials'?'수업자료':'업무 노트'}</dd><dt>마지막 수정</dt><dd>${html(page.updatedAtMs?new Date(page.updatedAtMs).toLocaleString('ko-KR'):'기록 없음')}</dd></dl>`}<h2>첨부파일 <span>${attachments.length}</span></h2>${attachmentList()}${readOnly?'<button type="button" data-doc-action="export-json">원문 JSON 내보내기</button><p>전체 블록·속성·첨부 참조를 원문 그대로 보관합니다.</p>':''}`;
  }
  function attachmentList(){return attachments.length?attachments.map(item=>`<article class="dw-attachment"><strong>${html(item.fileName)}</strong><small>이 PC · ${(item.size/1024/1024).toFixed(1)}MB · 첨부 등록됨</small><button type="button" data-open-attachment="${html(item.attachmentId)}">파일 열기 ↗</button></article>`).join(''):'<p class="dw-empty">첨부파일이 없습니다.</p>';}
  async function refreshAttachments(){const id=page?.pageId;const generation=++attachmentGeneration;if(!id){attachments=[];details();return;}const items=await repository.attachments(tenant,id);if(generation!==attachmentGeneration || page?.pageId!==id)return;attachments=items;view.dialog.querySelector('#dwAttachmentCount')!.textContent=String(items.length);details();}
  function confirm(title:string,body:string,task:()=>Promise<unknown>,destructive=false,acceptLabel=title,cancelLabel='취소'){confirmation=task;view.confirm.hidden=false;view.confirm.querySelector('section')!.innerHTML=`<header><h2 id="dwConfirmTitle">${html(title)}</h2><button type="button" data-doc-action="confirm-cancel" aria-label="확인 닫기">×</button></header>${body}<footer><button type="button" data-doc-action="confirm-cancel">${html(cancelLabel)}</button><button class="${destructive?'dw-danger':'dw-primary'}" type="button" data-doc-action="confirm-accept">${html(acceptLabel)}</button></footer>`;view.confirm.querySelector<HTMLButtonElement>('[data-doc-action="confirm-cancel"]')?.focus();}
  function assertWritable(structure=false){if(isDeskRestoreBlocked())throw new DocumentUserError('자료함 복원 상태를 확인하는 동안 문서 변경이 중단됩니다.');if(readOnly || pendingRecovery)throw new DocumentUserError('읽기 전용 상태입니다. 초안과 원문을 먼저 확인해 주세요.');if(structure && page?.editPolicy?.structureEditable===false)throw new DocumentUserError('연결된 수업 문서의 제목·구조는 수업계획에서 관리합니다.');}
  async function flush() {
    if(nativeAttachmentPending)throw new DocumentUserError('파일 첨부가 끝난 뒤 이동하거나 앱을 닫아 주세요.');
    if (!session || !page) return;
    if (readOnly) return;
    if (pendingRecovery) return; // The durable draft can be reviewed on the next visit.
    if (session.isComposing()) throw new DocumentUserError('한글 입력을 마친 뒤 저장해 주세요.');
    await editor.waitForPendingUploads(page.pageId);
    await editor.serializeCurrent({forceChange:true});
    await session.flush();
  }
  async function bindSession(record:LocalDocument, draftGeneration=0) {
    await editor.releaseRealtime();
    view.editor.replaceChildren();
    session?.close(); page = structuredClone(record);
    if(record.properties?.nodeKind==='folder' || (record.systemKind || record.properties?.systemKind || '').endsWith('_folder')){
      session=null;readOnly=true;attachments=[];updateControls();details();view.title.value=record.title;view.title.disabled=true;view.heading.textContent=record.title;
      view.editor.textContent='폴더 안의 노트를 선택해 주세요.';view.status.textContent='폴더 · 읽기 전용';view.dialog.querySelector('#dwAttachmentCount')!.textContent='0';return;
    }
    originalBlocks=structuredClone(record.blocks);
    const unsupported=unsupportedDocumentNodes(record.blocks);
    readOnly=unsupported.length>0;
    session = createDocumentSaveSession({tenantId:tenant,page,repository,draftGeneration,onState:status,onSaved:(saved,{latest})=> {
      if (!page || page.pageId !== saved.pageId) return;
      page.revision = saved.revision; page.updatedAtMs = saved.updatedAtMs;
      if(latest){Object.assign(page,saved);view.title.value=page.title;view.heading.textContent=page.title || '제목 없음';}
      const index=pages.findIndex(item=>item.pageId===saved.pageId);
      if(index>=0) pages[index]=saved; else pages.push(saved);
      renderPages(); changed();
    }});
    view.title.value=page.title; view.title.disabled=readOnly || page.editPolicy?.titleEditable===false || Boolean(page.properties?.systemKind);
    view.title.title=page.editPolicy?.titleEditable===false?'연결된 수업의 제목은 수업계획에서 변경하세요.':'';
    view.label.textContent = workspace==='lessonMaterials' ? '수업자료 · 로컬 문서' : '업무노트 · 로컬 문서';
    view.heading.textContent=page.title || '제목 없음';
    view.status.textContent=readOnly?'호환 읽기 전용':'이 PC 저장본'; view.status.dataset.state=readOnly?'readonly':'saved';
    if(readOnly) {
      view.editor.innerHTML=`<p class="dw-alert">지원하지 않는 블록(${html(unsupported.join(', '))})이 있어 원문을 보존한 채 열람합니다. 전체 블록·속성·첨부 참조를 원문 JSON으로 보관하세요.</p><button type="button" data-doc-action="export-json">원문 JSON 내보내기</button><pre class="dw-readonly">${html(page.markdown)}</pre>`;
    } else editor.mount(page);
    updateControls();await refreshAttachments();
  }
  function showRecovery(draft:DocumentDraft, canonical:LocalDocument) {
    pendingRecovery=draft;preservedDraft=draft;updateControls();
    const conflict=draft.baseRevision !== documentRevision(canonical);
    view.recovery.hidden=false;
    const preview=(value:LocalDocument)=>`<h3>${html(value.title)}</h3><pre>${html(value.markdown)}</pre>`;
    view.recovery.innerHTML=`<strong>${conflict?'최신 문서와 다른 초안이 있습니다':'이전에 작성하던 초안이 있습니다'}</strong><span class="dw-recovery-badge">${conflict?'저장본이 변경됨':'저장본과 같은 기준'}</span><p>어떤 내용을 이어서 작성할지 먼저 확인해 주세요.</p><div class="dw-recovery-compare"><article><header>현재 저장본 · ${html(canonical.updatedAtMs?new Date(canonical.updatedAtMs).toLocaleString('ko-KR'):'시간 기록 없음')}</header>${preview(canonical)}</article><article><header>내 초안 · ${html(draft.updatedAtMs?new Date(draft.updatedAtMs).toLocaleString('ko-KR'):'시간 기록 없음')}</header>${preview(draft)}</article></div><div>${conflict || readOnly?'': '<button data-recovery="resume" class="dw-primary" type="button">초안 이어 쓰기</button>'}<button data-recovery="view" type="button">저장본 읽기 전용으로 보기</button><button data-recovery="copy" type="button">초안을 새 문서로 보관</button></div><p>보기만 해서는 초안을 지우거나 저장본을 변경하지 않습니다.</p><button data-recovery="discard" type="button">초안 삭제</button>`;
  }
  async function openPage(pageId:string, alreadyFlushed=false) {
    if(!alreadyFlushed)await flush();
    const canonical=await repository.get(tenant,pageId);
    const draft=await repository.draft(tenant,pageId);
    pendingRecovery=null;preservedDraft=null;readOnly=false;selectTab('body'); view.recovery.hidden=true; view.panel.hidden=true; clearError();
    await bindSession(canonical); renderPages();
    if(draft && (draft.markdown!==canonical.markdown || draft.title!==canonical.title || JSON.stringify(draft.blocks)!==JSON.stringify(canonical.blocks))) showRecovery(draft,canonical);
    else if(draft && !isDeskRestoreBlocked()) await repository.discardDraft(tenant,pageId,draft.generation);
  }
  async function createPage(parentId:string|null,title='제목 없음',markdown='') {
    if(isDeskRestoreBlocked())throw new DocumentUserError('자료함 복원 상태를 확인하는 동안 새 문서를 만들 수 없습니다.');
    const parent=pages.find(item=>item.pageId===parentId);
    if(parent?.properties?.nodeKind==='note')parentId=parent.parentId||null;
    const pageId=crypto.randomUUID();
    const body=markdown.trim()?projectDocumentMarkdown(pageId,markdown):{blocks:[{id:crypto.randomUUID(),type:'text',text:''}],markdown:''};
    const created=await repository.save(tenant,{pageId,parentId,title,emoji:'',properties:{nodeKind:'note'},position:Date.now(),...body},0);
    await refreshPages(); changed(); return created;
  }
  function currentPage() { if(!page) throw new DocumentUserError('문서를 먼저 열어 주세요.'); return page; }
  function selectTab(name:string){view.dialog.querySelectorAll<HTMLButtonElement>('.dw-tabs [role=tab]').forEach(button=>button.setAttribute('aria-selected',String(button.dataset.docAction===name)));}
  function panel(title:string,body:string) { view.panel.hidden=false; view.panel.innerHTML=`<div class="dw-panel-heading"><h2>${html(title)}</h2><button type="button" data-doc-action="panel-close" aria-label="상세 작업 닫기">×</button></div>${body}`; }
  async function showHistory(more=false) {
    await flush();
    if(!page){panel('수정 이력 확인','<p>이력을 확인할 문서를 선택하세요.</p>'+pages.map(item=>`<button type="button" data-history-page="${html(item.pageId)}">${html(item.title)}</button>`).join(''));return;}
    const current=currentPage(); const result=await repository.history(tenant,current.pageId,more?historyCursor:null);
    const body=result.items.length?result.items.map(version=>`<article><strong>${html(version.title || current.title)}</strong><p>${html(new Date(version.capturedAtMs).toLocaleString('ko-KR'))}</p><button type="button" data-preview-version="${html(version.versionId)}">내용 확인</button></article>`).join(''):'<p>저장된 이전 버전이 없습니다. 다음 수정부터 이전 내용이 남습니다.</p>';
    if(more){view.panel.querySelector('[data-doc-action=history-more]')?.remove();view.panel.insertAdjacentHTML('beforeend',body);}else panel('최근 30일 수정 이력',body);
    historyCursor=result.nextCursor;
    if(result.hasMore && historyCursor)view.panel.insertAdjacentHTML('beforeend','<button type="button" data-doc-action="history-more">이전 이력 더 보기</button>');
  }
  async function showTrash() {
    await flush(); const items=await repository.trash(tenant);
    panel('휴지통 · 30일 보관','<p>하위 문서와 첨부파일도 함께 복원합니다.</p>'+ (items.length?items.map(item=>`<article><strong>${html(item.title)}</strong><button type="button" data-restore="${html(item.pageId)}">복원</button></article>`).join(''):'<p>휴지통이 비어 있습니다.</p>'));
  }
  async function movePanel() {
    await flush(); const current=currentPage();
    const descendants=new Set([current.pageId]); let grew=true;
    while(grew){grew=false;for(const item of pages){if(item.parentId && descendants.has(item.parentId) && !descendants.has(item.pageId)){descendants.add(item.pageId);grew=true;}}}
    panel('문서 이동','<p>문서를 넣을 상위 페이지를 선택하세요.</p><button type="button" data-move-parent="">자료함 최상위</button>'+pages.filter(item=>!descendants.has(item.pageId)).map(item=>`<button type="button" data-move-parent="${html(item.pageId)}">${html(item.title)}</button>`).join(''));
  }
  async function close() { await flush(); await editor.releaseRealtime(); session?.close(); session=null; page=null;view.hide();await options.onClose?.(workspace); }
  async function structureChange(task:()=>Promise<unknown>) {
    editingLocked=true;editor.setEditable(false);view.title.disabled=true;
    try {await flush();return await task();}
    finally {editingLocked=false;updateControls();}
  }
  async function action(name:string) {
    clearError();
    if(name==='menu'){view.menu.hidden=!view.menu.hidden;view.dialog.querySelector('[data-doc-action=menu]')!.setAttribute('aria-expanded',String(!view.menu.hidden));updateControls();return;}
    if(name==='details'){view.details.hidden=!view.details.hidden;const button=view.dialog.querySelector('[data-doc-action=details]')!;button.setAttribute('aria-expanded',String(!view.details.hidden));button.textContent=view.details.hidden?'정보 ›':'정보 ‹';return;}
    if(name==='confirm-cancel'){confirmation=null;view.confirm.hidden=true;view.title.focus();return;}
    if(name==='confirm-accept'){const task=confirmation;if(!task)return;await task();confirmation=null;view.confirm.hidden=true;return;}
    view.menu.hidden=true;
    if(name==='body'){view.panel.hidden=true;view.dialog.querySelector('.dw-paper')!.removeAttribute('hidden');selectTab('body');return;}
    if(name==='attachments'){selectTab('attachments');await flush();await refreshAttachments();panel('첨부파일',attachmentList());return;}
    if(name==='online-lesson'){await flush();await close();options.openTeacherHome?.('/admin/lesson-plans');return;}
    if(name==='export-json'){await flush();const active=currentPage();downloadDocument(`${active.title || '문서'}.json`,JSON.stringify(active,null,2),'application/json;charset=utf-8');return;}
    if(name==='close') return close();
    if(name==='save') return flush();
    if(name==='compare-draft') {
      const id=currentPage().pageId;await session?.checkpoint();
      const draft=await repository.draft(tenant,id);if(!draft)throw new DocumentUserError('초안 저장을 확인하지 못했습니다. 현재 본문을 복사해 보관해 주세요.');
      const canonical=await repository.get(tenant,id);await bindSession(canonical);showRecovery(draft,canonical);return;
    }
    if(name==='panel-close') {view.panel.hidden=true;selectTab('body');return;}
    if(name==='trash-list') return showTrash();
    if(name==='history'){selectTab('history');return showHistory();}
    if(name==='history-more') return showHistory(true);
    if(name==='move'){assertWritable(true);return movePanel();}
    if(name==='favorite'){toggleDocumentFavorite(tenant,currentPage().pageId);updateControls();return;}
    if(name==='attach') {assertWritable();
      if(options.request){await editor.insertFiles(await selectDocumentFiles());return;}
      await flush();nativeAttachmentPending=true;
      try {
      const selected=await chooseNativeFiles({multiple:true,directory:false,title:'문서에 첨부할 파일 선택'});
      for(const sourcePath of (Array.isArray(selected)?selected:selected?[selected]:[])) {
        const record=await repository.uploadPath(tenant,currentPage().pageId,sourcePath);
        const kind=record.contentType==='application/pdf'?'pdf':['image','video','audio'].find(value=>record.contentType.startsWith(`${value}/`)) || 'file';
        if(!editor.insertStoredAttachment({...record,kind,displayMode:record.size<=20*1024*1024 && kind!=='file'?'preview':'file'}))throw new DocumentUserError('파일은 보관했지만 본문에 추가하지 못했습니다. 문서 편집 상태를 확인해 주세요.');
      }
      } finally {nativeAttachmentPending=false;}
      await flush();return;
    }
    if(name==='new' || name==='child') {if(name==='child')assertWritable(true);await flush();const parent=name==='child'?currentPage().pageId:workspace==='lessonMaterials'?(await repository.ensureWorkspace(tenant,'lesson_materials')).pageId:null; const created=await createPage(parent); return openPage(created.pageId);}
    if(name==='duplicate') {assertWritable(true);await flush(); const result=await repository.mutate(tenant,currentPage(),'duplicate'); await refreshPages(); changed(); if(result.page?.pageId) await openPage(result.page.pageId);return;}
    if(name==='trash') {assertWritable(true);await flush();const active=structuredClone(currentPage());const descendants=new Set<string>();let grew=true;while(grew){grew=false;for(const candidate of pages){if(candidate.parentId && (candidate.parentId===active.pageId || descendants.has(candidate.parentId)) && !descendants.has(candidate.pageId)){descendants.add(candidate.pageId);grew=true;}}}confirm('휴지통으로 이동',`<p>다음 문서와 하위 문서를 휴지통으로 이동합니다.</p><strong>${html(active.title)}</strong><p>문서 1개와 하위 문서 ${descendants.size}개</p><p>첨부파일은 복구할 수 있도록 함께 보존됩니다.</p><p>휴지통에서 30일 동안 복원할 수 있습니다. 보관 기간이 지나면 자동 정리됩니다.</p><p>확인 중 문서가 변경되면 이동을 중단하고 다시 확인합니다.</p>`,()=>structureChange(async()=>{assertWritable(true);await repository.mutate(tenant,active,'trash');session?.close();session=null;page=null;await editor.releaseRealtime();view.title.value='';view.heading.textContent='문서를 선택해 주세요';view.editor.replaceChildren();await refreshPages();changed();updateControls();await refreshAttachments();}),true);return;}
    if(name==='export') {await flush(); const active=currentPage();downloadDocument(`${active.title || '문서'}.md`,active.markdown);return;}
    if(name==='import') {
      await flush(); const files=await selectDocumentFiles('.md,.markdown,text/markdown,text/plain',false); if(!files[0])return;
      if(files[0].size>800_000)throw new DocumentUserError('Markdown 파일은 200,000자 이내로 나눠 가져와 주세요.');
      const source=await files[0].text(); const created=await createPage(page?.parentId || null,files[0].name.replace(/\.(?:md|markdown)$/i,''),source);
      return openPage(created.pageId);
    }
  }
  async function begin(detail:DocumentWorkspaceInput,kind:string) {
    if(options.isNative && !options.isNative()) throw new DocumentUserError('문서 작성은 Windows 앱에서 사용할 수 있습니다.');
    const nextTenant=detail.tenantId || options.getTenantId();
    if(!nextTenant)throw new DocumentUserError('학급을 먼저 선택해 주세요.');
    if(await options.onOpen?.({...detail,tenantId:nextTenant,workspace:detail.workspace || workspace})===false)return;await flush();
    if(tenant){navigationStates.set(`${tenant}:${workspace}`,{query:view.search.value,scroll:view.pages.scrollTop});}
    if(tenant && nextTenant!==tenant){await editor.releaseRealtime();session?.close();session=null;page=null;}
    tenant=nextTenant;workspace=detail.workspace || workspace;
    const navigation=navigationStates.get(`${tenant}:${workspace}`);view.search.value=navigation?.query || '';view.pages.scrollTop=navigation?.scroll || 0;
    for(const id of ['dwNavTitle','dwWorkspaceName'])view.dialog.querySelector(`#${id}`)!.textContent=workspace==='lessonMaterials'?'수업자료':'업무 노트';view.search.placeholder=workspace==='lessonMaterials'?'수업자료 찾기':'업무 노트 찾기';
    view.show();
    view.panel.hidden=true;view.menu.hidden=true;view.confirm.hidden=true;confirmation=null;selectTab('body');
    if(!page){view.title.disabled=true;view.title.value='';view.heading.textContent='문서를 선택해 주세요';view.label.textContent='선택한 문서 없음';view.status.textContent='저장할 문서를 선택해 주세요';view.editor.textContent='왼쪽 목록에서 문서를 열거나 새 문서를 만드세요.';updateControls();details();}
    await refreshPages();
    if(kind==='create') {
      if(isDeskRestoreBlocked())throw new DocumentUserError('자료함 복원 상태를 확인하는 동안 새 문서를 만들 수 없습니다.');await flush(); const template=documentTemplates[detail.templateId || ''];
      const parent=workspace==='lessonMaterials'?(await repository.ensureWorkspace(tenant,'lesson_materials')).pageId:null;
      const created=await createPage(parent,template?.title || '제목 없음',template?.markdown || '');return openPage(created.pageId);
    }
    if(detail.pageId)await openPage(detail.pageId);
    if(kind==='trash')return showTrash();
    if(kind==='history')return showHistory();
  }
  view.dialog.addEventListener('cancel',event=>{event.preventDefault();void run(close);});
  view.dialog.addEventListener('click',event=> {
    const target=(event.target as Element).closest<HTMLElement>('button');if(!target)return;
    if(target.dataset.folder){const id=target.dataset.folder;collapsedFolders.has(id)?collapsedFolders.delete(id):collapsedFolders.add(id);renderPages();}
    else if(target.dataset.openAttachment)void run(async()=>{try{await repository.openAttachment(tenant,target.dataset.openAttachment!);}catch{throw new DocumentUserError('첨부파일을 열지 못했습니다. 파일 위치와 연결 프로그램을 확인한 뒤 다시 시도해 주세요.');}});
    else if(target.dataset.docAction)void run(()=>action(target.dataset.docAction!));
    else if(target.dataset.openPage)void run(()=>openPage(target.dataset.openPage!));
    else if(target.dataset.historyPage)void run(async()=>{await openPage(target.dataset.historyPage!);await showHistory();});
    else if(target.dataset.previewVersion)void run(async()=> {
      const versionId=target.dataset.previewVersion!;const version=await repository.historyVersion(tenant,currentPage().pageId,versionId);
      panel('이전 내용 확인',`<strong>${html(version.title)}</strong><pre class="dw-version-body">${html(version.markdown)}</pre><p>현재 문서를 이 내용으로 복원합니다. 현재 내용도 이력에 남습니다.</p><button type="button" data-version="${html(versionId)}" ${readOnly || pendingRecovery || isDeskRestoreBlocked()?'disabled':''}>이 버전으로 복원</button><button type="button" data-doc-action="history">이력 목록으로</button>`);
    });
    else if(target.dataset.recovery)void run(async()=> {
      const draft=pendingRecovery || preservedDraft; if(!draft || !page)return;
      const choice=target.dataset.recovery;
      if(choice==='view'){readOnly=true;pendingRecovery=null;view.recovery.hidden=false;view.recovery.innerHTML='<strong>저장본 읽기 전용 · 초안 보관됨</strong><p>내 초안은 그대로 보관되어 있습니다.</p><button type="button" data-recovery="review">저장본과 초안 다시 비교</button>';view.status.textContent='저장본 읽기 전용 · 초안 보관됨';updateControls();return;}
      if(choice==='review'){const canonical=await repository.get(tenant,page.pageId);await bindSession(canonical);showRecovery(draft,canonical);return;}
      if(isDeskRestoreBlocked())throw new DocumentUserError('자료함 복원 상태를 확인하는 동안 초안을 변경할 수 없습니다.');
      if(choice==='discard'){const id=page.pageId;confirm('초안 삭제',`<p>‘${html(draft.title)}’ 초안만 삭제합니다. 현재 저장본은 변경되지 않습니다.</p>`,async()=>{await repository.discardDraft(tenant,id,draft.generation);preservedDraft=null;pendingRecovery=null;await openPage(id);},true);return;}
      let recoveredId='';
      if(choice==='copy') {const result=await repository.mutate(tenant,page,'duplicate_draft',{generation:draft.generation});recoveredId=result.page?.pageId || '';if(!recoveredId)throw new DocumentUserError('복구 문서 생성을 확인하지 못했습니다. 초안은 보관하고 있습니다.');await repository.discardDraft(tenant,page.pageId,draft.generation);}
      const canonical=await repository.get(tenant,recoveredId || page.pageId);
      if(choice==='resume' && (draft.baseRevision!==documentRevision(canonical) || unsupportedDocumentNodes(draft.blocks).length)) {await bindSession(canonical);showRecovery(draft,canonical);throw new DocumentUserError('저장본이 변경되었거나 초안에 지원하지 않는 블록이 있습니다. 초안을 새 문서로 보관해 주세요.');}
      pendingRecovery=null;preservedDraft=null;view.recovery.hidden=true;
      await bindSession(canonical,choice==='resume'?draft.generation:0);
      if(choice==='resume') {page={...canonical,title:canonical.editPolicy?.titleEditable===false?canonical.title:draft.title,blocks:draft.blocks,markdown:draft.markdown};view.title.value=page.title;editor.mount(page);session!.change(page);updateControls();}
      await refreshPages();changed();
    });
    else if(target.dataset.restore)void run(async()=> {if(isDeskRestoreBlocked())throw new DocumentUserError('자료함 복원 상태를 확인하는 동안 문서를 복원할 수 없습니다.');const items=await repository.trash(tenant);const item=items.find(p=>p.pageId===target.dataset.restore);if(!item)return; await repository.mutate(tenant,item,'restore');await refreshPages();changed();await showTrash();});
    else if(target.dataset.version)void run(async()=>{assertWritable();await flush();const active=structuredClone(currentPage());const versionId=target.dataset.version!;const version=await repository.historyVersion(tenant,active.pageId,versionId);confirm('이 버전으로 복원',`<p>현재 내용도 수정 이력에 남습니다. 확인 중 문서가 변경되면 복원을 중단합니다.</p><div class="dw-recovery-compare"><article><header>현재 저장본</header><pre>${html(active.markdown)}</pre></article><article><header>복원할 버전</header><pre>${html(version.markdown)}</pre></article></div>`,()=>structureChange(async()=>{assertWritable();await repository.mutate(tenant,active,'restore_version',{versionId});await openPage(active.pageId,true);changed();}));});
    else if(target.dataset.moveParent!==undefined)void run(()=>structureChange(async()=> {assertWritable(true);await repository.mutate(tenant,currentPage(),'move',{parentId:target.dataset.moveParent || null});await openPage(currentPage().pageId,true);await refreshPages();changed();}));
  });
  view.dialog.querySelector('.dw-format')!.addEventListener('mousedown',event=> {
    const target=(event.target as Element).closest<HTMLElement>('button');if(!target)return;
    if(target.dataset.mark){event.preventDefault();if(readOnly || pendingRecovery || isDeskRestoreBlocked())return;editor.shortcut(target.dataset.mark);}
    if(target.dataset.block){event.preventDefault();if(readOnly || pendingRecovery || isDeskRestoreBlocked())return;editor.blockShortcut(target.dataset.block);}
  });
  view.title.addEventListener('input',()=> {if(page && !pendingRecovery && !readOnly && !isDeskRestoreBlocked()){page.title=view.title.value;view.heading.textContent=page.title || '제목 없음';session?.change(page);}});
  view.dialog.addEventListener('compositionstart',()=>session?.composition(true));
  view.dialog.addEventListener('compositionend',()=>session?.composition(false));
  view.dialog.addEventListener('keydown',event=>{
    if(!view.confirm.hidden){
      if(event.key==='Escape'){event.preventDefault();confirmation=null;view.confirm.hidden=true;view.title.focus();return;}
      if(event.key==='Tab'){const buttons=Array.from(view.confirm.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'));const first=buttons[0],last=buttons[buttons.length-1];if(event.shiftKey && document.activeElement===first){event.preventDefault();last?.focus();}else if(!event.shiftKey && document.activeElement===last){event.preventDefault();first?.focus();}return;}
    }
    if((event.ctrlKey || event.metaKey) && event.key.toLowerCase()==='s'){event.preventDefault();void run(flush);}
  });
  view.search.addEventListener('input',()=>{const generation=++searchGeneration;void repository.list(tenant,view.search.value).then(records=>{if(generation===searchGeneration)renderPages(records);}).catch(error);});
  window.addEventListener('beforeunload',event=>{if(session?.dirty() || pendingRecovery || nativeAttachmentPending || editor.hasBlockingUploads()){event.preventDefault();event.returnValue='';}});
  for(const [name,kind] of [['desk:create-document','create'],['desk:open-document','open'],['desk:open-trash','trash'],['desk:open-history','history']]) {
    document.addEventListener(name,event=>{void run(()=>begin((event as CustomEvent).detail || {},kind));});
  }
  window.addEventListener('desk:restore-lock-changed',updateControls);window.addEventListener('desk:favorites-changed',updateControls);
  return {async canLeave(){try{await flush();return true;}catch(reason){error(reason);confirm('저장을 확인한 뒤 이동하세요','<p>화면 이동이 중단되었습니다. 현재 내용과 초안은 그대로 유지됩니다.</p>',flush,false,'다시 저장','계속 작성');return false;}}, flush, open:(input:DocumentWorkspaceInput|string)=>run(()=>begin(typeof input==='string'?{pageId:input}:input,'open'))};
}
