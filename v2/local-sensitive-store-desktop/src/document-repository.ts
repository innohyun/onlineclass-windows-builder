import { invoke } from '@tauri-apps/api/core';

export type DocumentBlock = Record<string, any>;
export type LocalDocument = {
  tenantId?: string; pageId: string; parentId?: string | null; title: string; emoji?: string;
  position?: number; properties: Record<string, any>; blocks: DocumentBlock[]; markdown: string;
  updatedAtMs?: number; revision?: number; expectedRevision?: number; systemKind?: string;
  editPolicy?: {titleEditable:boolean;structureEditable:boolean};
};
export type DocumentDraft = LocalDocument & { baseRevision: number; generation: number };
export type HistoryCursor = {capturedAtMs:number;versionId:string};
export type DocumentVersion = { versionId: string; title: string; capturedAtMs: number; documentUpdatedAtMs:number;byteSize:number };
export type DocumentHistory = {items:DocumentVersion[];nextCursor:HistoryCursor|null;hasMore:boolean};
export type DocumentAttachment = { attachmentId: string; pageId: string; blockId: string; fileName: string; contentType: string; size: number };
export type NativeRequest = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
type Result = { ok?: boolean; error?: string; verified?: boolean; revision?: number; [key: string]: any };

const errors: Record<string, string> = {
  work_note_revision_conflict: '다른 창이나 기기에서 이 문서가 변경됐습니다. 작성한 내용은 초안에 보관했습니다. 최신본과 비교해 주세요.',
  work_note_system_folder_protected: '이 페이지는 수업 또는 시스템에서 관리합니다. 연결된 수업 화면에서 구조를 변경해 주세요.',
  work_note_parent_cycle: '하위 페이지 안으로 상위 페이지를 옮길 수 없습니다.',
  work_note_metadata_too_large: '문서 제목은 240자, 아이콘은 16자 이내로 입력해 주세요.',
  work_note_cloud_material_read_only: '학생 학습자료는 기존 교사 홈에서 편집해 주세요. 로컬 자료함에서는 열람할 수 있습니다.',
  work_note_draft_generation_conflict: '더 최근에 작성한 초안이 있습니다. 저장본과 초안을 다시 확인해 주세요.',
  work_note_not_found: '문서를 찾을 수 없습니다. 휴지통과 현재 자료함을 확인해 주세요.',
  work_note_attachment_too_large: '끌어넣기는 20 MiB까지 지원합니다. 큰 파일은 상단 ‘파일 첨부’에서 선택해 주세요.',
};
export function documentError(error: unknown): string {
  const code = String((error as { code?: string; message?: string })?.code || (error as Error)?.message || error);
  return errors[code] || (code.startsWith('work_note_') ? '문서를 처리하지 못했습니다. 작성한 내용을 유지한 채 다시 시도해 주세요.' : code);
}
export const documentRevision = (page: LocalDocument) => Number(page.revision ?? page.updatedAtMs ?? 0);

export function createDocumentRepository(request: NativeRequest = invoke) {
  async function checked(command: string, args: Record<string, unknown>): Promise<Result> {
    const result = await request<Result>(command, args);
    if (!result || result.ok !== true) {
      const code = result?.error || 'work_note_request_failed';
      throw Object.assign(new Error(documentError(code)), { code });
    }
    return result;
  }
  function pageResult(result: Result, pageId?: string): LocalDocument {
    const page = result.page;
    if (!page || (pageId && page.pageId !== pageId)) throw new Error('저장한 문서의 확인 결과가 일치하지 않습니다.');
    return { ...page, ...(result.editPolicy?{editPolicy:result.editPolicy}:{}), properties: page.properties || {}, blocks: page.blocks || [], markdown: page.markdown || '',
      revision: Number(result.revision ?? page.revision ?? page.updatedAtMs ?? 0) };
  }
  return {
    async ensureWorkspace(tenantId: string, workspace: string): Promise<LocalDocument> {
      return pageResult(await checked('ensure_local_work_note_workspace', { tenantId, workspace }));
    },
    async list(tenantId: string, query = ''): Promise<LocalDocument[]> {
      return (await checked('list_local_work_note_documents', { tenantId, query })).items || [];
    },
    async get(tenantId: string, pageId: string): Promise<LocalDocument> {
      return pageResult(await checked('get_local_work_note_document', { tenantId, pageId }), pageId);
    },
    async save(tenantId: string, page: LocalDocument, expectedRevision: number): Promise<LocalDocument> {
      const result = await checked('save_local_work_note_document', { input: { ...page, tenantId, expectedRevision } });
      if (result.verified !== true) throw new Error('저장 결과를 확인하지 못했습니다. 내용을 보관한 채 다시 확인해 주세요.');
      return pageResult(result, page.pageId);
    },
    async mutate(tenantId: string, page: LocalDocument, action: string, extra: Record<string, unknown> = {}) {
      const result=await checked('mutate_local_work_note_document', { input: { tenantId, pageId: page.pageId,
        expectedRevision: documentRevision(page), action, ...extra } });
      if(result.verified!==true)throw new Error('문서 변경 결과를 확인하지 못했습니다. 현재 문서를 다시 확인해 주세요.');
      return result;
    },
    async trash(tenantId: string): Promise<LocalDocument[]> {
      return (await checked('list_local_work_note_trash', { tenantId })).items || [];
    },
    async history(tenantId: string, pageId: string, cursor:HistoryCursor|null=null): Promise<DocumentHistory> {
      const result=await checked('list_local_work_note_history', { tenantId, pageId,limit:50,cursor });
      return {items:result.items || [],nextCursor:result.nextCursor || null,hasMore:result.hasMore===true};
    },
    async historyVersion(tenantId:string,pageId:string,versionId:string):Promise<LocalDocument> {
      return pageResult(await checked('get_local_work_note_version',{tenantId,pageId,versionId}),pageId);
    },
    async saveDraft(tenantId: string, draft: DocumentDraft) {
      return checked('save_local_work_note_draft', { input: { ...draft, tenantId } });
    },
    async draft(tenantId: string, pageId: string): Promise<DocumentDraft | null> {
      return (await checked('get_local_work_note_draft', { tenantId, pageId })).draft || null;
    },
    async discardDraft(tenantId: string, pageId: string, generation: number) {
      return checked('discard_local_work_note_draft', { tenantId, pageId, generation });
    },
    async attachments(tenantId: string, pageId = ''): Promise<DocumentAttachment[]> {
      return (await checked('list_local_work_note_attachments', { tenantId, pageId })).items || [];
    },
    async uploadPath(tenantId:string,pageId:string,sourcePath:string):Promise<DocumentAttachment> {
      const fileName=sourcePath.split(/[\\/]/).pop() || '첨부파일';
      const types:Record<string,string>={pdf:'application/pdf',png:'image/png',jpg:'image/jpeg',jpeg:'image/jpeg',gif:'image/gif',webp:'image/webp',txt:'text/plain',md:'text/markdown',mp4:'video/mp4',mp3:'audio/mpeg',wav:'audio/wav'};
      const result=await checked('save_local_work_note_attachment',{input:{tenantId,pageId,sourcePath,fileName,
        attachmentId:crypto.randomUUID(),blockId:crypto.randomUUID(),contentType:types[fileName.split('.').pop()?.toLowerCase() || ''] || 'application/octet-stream'}});
      if(!result.attachment?.attachmentId)throw new Error('첨부파일 저장을 확인하지 못했습니다.');
      return result.attachment;
    },
    async openAttachment(tenantId:string,attachmentId:string) {
      return checked('open_local_data_attachment',{tenantId,mediaId:attachmentId,attachmentKind:'work-note'});
    },
    async upload(tenantId: string, pageId: string, blockId: string, file: File, attachmentId: string): Promise<DocumentAttachment> {
      if (file.size > 20 * 1024 * 1024) throw Object.assign(new Error(errors.work_note_attachment_too_large), { code: 'work_note_attachment_too_large' });
      const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
      const result = await checked('save_local_work_note_attachment', { input: { tenantId, pageId, attachmentId,
        blockId, fileName: file.name, contentType: file.type || 'application/octet-stream', bytes } });
      if (!result.attachment?.attachmentId) throw new Error('첨부파일 저장을 확인하지 못했습니다.');
      return result.attachment;
    },
    async attachmentBlob(tenantId: string, attachmentId: string): Promise<Blob> {
      const result = await checked('read_local_work_note_attachment', { tenantId, attachmentId });
      const bytes = Uint8Array.from(atob(result.base64), (character) => character.charCodeAt(0));
      return new Blob([bytes], { type: result.contentType || 'application/octet-stream' });
    },
    async removeAttachment(tenantId: string, attachmentId: string) {
      return checked('delete_local_work_note_attachment', { tenantId, attachmentId });
    },
  };
}
export type DocumentRepository = ReturnType<typeof createDocumentRepository>;
