export const escapeDocumentHtml = (value: unknown) => String(value ?? '').replace(/[&<>"']/g, (c) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]!));
export function createDocumentWorkspaceView(options:{mount?:HTMLElement}={}) {
  const dialog = document.createElement('dialog');
  dialog.className = 'document-workspace' + (options.mount?' dw-inline':'');
  dialog.setAttribute('aria-label', '로컬 문서 편집');
  dialog.innerHTML = `
    <header class="dw-header"><button type="button" data-doc-action="close" aria-label="자료 목록으로 돌아가기">‹ 목록</button><div><span id="dwWorkspaceName" class="dw-eyebrow">업무 노트</span><strong id="dwHeading">문서 편집</strong></div>
      <span id="dwSaveState" role="status" aria-live="polite">이 PC 문서</span>
      <button type="button" data-doc-action="save" class="dw-primary">저장 <kbd>Ctrl S</kbd></button>
      <button type="button" data-doc-action="details" aria-controls="dwDetails" aria-expanded="true" aria-label="문서 정보 패널 접기">정보 ‹</button>
      <div class="dw-menu-wrap"><button type="button" data-doc-action="menu" aria-label="문서 메뉴" aria-controls="dwMenu" aria-expanded="false">···</button>
        <div id="dwMenu" class="dw-actions" role="toolbar" aria-label="문서 작업" hidden>
          <button data-doc-action="favorite" type="button">☆ 즐겨찾기 추가</button><button data-doc-action="history" type="button">수정 이력</button>
          <button data-doc-action="export" type="button">전체 Markdown 내보내기</button><button data-doc-action="export-json" type="button">원문 JSON 내보내기</button>
          <hr><button data-doc-action="duplicate" type="button">복제</button><button data-doc-action="child" type="button">하위 문서 추가</button>
          <button data-doc-action="move" type="button">문서 이동</button><button data-doc-action="trash" type="button">휴지통으로 이동</button>
          <p id="dwStructureNotice" hidden>수업에 연결된 문서의 제목·구조는 수업계획에서 관리합니다.</p>
        </div></div></header>
    <div class="dw-alert" id="dwAlert" role="alert" hidden></div>
    <div class="dw-layout"><aside class="dw-nav"><div class="dw-nav-heading"><strong id="dwNavTitle">업무 노트</strong></div>
      <input id="dwSearch" type="search" aria-label="로컬 문서 찾기" placeholder="문서 찾기">
      <nav id="dwPages" aria-label="자료함 문서"></nav>
      <button type="button" data-doc-action="new">＋ 새 문서</button><button type="button" data-doc-action="import">Markdown 가져오기</button></aside>
    <main class="dw-main"><div class="dw-tabs" role="tablist" aria-label="문서 보기"><button type="button" role="tab" aria-selected="true" data-doc-action="body">▤ 본문</button><button type="button" role="tab" aria-selected="false" data-doc-action="attachments">♧ 첨부 <span id="dwAttachmentCount">0</span></button><button type="button" role="tab" aria-selected="false" data-doc-action="history">◷ 이력</button></div>
      <section id="dwRecovery" class="dw-recovery" hidden aria-label="미저장 초안 복구"></section>
      <div class="dw-paper"><div class="dw-document-label" id="dwDocumentLabel">업무 노트</div>
        <input id="dwTitle" class="dw-title" aria-label="문서 제목" placeholder="제목 없음" maxlength="240">
        <div class="dw-format" role="toolbar" aria-label="본문 서식">
          <button type="button" data-block="0">본문 문단</button><button type="button" data-mark="bold" title="굵게">B</button><button type="button" data-mark="italic" title="기울임">I</button>
          <button type="button" data-block="1">제목</button><button type="button" data-block="5">목록</button><button type="button" data-block="4">체크 목록</button>
          <button type="button" data-block="7">접기</button><button type="button" data-doc-action="attach">파일 첨부</button>
        </div><p class="dw-help">/ 로 블록 추가 · [[ 로 문서 연결 · 끌어넣기·미리보기 20 MiB까지</p>
        <div id="editor" aria-label="문서 본문"></div>
        <p class="dw-storage-note">ⓘ 문서 저장과 백업·동기화는 별도로 진행됩니다.</p>
      </div></main>
    <aside id="dwDetails" class="dw-details" aria-label="문서 정보와 첨부파일"></aside>
    <aside id="dwPanel" class="dw-panel" hidden aria-label="문서 상세 작업"></aside></div>
    <div id="dwConfirm" class="dw-confirm-overlay" hidden><section role="dialog" aria-modal="true" aria-labelledby="dwConfirmTitle"></section></div>
    <div id="slashMenu" class="dw-floating slash-menu"><div id="slashCommands"></div></div>
    <div id="inlineMenu" class="dw-floating inline-menu"></div>
    <div id="selectionToolbar" class="dw-floating selection-toolbar">${[['bold','굵게'],['italic','기울임'],['underline','밑줄'],['strike','취소선'],['code','코드'],['link','링크']].map(([key,label])=>`<button type="button" data-mark="${key}">${label}</button>`).join('')}</div>
    <div id="tableToolbar" class="dw-floating table-toolbar">${[['rowAfter','행 추가'],['colAfter','열 추가'],['deleteRow','행 삭제'],['deleteCol','열 삭제'],['deleteTable','표 삭제']].map(([key,label])=>`<button type="button" data-table="${key}">${label}</button>`).join('')}</div>
    <div id="blockRangeToolbar" class="dw-floating block-range-toolbar"><span id="blockRangeCount"></span>${[['up','위로'],['down','아래로'],['indent','들여쓰기'],['outdent','내어쓰기'],['duplicate','복제'],['delete','삭제']].map(([key,label])=>`<button type="button" data-block-range="${key}">${label}</button>`).join('')}</div>
    <div id="blockHandle" class="dw-block-handle"><button type="button" data-block-add aria-label="블록 추가">＋</button><button type="button" data-block-grip aria-label="블록 이동과 메뉴">⠿</button></div>
    <div id="blockDropIndicator"></div><div id="blockMenu" class="dw-floating block-menu"></div>`;
  (options.mount || document.body).append(dialog);
  const find = <T extends HTMLElement>(id: string) => dialog.querySelector<T>(`#${id}`)!;
  return {dialog, show:()=>{if(options.mount)dialog.setAttribute('open','');else if(!dialog.open)dialog.showModal();}, hide:()=>{if(options.mount)dialog.removeAttribute('open');else dialog.close();}, title:find<HTMLInputElement>('dwTitle'), editor:find('editor'), heading:find('dwHeading'),
    status:find('dwSaveState'), alert:find('dwAlert'), pages:find('dwPages'), search:find<HTMLInputElement>('dwSearch'),
    panel:find('dwPanel'), details:find('dwDetails'), menu:find('dwMenu'), confirm:find('dwConfirm'), recovery:find('dwRecovery'), label:find('dwDocumentLabel')};
}
export function selectDocumentFiles(accept = '', multiple = true): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement('input'); input.type = 'file'; input.accept = accept; input.multiple = multiple;
    input.addEventListener('change', () => { resolve(Array.from(input.files || [])); input.remove(); }, {once:true});
    input.addEventListener('cancel', () => { resolve([]); input.remove(); }, {once:true});
    input.style.display = 'none'; document.body.append(input); input.click();
  });
}
export function downloadDocument(name: string, text: string, type='text/markdown;charset=utf-8') {
  const url = URL.createObjectURL(new Blob([text], {type}));
  const link = document.createElement('a'); link.href = url; link.download = name.replace(/[<>:"/\\|?*]/g,'_');
  link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
}
