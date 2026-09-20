import type { DocumentBlock } from './document-repository';
export function unsupportedDocumentNodes(blocks:DocumentBlock[]):string[];
export function retainDocumentBlockMetadata(blocks:DocumentBlock[], originals:DocumentBlock[]):DocumentBlock[];
