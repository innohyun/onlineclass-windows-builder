export async function createAndLinkWorkNotePage({ createPage, insertLink, flush, openPage }) {
  const child = await createPage();
  if (!child?.pageId || insertLink(child) !== true) {
    throw Object.assign(new Error("새 페이지는 만들었지만 현재 문서의 삽입 위치가 바뀌어 링크를 저장하지 못했습니다. 왼쪽 페이지 목록에서 새 페이지를 열 수 있습니다."), {
      code: "WORK_NOTE_PAGE_LINK_INSERT_FAILED",
    });
  }
  await flush();
  await openPage(child.pageId);
  return child;
}

export function createWorkNoteLinkedPageFlow(options) {
  return async function createLinkedPage(range = null) {
    const sourceEditor = options.getEditor();
    const sourcePageId = options.getPageId();
    const sourceGeneration = options.getGeneration();
    const sourceDoc = sourceEditor.state.doc;
    const expectedText = range ? sourceDoc.textBetween(range.from, range.to, "\n", "\n") : "";
    return createAndLinkWorkNotePage({
      createPage: () => options.createPage(sourcePageId, "제목 없음", { open: false }),
      insertLink: (child) => {
        const editor = options.getEditor();
        if (editor !== sourceEditor || options.getPageId() !== sourcePageId || options.getGeneration() !== sourceGeneration) return false;
        if (range) {
          if (range.from < 0 || range.to > editor.state.doc.content.size
            || editor.state.doc.textBetween(range.from, range.to, "\n", "\n") !== expectedText) return false;
        } else if (editor.state.doc !== sourceDoc) return false;
        let chain = editor.chain().focus();
        if (range) chain = chain.deleteRange(range);
        return chain.insertContent({
          type: "pageLinkBlock",
          attrs: { pageId: child.pageId, title: child.title, blockId: options.createBlockId() },
        }).run();
      },
      flush: options.flush,
      openPage: options.openPage,
    });
  };
}
