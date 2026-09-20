import type { LocalDocument, DocumentRepository } from './document-repository';
export type SaveState = { state: string; error?: unknown; dirty: boolean; generation: number };
export function createDocumentSaveSession(options: {
  tenantId: string; page: LocalDocument; repository: DocumentRepository; draftGeneration?: number;
  onState?: (state: SaveState) => void; onSaved?: (page: LocalDocument, detail: { latest: boolean }) => void;
}): {
  change(page: LocalDocument): void; composition(active: boolean): void; flush(): Promise<void>;
  checkpoint(): Promise<void>; snapshot(): LocalDocument; revision(): number; dirty(): boolean;
  isComposing(): boolean; generation(): number; close(): void;
};
