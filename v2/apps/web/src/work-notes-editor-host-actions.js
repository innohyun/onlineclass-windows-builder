// Host controls share the editor's current instance and never recreate its document.
export function insertStoredWorkNoteAttachment(editor, editable, record, createBlockId) {
  if (!editor || !editable || !record?.attachmentId) return false;
  return editor.chain().focus().insertContent({ type: 'attachmentBlock', attrs: {
    blockId: record.blockId || createBlockId(), attachmentId: record.attachmentId, fileName: record.fileName,
    contentType: record.contentType, size: record.size, kind: record.kind || 'file', displayMode: record.displayMode || 'file',
  } }).run();
}

export function applyWorkNoteShortcut(editor, action, { editSelectedLink, blockAction }) {
  if (!editor) return false;
  const commands = {
    bold: () => editor.chain().focus().toggleBold().run(), italic: () => editor.chain().focus().toggleItalic().run(),
    underline: () => editor.chain().focus().toggleUnderline().run(), strike: () => editor.chain().focus().toggleStrike().run(),
    code: () => editor.chain().focus().toggleCode().run(), link: () => editSelectedLink(), duplicate: () => blockAction('duplicate'),
    moveUp: () => blockAction('up'), moveDown: () => blockAction('down'),
  };
  return commands[action]?.() ?? false;
}
