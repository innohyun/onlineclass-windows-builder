import { createDocumentRepository, documentError, documentRevision, type LocalDocument, type NativeRequest, type DocumentDraft, type HistoryCursor } from './document-repository';
import { createDocumentSaveSession, type SaveState } from './document-save-session.mjs';
import { createWorkNotesTiptapEditor, projectDocumentMarkdown } from './document-editor-bridge.mjs';
import { createDocumentWorkspaceView, escapeDocumentHtml as html, selectDocumentFiles, downloadDocument } from './document-workspace-view';
import { documentTemplates } from './document-templates';
import { unsupportedDocumentNodes, retainDocumentBlockMetadata } from './document-compatibility.mjs';
import './document-workspace.css';
import { open as chooseNativeFiles } from '@tauri-apps/plugin-dialog';

type WorkspaceOptions = {getTenantId:()=>string; isNative?:()=>boolean; onChanged?:()=>void; openTeacherHome?:(path:string)=>void; request?:NativeRequest};
type SaveSession = ReturnType<typeof createDocumentSaveSession>;
export function initDocumentWorkspace(options: WorkspaceOptions) {
  const view = createDocumentWorkspaceView();
  const repository = createDocumentRepository(options.request);
  let page: LocalDocument | null = null;
  let tenant = '';
  let pages: LocalDocument[] = [];
  let session: SaveSession | null = null;
  let workspace = 'workNotes';
  let pendingRecovery: DocumentDraft | null = null;
  let readOnly = false;
  let editingLocked = false;
  let nativeAttachmentPending = false;
  let originalBlocks: LocalDocument['blocks'] = [];
  let historyCursor:HistoryCursor|null=null;
  let operation: Promise<unknown> = Promise.resolve();
  let searchGeneration = 0;
  const error = (reason: unknown) => { view.alert.textContent = documentError(reason); view.alert.hidden = false;if(!view.dialog.open)view.dialog.showModal(); };
  const clearError = () => { view.alert.hidden = true; view.alert.textContent = ''; };
  const run = (task:()=>Promise<unknown>) => { operation = operation.catch(()=>{}).then(task).catch(error); return operation; };
  function status(state: SaveState) {
    view.status.dataset.state = state.state;
    view.status.textContent = ({saved:'내 PC에 저장됨',saving:'저장 중…',dirty:'변경 내용 저장 대기',composing:'입력 중…',error:'저장 확인 필요',draft_error:'초안 저장 확인 필요'} as Record<string,string>)[state.state] || '내 PC 문서';
    if (state.error) {
      error(state.error);
      const compare=document.createElement('button');compare.type='button';compare.dataset.docAction='compare-draft';compare.textContent='저장본과 내 초안 비교';view.alert.append(' ',compare);
    }
    if (state.state === 'saved') clearError();
  }
  const path = (id: string) => pages.find(item=>item.pageId===id)?.title || '문서';
  const editor = createWorkNotesTiptapEditor({
    element:view.editor, getPage:()=>page, getPages:()=>pages, pagePath:path, escapeHtml:html,
    isEditable:()=>Boolean(page && !pendingRecovery && !readOnly && !editingLocked), canUseAi:()=>false,
    normalizeSerializedBlocks:(blocks:LocalDocument['blocks'])=>retainDocumentBlockMetadata(blocks,originalBlocks),
    onChange:(next:LocalDocument)=>{ if (session && !pendingRecovery && !readOnly) session.change(next); },
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
    onAttachmentSettled:()=>{ if(page) session?.change(page); },
  });
  function changed() { options.onChanged?.(); document.dispatchEvent(new CustomEvent('desk:documents-changed')); }
  async function refreshPages() {
    pages = await repository.list(tenant);
    renderPages();
  }
  function renderPages(records=pages) {
    const sorted = [...records].sort((a,b)=>(a.position||0)-(b.position||0)||a.title.localeCompare(b.title,'ko'));
    const ids=new Set(sorted.map(item=>item.pageId));const visited=new Set<string>();const ordered:LocalDocument[]=[];
    const append=(item:LocalDocument)=>{if(visited.has(item.pageId))return;visited.add(item.pageId);ordered.push(item);for(const child of sorted.filter(candidate=>candidate.parentId===item.pageId))append(child);};
    for(const item of sorted.filter(candidate=>!candidate.parentId || !ids.has(candidate.parentId)))append(item);
    for(const item of sorted)append(item);
    const depth = (item:LocalDocument) => { let n=0; let id=item.parentId; const seen=new Set([item.pageId]); while(id && n<8 && !seen.has(id)) { seen.add(id); n++; id=pages.find(p=>p.pageId===id)?.parentId; } return n; };
    view.pages.innerHTML = ordered.length ? ordered.map(item=>`<button type="button" data-open-page="${html(item.pageId)}" aria-current="${item.pageId===page?.pageId?'page':'false'}" style="--depth:${depth(item)}"><span>${html(item.emoji || '▤')}</span><span>${html(item.title || '제목 없음')}</span></button>`).join('') : '<p class="dw-empty">문서가 없습니다. 새 문서를 만들어 보세요.</p>';
    editor.refreshPageLinks();
  }
  async function flush() {
    if(nativeAttachmentPending)throw new Error('파일 첨부가 끝난 뒤 이동하거나 앱을 닫아 주세요.');
    if (!session || !page) return;
    if (readOnly) return;
    if (pendingRecovery) return; // The durable draft can be reviewed on the next visit.
    if (session.isComposing()) throw new Error('한글 입력을 마친 뒤 저장해 주세요.');
    await editor.waitForPendingUploads(page.pageId);
    await editor.serializeCurrent({forceChange:true});
    await session.flush();
  }
  async function bindSession(record:LocalDocument, draftGeneration=0) {
    await editor.releaseRealtime();
    view.editor.replaceChildren();
    session?.close(); page = structuredClone(record);
    if(record.properties?.nodeKind==='folder'){
      session=null;readOnly=true;view.title.value=record.title;view.title.disabled=true;view.heading.textContent=record.title;
      view.editor.textContent='폴더 안의 노트를 선택해 주세요.';view.status.textContent='폴더';return;
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
    view.status.textContent='내 PC에서 불러옴'; view.status.dataset.state='saved';
    if(readOnly) {
      view.editor.innerHTML=`<p class="dw-alert">지원되지 않는 블록(${html(unsupported.join(', '))})이 있어 원문을 보존한 채 열람합니다. 최신 편집기에서 수정해 주세요.</p><pre class="dw-readonly">${html(page.markdown)}</pre>`;
    } else editor.mount(page);
  }
  function showRecovery(draft:DocumentDraft, canonical:LocalDocument) {
    pendingRecovery=draft; editor.setEditable(false); view.title.disabled=true;
    const conflict=draft.baseRevision !== documentRevision(canonical);
    view.recovery.hidden=false;
    view.recovery.innerHTML=`<strong>${conflict?'최신 문서와 다른 초안이 있습니다':'이전에 작성하던 초안이 있습니다'}</strong><p>초안 제목: ${html(draft.title)} · ${draft.markdown.length.toLocaleString()}자. 저장된 문서와 초안을 확인한 뒤 이어서 작성하세요.</p><details><summary>초안 내용 보기</summary><pre>${html(draft.markdown)}</pre></details><div>${conflict?'': '<button data-recovery="resume" type="button">초안 이어 쓰기</button>'}<button data-recovery="copy" type="button">초안을 새 문서로 보관</button><button data-recovery="canonical" type="button">현재 저장본 사용 · 초안 삭제</button></div>`;
  }
  async function openPage(pageId:string, alreadyFlushed=false) {
    if(!alreadyFlushed)await flush();
    const canonical=await repository.get(tenant,pageId);
    const draft=await repository.draft(tenant,pageId);
    pendingRecovery=null; view.recovery.hidden=true; view.panel.hidden=true; clearError();
    await bindSession(canonical); renderPages();
    if(draft && (draft.markdown!==canonical.markdown || draft.title!==canonical.title || JSON.stringify(draft.blocks)!==JSON.stringify(canonical.blocks))) showRecovery(draft,canonical);
    else if(draft) await repository.discardDraft(tenant,pageId,draft.generation);
  }
  async function createPage(parentId:string|null,title='제목 없음',markdown='') {
    const parent=pages.find(item=>item.pageId===parentId);
    if(parent?.properties?.nodeKind==='note')parentId=parent.parentId||null;
    const pageId=crypto.randomUUID();
    const body=markdown.trim()?projectDocumentMarkdown(pageId,markdown):{blocks:[{id:crypto.randomUUID(),type:'text',text:''}],markdown:''};
    const created=await repository.save(tenant,{pageId,parentId,title,emoji:'',properties:{nodeKind:'note'},position:Date.now(),...body},0);
    await refreshPages(); changed(); return created;
  }
  function currentPage() { if(!page) throw new Error('문서를 먼저 열어 주세요.'); return page; }
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
  async function close() { await flush(); await editor.releaseRealtime(); session?.close(); session=null; page=null; view.dialog.close(); }
  async function structureChange(task:()=>Promise<unknown>) {
    editingLocked=true;editor.setEditable(false);view.title.disabled=true;
    try {await flush();return await task();}
    finally {editingLocked=false;editor.setEditable(Boolean(page && !pendingRecovery && !readOnly));view.title.disabled=!page || readOnly || page.editPolicy?.titleEditable===false || Boolean(pendingRecovery || page.properties?.systemKind);}
  }
  async function action(name:string) {
    clearError();
    if(name==='close') return close();
    if(name==='save') return flush();
    if(name==='compare-draft') {
      const id=currentPage().pageId;await session?.checkpoint();
      const draft=await repository.draft(tenant,id);if(!draft)throw new Error('초안 저장을 확인하지 못했습니다. 현재 본문을 복사해 보관해 주세요.');
      const canonical=await repository.get(tenant,id);await bindSession(canonical);showRecovery(draft,canonical);return;
    }
    if(name==='panel-close') {view.panel.hidden=true;return;}
    if(name==='trash-list') return showTrash();
    if(name==='history') return showHistory();
    if(name==='history-more') return showHistory(true);
    if(name==='move') return movePanel();
    if(name==='favorite') {window.dispatchEvent(new CustomEvent('desk:toggle-favorite',{detail:{pageId:currentPage().pageId}}));return;}
    if(name==='attach') {
      if(options.request){await editor.insertFiles(await selectDocumentFiles());return;}
      await flush();nativeAttachmentPending=true;
      try {
      const selected=await chooseNativeFiles({multiple:true,directory:false,title:'문서에 첨부할 파일 선택'});
      for(const sourcePath of (Array.isArray(selected)?selected:selected?[selected]:[])) {
        const record=await repository.uploadPath(tenant,currentPage().pageId,sourcePath);
        const kind=record.contentType==='application/pdf'?'pdf':['image','video','audio'].find(value=>record.contentType.startsWith(`${value}/`)) || 'file';
        if(!editor.insertStoredAttachment({...record,kind,displayMode:record.size<=20*1024*1024 && kind!=='file'?'preview':'file'}))throw new Error('파일은 보관했지만 본문에 추가하지 못했습니다. 문서 편집 상태를 확인해 주세요.');
      }
      } finally {nativeAttachmentPending=false;}
      await flush();return;
    }
    if(name==='new' || name==='child') {await flush(); const created=await createPage(name==='child'?currentPage().pageId:null); return openPage(created.pageId);}
    if(name==='duplicate') {await flush(); const result=await repository.mutate(tenant,currentPage(),'duplicate'); await refreshPages(); changed(); if(result.page?.pageId) await openPage(result.page.pageId);return;}
    if(name==='trash') {await flush(); panel('휴지통으로 이동',`<p>‘${html(currentPage().title)}’ 문서와 하위 문서를 휴지통으로 옮깁니다. 30일 안에 복원할 수 있습니다.</p><button type="button" data-doc-action="confirm-trash">휴지통으로 이동</button>`);return;}
    if(name==='confirm-trash') {
      return structureChange(async()=>{await repository.mutate(tenant,currentPage(),'trash');
        session?.close();session=null;page=null; await editor.releaseRealtime(); view.title.value='';
        await refreshPages();changed();return showTrash();});
    }
    if(name==='export') {await flush(); const active=currentPage();downloadDocument(`${active.title || '문서'}.md`,active.markdown);return;}
    if(name==='import') {
      await flush(); const files=await selectDocumentFiles('.md,.markdown,text/markdown,text/plain',false); if(!files[0])return;
      if(files[0].size>800_000)throw new Error('Markdown 파일은 200,000자 이내로 나눠 가져와 주세요.');
      const source=await files[0].text(); const created=await createPage(page?.parentId || null,files[0].name.replace(/\.(?:md|markdown)$/i,''),source);
      return openPage(created.pageId);
    }
  }
  async function begin(detail:{tenantId?:string;workspace?:string;pageId?:string;templateId?:string},kind:string) {
    if(options.isNative && !options.isNative()) throw new Error('문서 작성은 Windows 앱에서 사용할 수 있습니다.');
    const nextTenant=detail.tenantId || options.getTenantId();
    if(!nextTenant)throw new Error('학급을 먼저 선택해 주세요.');
    if(tenant && nextTenant!==tenant) {await flush(); await editor.releaseRealtime();session?.close();session=null;page=null;}
    tenant=nextTenant; workspace=detail.workspace || 'workNotes';
    if(!view.dialog.open)view.dialog.showModal();
    view.panel.hidden=true;
    if(!page){view.title.disabled=true;view.title.value='';view.heading.textContent='문서를 선택해 주세요';view.editor.textContent='왼쪽 목록에서 문서를 열거나 새 문서를 만드세요.';}
    await refreshPages();
    if(kind==='create') {
      await flush(); const template=documentTemplates[detail.templateId || ''];
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
    if(target.dataset.docAction)void run(()=>action(target.dataset.docAction!));
    else if(target.dataset.openPage)void run(()=>openPage(target.dataset.openPage!));
    else if(target.dataset.historyPage)void run(async()=>{await openPage(target.dataset.historyPage!);await showHistory();});
    else if(target.dataset.previewVersion)void run(async()=> {
      const versionId=target.dataset.previewVersion!;const version=await repository.historyVersion(tenant,currentPage().pageId,versionId);
      panel('이전 내용 확인',`<strong>${html(version.title)}</strong><pre class="dw-version-body">${html(version.markdown)}</pre><p>현재 문서를 이 내용으로 복원합니다. 현재 내용도 이력에 남습니다.</p><button type="button" data-version="${html(versionId)}">이 버전으로 복원</button><button type="button" data-doc-action="history">이력 목록으로</button>`);
    });
    else if(target.dataset.recovery)void run(async()=> {
      const draft=pendingRecovery; if(!draft || !page)return;
      const choice=target.dataset.recovery;
      let recoveredId='';
      if(choice==='copy') {const result=await repository.mutate(tenant,page,'duplicate_draft',{generation:draft.generation});recoveredId=result.page?.pageId || '';if(!recoveredId)throw new Error('복구 문서 생성을 확인하지 못했습니다. 초안은 보관하고 있습니다.');}
      if(choice!=='resume') await repository.discardDraft(tenant,page.pageId,draft.generation);
      pendingRecovery=null;view.recovery.hidden=true;
      const canonical=await repository.get(tenant,recoveredId || page.pageId);
      if(choice==='resume' && draft.baseRevision!==documentRevision(canonical)) {
        await bindSession(canonical);showRecovery(draft,canonical);throw new Error('초안을 확인하는 동안 저장본이 변경되었습니다. 초안을 새 문서로 보관하거나 최신본과 비교해 주세요.');
      }
      await bindSession(canonical,choice==='resume'?draft.generation:0);
      if(choice==='resume') {page={...canonical,title:draft.title,blocks:draft.blocks,markdown:draft.markdown};view.title.value=page.title;editor.mount(page);session!.change(page);}
      await refreshPages();changed();
    });
    else if(target.dataset.restore)void run(async()=> {const items=await repository.trash(tenant);const item=items.find(p=>p.pageId===target.dataset.restore);if(!item)return; await repository.mutate(tenant,item,'restore');await refreshPages();changed();await showTrash();});
    else if(target.dataset.version)void run(()=>structureChange(async()=> {await repository.mutate(tenant,currentPage(),'restore_version',{versionId:target.dataset.version});await openPage(currentPage().pageId,true);changed();}));
    else if(target.dataset.moveParent!==undefined)void run(()=>structureChange(async()=> {await repository.mutate(tenant,currentPage(),'move',{parentId:target.dataset.moveParent || null});await openPage(currentPage().pageId,true);await refreshPages();changed();}));
  });
  view.dialog.querySelector('.dw-format')!.addEventListener('mousedown',event=> {
    const target=(event.target as Element).closest<HTMLElement>('button');if(!target)return;
    if(target.dataset.mark){event.preventDefault();editor.shortcut(target.dataset.mark);}
    if(target.dataset.block){event.preventDefault();editor.blockShortcut(target.dataset.block);}
  });
  view.title.addEventListener('input',()=> {if(page && !pendingRecovery && !readOnly){page.title=view.title.value;view.heading.textContent=page.title || '제목 없음';session?.change(page);}});
  view.dialog.addEventListener('compositionstart',()=>session?.composition(true));
  view.dialog.addEventListener('compositionend',()=>session?.composition(false));
  view.dialog.addEventListener('keydown',event=>{if((event.ctrlKey || event.metaKey) && event.key.toLowerCase()==='s'){event.preventDefault();void run(flush);}});
  view.search.addEventListener('input',()=>{const generation=++searchGeneration;void repository.list(tenant,view.search.value).then(records=>{if(generation===searchGeneration)renderPages(records);}).catch(error);});
  window.addEventListener('beforeunload',event=>{if(session?.dirty() || pendingRecovery || nativeAttachmentPending || editor.hasBlockingUploads()){event.preventDefault();event.returnValue='';}});
  for(const [name,kind] of [['desk:create-document','create'],['desk:open-document','open'],['desk:open-trash','trash'],['desk:open-history','history']]) {
    document.addEventListener(name,event=>{void run(()=>begin((event as CustomEvent).detail || {},kind));});
  }
  return {async canLeave(){try{await flush();return true;}catch(reason){error(reason);return false;}}, flush, open:(pageId:string)=>run(()=>begin({pageId},'open'))};
}
