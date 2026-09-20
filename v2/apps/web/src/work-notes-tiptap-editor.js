import { Editor } from "@tiptap/core";
import { applyWorkNoteShortcut, insertStoredWorkNoteAttachment } from './work-notes-editor-host-actions.js';
import Collaboration, { isChangeOrigin } from "@tiptap/extension-collaboration";
import { Details, DetailsContent, DetailsSummary } from "@tiptap/extension-details";
import Highlight from "@tiptap/extension-highlight";
import Link from "@tiptap/extension-link";
import { TaskItem, TaskList } from "@tiptap/extension-list";
import Placeholder from "@tiptap/extension-placeholder";
import StarterKit from "@tiptap/starter-kit";
import { TableKit } from "@tiptap/extension-table";
import { TextStyleKit } from "@tiptap/extension-text-style";
import Underline from "@tiptap/extension-underline";
import * as Y from "yjs";
import { createAttachmentBlock } from "./work-notes-attachment-node.js";
import { deleteWorkNoteAttachmentBlock, deleteWorkNoteBlockRangeAttachments, handleWorkNoteAttachmentDeleteKey } from "./work-notes-attachment-block-actions.js";
import { createWorkNoteAttachmentUploadController } from "./work-notes-attachment-upload-controller.js";
import { createWorkNoteBlockInteractions, workNoteFloatingMenuPosition } from "./work-notes-block-interactions.js";
import {
  canMoveWorkNoteNestedBlock,
  canWorkNoteNestedBlockAction,
  changeWorkNoteNestedBlockIndent,
  duplicateWorkNoteNestedBlock,
  insertWorkNoteNestedBlockAfter,
  moveWorkNoteNestedBlock,
  moveWorkNoteNestedBlockByDirection,
  transformWorkNoteNestedBlock,
  workNoteBlockIdentityTransaction,
} from "./work-notes-nested-blocks.js";
import { workNotePastedUrl } from "./work-notes-url-paste.js";
import { applyWorkNoteAiProposal, captureWorkNoteAiTarget } from "./work-note-ai-editor-target.js";
import { createWorkNoteRealtimeEditorLifecycle } from "./work-note-realtime-editor-lifecycle.js";
import { createWorkNoteLinkedPageFlow } from "./work-note-linked-page.js";
import { handleWorkNoteListTabKey } from "./work-notes-list-keyboard.js";
import { flattenWorkNoteCommands } from "./work-notes-slash-commands.js";
import { assertWorkNoteProjectionReceipt, hasMeaningfulWorkNoteBlocks } from "./work-note-projection-safety.js";
import { BlockIdentity, Callout, PageLinkBlock, UserMention } from "./work-notes-tiptap-nodes.js";
import { focusWorkNoteMention, workNoteDateValue, workNoteInlineOptions, workNoteMentionOptions, workNoteUserMentionContent } from './work-notes-inline-options.js';
import {
  canMoveWorkNoteBlockRangeToIndex,
  changeWorkNoteBlockIndent,
  clearWorkNoteBlockRange,
  createWorkNoteBlockRangeExtension,
  duplicateWorkNoteBlockRange,
  getWorkNoteBlockRange,
  moveWorkNoteBlockRange,
  moveWorkNoteBlockRangeToIndex,
  setWorkNoteBlockRange,
} from "./work-notes-block-range.js";
import {
  blockId,
  blocksToTiptapDocument,
  paragraph,
  textNode,
  tiptapDocumentToBlocks,
  tiptapDocumentToMarkdown,
} from "./work-notes-tiptap-serialization.js";
export {
  blocksToTiptapDocument,
  tiptapDocumentToBlocks,
  tiptapDocumentToMarkdown,
} from "./work-notes-tiptap-serialization.js";
export { createAndLinkWorkNotePage } from "./work-note-linked-page.js";

export function isWorkNoteRemoteTransaction(transaction) {
  return Boolean(transaction && isChangeOrigin(transaction));
}

export function classifyWorkNoteTransaction(transaction) {
  let current = transaction;
  let docChanged = false;
  let remote = false;
  const seen = new Set();
  while (current && !seen.has(current)) {
    seen.add(current);
    docChanged ||= current.docChanged === true;
    remote ||= isWorkNoteRemoteTransaction(current);
    current = current.getMeta?.("appendedTransaction") || null;
  }
  return { docChanged, remote };
}

export function createWorkNotesTiptapEditor(options) {
  const canEdit = () => options.isEditable ? options.isEditable() === true : options.editable !== false;
  const canUseAi = () => canEdit() && Boolean(options.onOpenAi) && (options.canUseAi?.() ?? true);
  const ui = {
    slashMenu: document.getElementById("slashMenu"),
    slashCommands: document.getElementById("slashCommands"),
    inlineMenu: document.getElementById("inlineMenu"),
    selectionToolbar: document.getElementById("selectionToolbar"),
    tableToolbar: document.getElementById("tableToolbar"),
    blockRangeToolbar: document.getElementById("blockRangeToolbar"),
    blockRangeCount: document.getElementById("blockRangeCount"),
    blockHandle: document.getElementById("blockHandle"),
    blockDropIndicator: document.getElementById("blockDropIndicator"),
    blockMenu: document.getElementById("blockMenu"),
  };
  let editor = null;
  let ydoc = null;
  let realtime = null;
  let applyingRemoteUpdate = 0;
  let bootstrapping = false;
  let mountGeneration = 0;
  let localMutationVersion = 0;
  let pageId = "";
  let menu = null;
  let menuIndex = 0;
  let rangeAnchorIndex = -1;
  let blockInteractions = null;
  let applying = false;
  const createLinkedPage = createWorkNoteLinkedPageFlow({
    getEditor: () => editor,
    getPageId: () => pageId,
    getGeneration: () => mountGeneration,
    createBlockId: blockId,
    createPage: (parentId, title, settings) => options.createPage(parentId, title, settings),
    flush: () => options.flush(),
    openPage: (childPageId) => options.openPage(childPageId),
  });
  const attachmentUploads = createWorkNoteAttachmentUploadController({
    getEditor: () => editor,
    getPageId: () => pageId,
    getDocumentId: () => options.getDocumentId?.() || "",
    getGeneration: () => mountGeneration,
    canEdit,
    uploadAttachment: options.uploadAttachment,
    removeAttachment: options.removeAttachment,
    storedCopy: options.attachmentStoredCopy,
    onStatus: options.onAttachmentStatus,
    onError: options.onAttachmentError,
    onSettled: options.onAttachmentSettled,
  });

  const pages = () => [...options.getPages()];
  const closeMenus = () => {
    menu = null;
    ui.slashMenu.style.display = "none";
    ui.inlineMenu.style.display = "none";
    ui.blockMenu.style.display = "none";
    blockInteractions?.finishMobileMove();
  };
  const positionMenu = (element, rect, width) => {
    element.style.visibility = "hidden";
    element.style.display = "block";
    const menuRect = { width: element.offsetWidth || width, height: element.offsetHeight };
    const position = workNoteFloatingMenuPosition(rect, menuRect, { width: innerWidth, height: innerHeight });
    element.style.left = `${position.left}px`;
    element.style.top = `${position.top}px`;
    element.style.removeProperty("visibility");
  };

  function renderSlash() {
    const needle = menu.query.trim().toLowerCase();
    const matches = flattenWorkNoteCommands().filter(({ command }) => (command[0] !== "ai" || canUseAi())
      && (!needle || `${command[1]} ${command[2]} ${command[3]}`.toLowerCase().includes(needle)));
    menu.items = matches;
    menuIndex = Math.min(menuIndex, Math.max(0, matches.length - 1));
    ui.slashCommands.replaceChildren();
    let previousGroup = "";
    matches.forEach(({ group, command }, index) => {
      if (group !== previousGroup) {
        const heading = document.createElement("div");
        heading.className = "menu-heading";
        heading.textContent = group;
        ui.slashCommands.append(heading);
        previousGroup = group;
      }
      const button = document.createElement("button");
      button.type = "button";
      button.className = `slash-command${index === menuIndex ? " active" : ""}`;
      button.innerHTML = `<span class="command-icon"><i class="fa-solid ${command[4]}"></i></span><span class="command-copy"><b>${options.escapeHtml(command[1])}</b><small>${options.escapeHtml(command[3])}</small></span>`;
      button.addEventListener("mousedown", (event) => { event.preventDefault(); applySlash(command[0]); });
      ui.slashCommands.append(button);
    });
  }

  function showSlash(query, from, to) {
    const rect = editor.view.coordsAtPos(to);
    menu = { type: "slash", query, from, to, items: [] };
    menuIndex = 0;
    renderSlash();
    positionMenu(ui.slashMenu, rect, 320);
  }

  function inlineOptions(mode, query) { return workNoteInlineOptions({ mode, query, pages: pages(), pageId, pagePath: options.pagePath }); }

  async function loadExternalInline(activeMenu) {
    if (options.searchMentionCandidates && activeMenu.mode === 'at') {
      try {
        const result = await options.searchMentionCandidates(activeMenu.query.trim());
        if (menu !== activeMenu) return;
        activeMenu.items = [...workNoteMentionOptions(result), ...activeMenu.items];
        renderInline({ preserveItems: true });
      } catch {
        // 날짜와 페이지 선택은 멘션 후보 API가 일시적으로 실패해도 계속 사용할 수 있다.
      }
      return;
    }
    if (!options.searchPages || activeMenu.mode !== 'brackets' || !activeMenu.query.trim()) return;
    try {
      const records = await options.searchPages(activeMenu.query.trim());
      if (menu !== activeMenu) return;
      const localKeys = new Set(activeMenu.items.filter((item) => item.page).map((item) => `${item.page.documentId || ''}:${item.page.pageId}`));
      const localPageIds = new Set(activeMenu.items.filter((item) => item.page).map((item) => item.page.pageId));
      const external = records.filter((record) => !(record.storageKind === 'local' && localPageIds.has(record.pageId))
        && !localKeys.has(`${record.documentId}:${record.pageId}`)).slice(0, 7).map((record) => ({
        group: '페이지', icon: 'fa-link', label: record.title, copy: record.storageKind === 'local' ? record.documentTitle : `${record.documentTitle} · 업무 노트`, action: 'link',
        page: { ...record, external: true },
      }));
      activeMenu.items.splice(Math.max(0, activeMenu.items.length - 1), 0, ...external);
      renderInline({ preserveItems: true });
    } catch {
      // 로컬 page 연결은 검색 API가 일시적으로 실패해도 계속 사용할 수 있다.
    }
  }

  function renderInline({ preserveItems = false } = {}) {
    if (!preserveItems) menu.items = inlineOptions(menu.mode, menu.query);
    menuIndex = Math.min(menuIndex, Math.max(0, menu.items.length - 1));
    ui.inlineMenu.replaceChildren();
    let previousGroup = '';
    menu.items.forEach((item, index) => {
      if (item.group !== previousGroup) {
        const heading = document.createElement('div');
        heading.className = 'menu-heading';
        heading.textContent = item.group;
        ui.inlineMenu.append(heading);
        previousGroup = item.group;
      }
      const button = document.createElement("button");
      button.type = "button";
      button.className = `inline-option${index === menuIndex ? " active" : ""}`;
      button.innerHTML = `<i class="fa-solid ${item.icon}"></i><span><b>${options.escapeHtml(item.label)}</b><small>${options.escapeHtml(item.copy)}</small></span>`;
      button.addEventListener("mousedown", (event) => { event.preventDefault(); applyInline(item); });
      ui.inlineMenu.append(button);
    });
  }

  function showInline(mode, query, from, to) {
    const rect = editor.view.coordsAtPos(to);
    menu = { type: "inline", mode, query, from, to, items: [] };
    menuIndex = 0;
    renderInline();
    positionMenu(ui.inlineMenu, rect, 310);
    void loadExternalInline(menu);
  }

  function detectTrigger() {
    if (!editor || applying || !editor.isEditable) return;
    const { $from, from, to } = editor.state.selection;
    if (from !== to || !$from.parent.isTextblock) return closeMenus();
    const before = $from.parent.textBetween(0, $from.parentOffset, "\n", "\n");
    const start = from - before.length;
    let match = before.match(/(?:^|\s)\/([^\s/]*)$/u);
    if (match) return showSlash(match[1], from - match[0].trimStart().length, from);
    match = before.match(/\[\[([^\]]*)$/u);
    if (match) return showInline("brackets", match[1], from - match[0].length, from);
    match = before.match(/(?:^|\s)@([^\s@]*)$/u);
    if (match) return showInline("at", match[1], from - match[0].trimStart().length, from);
    match = before.match(/(?:^|\s)\+([^\s+]*)$/u);
    if (match) return showInline("plus", match[1], from - match[0].trimStart().length, from);
    if (menu?.type === "slash" || menu?.type === "inline") closeMenus();
  }

  async function applyInline(item) {
    if (!menu || menu.type !== "inline") return;
    const active = menu;
    applying = true;
    try {
      if (item.action === "date") {
        editor.chain().focus().deleteRange({ from: active.from, to: active.to }).insertContent(workNoteDateValue(item.value)).run();
      } else if (item.action === 'user') {
        editor.chain().focus().deleteRange({ from: active.from, to: active.to })
          .insertContent(workNoteUserMentionContent(item.candidate)).insertContent(' ').run();
      } else {
        let page = item.page;
        if (!page) page = await options.createPage(item.action === "root" ? null : pageId, active.query.trim() || "제목 없음", { open: false });
        const href = page.externalHref || (page.external ? `worknote://cloud/${page.documentId}/${page.pageId}` : `worknote://${page.pageId}`);
        editor.chain().focus().deleteRange({ from: active.from, to: active.to }).insertContent({ type: "text", text: page.title, marks: [{ type: "link", attrs: { href, title: page.title, target: null, rel: null, class: "internal-page-link" } }] }).insertContent(" ").run();
      }
    } finally {
      applying = false;
      closeMenus();
    }
  }

  function droppedFileKind(file) {
    const type = String(file?.type || "").toLowerCase();
    const name = String(file?.name || "").toLowerCase();
    if (type.startsWith("image/")) return "image";
    if (type === "application/pdf" || name.endsWith(".pdf")) return "pdf";
    if (type.startsWith("video/")) return "video";
    if (type.startsWith("audio/")) return "audio";
    return "file";
  }

  async function insertFiles(files, position = null, requestedKind = "") {
    if (!files?.length || !canEdit()) return [];
    const choices = await options.chooseAttachmentModes?.([...files], requestedKind || "auto")
      || [...files].map((file) => ({ file, kind: requestedKind || droppedFileKind(file), displayMode: requestedKind === "file" ? "file" : "preview" }));
    return attachmentUploads.insert(choices, position);
  }

  async function applySlash(type) {
    if (!menu || menu.type !== "slash") return;
    if (type === "ai" && !canUseAi()) { closeMenus(); return; }
    const range = { from: menu.from, to: menu.to };
    applying = true;
    closeMenus();
    try {
      if (type === "page") {
        await createLinkedPage(range);
        return;
      }
      let chain = editor.chain().focus().deleteRange(range);
      if (type === "ai") { chain.run(); options.onOpenAi?.("current"); }
      else if (type === "text") chain.setParagraph().run();
      else if (type === "h1" || type === "h2" || type === "h3") chain.setHeading({ level: Number(type.slice(1)) }).run();
      else if (type === "todo") chain.toggleTaskList().run();
      else if (type === "bullet") chain.toggleBulletList().run();
      else if (type === "number") chain.toggleOrderedList().run();
      else if (type === "quote") chain.toggleBlockquote().run();
      else if (type === "code") chain.toggleCodeBlock().run();
      else if (type === "divider") chain.setHorizontalRule().run();
      else if (type === "table") chain.insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run();
      else if (type === "callout") chain.insertContent({ type: "callout", attrs: { icon: "💡", tone: "purple", blockId: blockId() }, content: [paragraph("")] }).run();
      else if (type === "toggle") chain.insertContent({ type: "details", attrs: { open: true, blockId: blockId() }, content: [{ type: "detailsSummary", content: textNode("토글") }, { type: "detailsContent", content: [paragraph("")] }] }).run();
      else if (type === "pageLink") {
        chain.insertContent('[[', { updateSelection: true }).run();
        showInline("brackets", "", range.from, range.from + 2);
      }
      else if (["image", "file", "pdf", "video", "audio"].includes(type)) {
        chain.run();
        const files = await options.pickAttachment(type);
        if (files?.length) await insertFiles(files, null, type);
      }
      else if (type === "toc") {
        const headings = [];
        editor.state.doc.descendants((node) => { if (node.type.name === "heading") headings.push(`${"  ".repeat(Math.max(0, node.attrs.level - 1))}• ${node.textContent}`); });
        chain.insertContent({ type: "callout", attrs: { icon: "≡", tone: "gray", blockId: blockId() }, content: [paragraph(headings.join("\n") || "제목을 추가하면 목차가 표시됩니다.")] }).run();
      }
    } catch (error) {
      options.onStatus?.(error?.message || "새 페이지를 만들지 못했습니다.");
    } finally {
      applying = false;
    }
  }

  function handleMenuKey(event) {
    if (!menu || !["slash", "inline"].includes(menu.type)) return false;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const count = menu.items.length;
      if (count) menuIndex = (menuIndex + (event.key === "ArrowDown" ? 1 : -1) + count) % count;
      if (menu.type === "slash") renderSlash(); else renderInline({ preserveItems: true });
      return true;
    }
    if (event.key === "Enter" && menu.items.length) {
      event.preventDefault();
      if (menu.type === "slash") applySlash(menu.items[menuIndex].command[0]); else applyInline(menu.items[menuIndex]);
      return true;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      closeMenus();
      return true;
    }
    return false;
  }

  function renderBlockRangeToolbar(range) {
    rangeAnchorIndex = range?.anchor ?? -1;
    if (!range) {
      ui.blockRangeToolbar.style.display = "none";
      return;
    }
    const count = range.to - range.from + 1;
    ui.blockRangeCount.textContent = `${count}개 블록`;
    const buttons = Object.fromEntries([...ui.blockRangeToolbar.querySelectorAll("[data-block-range]")].map((button) => [button.dataset.blockRange, button]));
    if (buttons.ai) buttons.ai.hidden = !canUseAi();
    buttons.outdent.disabled = range.blocks.slice(range.from, range.to + 1).every(({ indent }) => indent === 0);
    buttons.indent.disabled = range.from === 0 || range.blocks.slice(range.from, range.to + 1).every(({ indent }) => indent === 3);
    buttons.up.disabled = range.from === 0;
    buttons.down.disabled = range.to === range.blocks.length - 1;
    ui.selectionToolbar.style.display = "none";
    ui.tableToolbar.style.display = "none";
    requestAnimationFrame(() => {
      if (!getWorkNoteBlockRange(editor)) return;
      if (matchMedia("(max-width: 740px)").matches) {
        ui.blockRangeToolbar.style.removeProperty("left");
        ui.blockRangeToolbar.style.removeProperty("top");
      } else {
        const selected = editor.view.dom.querySelector(".worknote-block-range-selected");
        if (!selected) return;
        const rect = selected.getBoundingClientRect();
        const width = ui.blockRangeToolbar.offsetWidth || 560;
        ui.blockRangeToolbar.style.left = `${Math.max(8, Math.min(innerWidth - width - 8, rect.left))}px`;
        ui.blockRangeToolbar.style.top = `${Math.max(8, rect.top - 48)}px`;
      }
      ui.blockRangeToolbar.style.display = "flex";
    });
  }

  function selectBlockRange(index, extend = false) {
    if (!editor || index < 0) return false;
    const anchor = extend && rangeAnchorIndex >= 0 ? rangeAnchorIndex : index;
    closeMenus();
    return setWorkNoteBlockRange(editor, anchor, index);
  }

  function blockRangeAction(action) {
    if (!editor) return false;
    if (action === "ai") { options.onOpenAi?.("blocks"); return true; }
    if (action === "outdent") return changeWorkNoteBlockIndent(editor, -1);
    if (action === "indent") return changeWorkNoteBlockIndent(editor, 1);
    if (action === "up") return moveWorkNoteBlockRange(editor, -1);
    if (action === "down") return moveWorkNoteBlockRange(editor, 1);
    if (action === "duplicate") return duplicateWorkNoteBlockRange(editor, blockId);
    if (action === "delete") return deleteWorkNoteBlockRangeAttachments(editor, { createId: blockId, removeAttachment: options.removeAttachment, onError: options.onAttachmentError });
    return false;
  }

  function selectionToolbar() {
    if (editor && getWorkNoteBlockRange(editor)) {
      ui.selectionToolbar.style.display = "none";
      ui.tableToolbar.style.display = "none";
      return;
    }
    const inTable = Boolean(editor?.isActive("table"));
    if (inTable && editor.isFocused) {
      const rect = editor.view.coordsAtPos(editor.state.selection.from);
      ui.tableToolbar.style.left = `${Math.max(8, Math.min(innerWidth - 430, rect.left))}px`;
      ui.tableToolbar.style.top = `${Math.max(8, rect.top - 44)}px`;
      ui.tableToolbar.style.display = "flex";
    } else ui.tableToolbar.style.display = "none";
    if (!editor || editor.state.selection.empty || !editor.isFocused || !editor.state.selection.$from.parent.isTextblock) {
      ui.selectionToolbar.style.display = "none";
      return;
    }
    const start = editor.view.coordsAtPos(editor.state.selection.from);
    const end = editor.view.coordsAtPos(editor.state.selection.to);
    ui.selectionToolbar.style.left = `${Math.max(8, Math.min(innerWidth - 390, (start.left + end.right) / 2 - 175))}px`;
    ui.selectionToolbar.style.top = `${Math.max(8, start.top - 44)}px`;
    ui.selectionToolbar.style.display = "flex";
    const aiButton = ui.selectionToolbar.querySelector("[data-ai]");
    if (aiButton) aiButton.hidden = !canUseAi();
    for (const button of ui.selectionToolbar.querySelectorAll("[data-mark]")) button.classList.toggle("active", editor.isActive(button.dataset.mark));
  }

  function blockAction(action, targetId = blockInteractions?.currentId() || "") {
    if (!editor || !targetId) return false;
    ui.blockMenu.style.display = "none";
    if (action === "ai") { options.onOpenAi?.("current"); return true; }
    if (action === "range") return selectBlockRange(blockInteractions.currentIndex(), false);
    if (action === "move") {
      blockInteractions.beginMobileMove(targetId);
      options.onStatus?.("옮길 위치의 블록을 누르세요.");
      return true;
    }
    let applied = false;
    if (action === "duplicate") applied = duplicateWorkNoteNestedBlock(editor, targetId, blockId);
    else if (action === "delete") applied = deleteWorkNoteAttachmentBlock(editor, targetId, { createId: blockId, removeAttachment: options.removeAttachment, onError: options.onAttachmentError });
    else if (action === "up" || action === "down") applied = moveWorkNoteNestedBlockByDirection(editor, targetId, action === "up" ? -1 : 1, blockId);
    else if (action === "indent" || action === "outdent") applied = changeWorkNoteNestedBlockIndent(editor, targetId, action === "indent" ? 1 : -1, blockId);
    else applied = transformWorkNoteNestedBlock(editor, targetId, action, blockId);
    if (!applied) options.onStatus?.("이 블록에서는 해당 작업을 실행할 수 없습니다.");
    return applied;
  }

  function renderBlockMenu(rect, targetId, { mobile = false, topLevel = false } = {}) {
    blockInteractions?.finishMobileMove();
    ui.blockMenu.classList.toggle("is-mobile", mobile);
    ui.blockMenu.innerHTML = [
      ...(canUseAi() ? [["ai", "fa-wand-magic-sparkles", "AI로 작성·편집"]] : []),
      ["text", "fa-font", "일반 텍스트로 전환"], ["h1", "fa-heading", "제목 1로 전환"], ["h2", "fa-heading", "제목 2로 전환"], ["h3", "fa-heading", "제목 3으로 전환"],
      ["bullet", "fa-list-ul", "글머리 기호로 전환"], ["number", "fa-list-ol", "번호 목록으로 전환"], ["todo", "fa-square-check", "할 일로 전환"],
      ["outdent", "fa-outdent", "내어쓰기"], ["indent", "fa-indent", "들여쓰기"],
      ...(mobile ? [["move", "fa-up-down-left-right", "이동"]] : []),
      ["duplicate", "fa-copy", "복제"], ["up", "fa-arrow-up", "위로 이동"], ["down", "fa-arrow-down", "아래로 이동"], ["delete", "fa-trash", "삭제"],
      ...(mobile && topLevel ? [["range", "fa-layer-group", "여러 블록 선택"]] : []),
    ].map(([action, icon, label]) => `<button type="button" data-action="${action}"><i class="fa-solid ${icon}"></i>${label}</button>`).join("");
    for (const button of ui.blockMenu.querySelectorAll("button")) {
      button.disabled = button.dataset.action !== "range" && !canWorkNoteNestedBlockAction(editor, targetId, button.dataset.action, blockId);
      button.addEventListener("mousedown", (event) => { event.preventDefault(); if (!button.disabled) blockAction(button.dataset.action, targetId); });
    }
    positionMenu(ui.blockMenu, rect, 220);
  }

  function renderPlacementMenu(rect, sourceId, targetId) {
    ui.blockMenu.classList.add("is-mobile");
    ui.blockMenu.innerHTML = [["before", "fa-arrow-up", "앞에 놓기"], ["inside", "fa-turn-down", "안에 넣기"], ["after", "fa-arrow-down", "뒤에 놓기"]]
      .map(([action, icon, label]) => `<button type="button" data-placement="${action}"><i class="fa-solid ${icon}"></i>${label}</button>`).join("");
    for (const button of ui.blockMenu.querySelectorAll("button")) {
      button.disabled = !canMoveWorkNoteNestedBlock(editor, sourceId, targetId, button.dataset.placement, blockId);
      button.addEventListener("mousedown", (event) => {
        event.preventDefault();
        if (button.disabled) return;
        moveWorkNoteNestedBlock(editor, sourceId, targetId, button.dataset.placement, blockId);
        blockInteractions.finishMobileMove();
        ui.blockMenu.style.display = "none";
      });
    }
    positionMenu(ui.blockMenu, rect, 220);
  }

  function refreshPageLinks() {
    if (!editor) return;
    const titleMap = new Map(pages().map((page) => [page.pageId, page.title]));
    const json = editor.getJSON();
    let changed = false;
    const visit = (node) => {
      if (node.type === "pageLinkBlock" && titleMap.has(node.attrs?.pageId) && node.attrs.title !== titleMap.get(node.attrs.pageId)) {
        node.attrs.title = titleMap.get(node.attrs.pageId);
        changed = true;
      }
      if (node.type === "text") for (const mark of node.marks || []) if (mark.type === "link" && String(mark.attrs?.href || "").startsWith("worknote://")) {
        const linkedId = mark.attrs.href.slice("worknote://".length);
        const title = titleMap.get(linkedId);
        if (title && mark.attrs.title !== title) {
          mark.attrs.title = title;
          node.text = title;
          changed = true;
        }
      }
      (node.content || []).forEach(visit);
    };
    visit(json);
    if (changed) editor.commands.setContent(json, { emitUpdate: true });
  }

  function mount(page) {
    const generation = ++mountGeneration;
    const realtimeEnabled = Boolean(options.connectRealtime && (options.shouldConnectRealtime?.(page) ?? true));
    const attachmentLocationLabel = typeof options.attachmentLocationLabel === "function" ? options.attachmentLocationLabel(page) : options.attachmentLocationLabel;
    const canRemoveAttachment = options.canRemoveAttachment === false ? false : () => canEdit() && (typeof options.canRemoveAttachment !== "function" || options.canRemoveAttachment(page) !== false);
    bootstrapping = true;
    applyingRemoteUpdate = 0;
    localMutationVersion = 0;
    realtime?.destroy?.();
    realtime = null;
    editor?.destroy();
    editor = null;
    ydoc?.destroy();
    renderBlockRangeToolbar(null);
    pageId = page.pageId;
    ydoc = new Y.Doc();
    const initial = blocksToTiptapDocument(page.blocks);
    function createBoundEditor(editorOptions = {}) {
      return new Editor({
        element: options.element,
        editable: false,
        injectCSS: false,
        ...editorOptions,
        extensions: [
          StarterKit.configure({ undoRedo: false, link: false, underline: false, bulletList: {}, orderedList: {}, listItem: {} }),
          Collaboration.configure({ document: ydoc, ...(Object.hasOwn(editorOptions, "content") ? { ySyncOptions: { mapping: new Map() } } : {}) }),
          TaskList,
          TaskItem.configure({ nested: true }),
          TableKit.configure({ table: { resizable: true, allowTableNodeSelection: true } }),
          Details.configure({ persist: true }), DetailsSummary, DetailsContent,
          Callout, PageLinkBlock, UserMention, createAttachmentBlock({
            getAttachmentBlob: options.getAttachmentBlob,
            openAttachment: options.openAttachment,
            maxPreviewBytes: options.maxAttachmentPreviewBytes,
            removeAttachment: options.removeAttachment,
            locationLabel: attachmentLocationLabel,
            canRemove: canRemoveAttachment,
            canEdit,
            retryAttachment: (token) => attachmentUploads.retry(token),
            cancelAttachment: (token) => attachmentUploads.cancel(token),
          }), BlockIdentity, createWorkNoteBlockRangeExtension(renderBlockRangeToolbar),
          Underline, Link.configure({
            openOnClick: true,
            autolink: true,
            isAllowedUri: (url, context) => String(url || "").startsWith("worknote://") || context.defaultValidate(url),
            HTMLAttributes: { target: "_blank", rel: "noopener noreferrer" },
          }), TextStyleKit, Highlight.configure({ multicolor: true }),
          Placeholder.configure({ placeholder: "입력하거나 '/'로 명령 선택" }),
        ],
        editorProps: {
          attributes: { class: "tiptap", spellcheck: "true", "aria-label": "노트 본문" },
          handleKeyDown: (_view, event) => {
            if (canEdit() && handleWorkNoteAttachmentDeleteKey(editor, event, { createId: blockId, removeAttachment: options.removeAttachment, onError: options.onAttachmentError })) return true;
            if (canEdit() && handleWorkNoteListTabKey(editor, event, blockId)) return true;
            if (event.key === " " && !event.isComposing && canUseAi() && editor.state.selection.empty
              && editor.state.selection.$from.parent.isTextblock && editor.state.selection.$from.parent.content.size === 0) {
              event.preventDefault();
              options.onOpenAi?.("insertion");
              return true;
            }
            if (event.key === "Escape" && clearWorkNoteBlockRange(editor)) {
              event.preventDefault();
              return true;
            }
            return handleMenuKey(event);
          },
          handleClick: (_view, _pos, event) => {
            if (!matchMedia("(max-width: 740px)").matches) clearWorkNoteBlockRange(editor);
            const pageButton = event.target.closest("[data-work-note-page]");
            const anchor = event.target.closest("a[href^='worknote://']");
            const target = pageButton?.dataset.workNotePage || anchor?.getAttribute("href")?.slice("worknote://".length);
            if (target) {
              event.preventDefault();
              const external = /^cloud\/([^/]+)\/(.+)$/u.exec(target);
              const local = /^local\/([^/]+)\/(.+)$/u.exec(target);
              if (external) options.openLinkedPage?.({ source: 'cloud', documentId: external[1], pageId: external[2] });
              else if (local) options.openLinkedPage?.({ source: 'local', tenantId: local[1], pageId: local[2] });
              else options.openPage(target);
              return true;
            }
            return false;
          },
          handleDrop: (view, event) => {
            const files = [...(event.dataTransfer?.files || [])];
            if (!files.length) return false;
            event.preventDefault();
            const position = view.posAtCoords({ left: event.clientX, top: event.clientY })?.pos ?? view.state.selection.from;
            insertFiles(files, position);
            return true;
          },
          handlePaste: (_view, event) => {
            const files = [...(event.clipboardData?.files || [])];
            if (files.length) {
              event.preventDefault();
              insertFiles(files);
              return true;
            }
            const pasted = workNotePastedUrl(event.clipboardData?.getData('text/plain'), location.origin);
            if (!pasted || !canEdit()) return false;
            event.preventDefault();
            const { from,to } = editor.state.selection;
            if (from !== to) editor.chain().focus().setLink({ href:pasted.href, title:pasted.label,
              target:pasted.internal?null:'_blank', rel:pasted.internal?null:'noopener noreferrer',
              class:pasted.internal?'internal-page-link':'pasted-web-link' }).run();
            else editor.chain().focus().insertContent({ type:'text', text:pasted.label, marks:[{ type:'link', attrs:{
              href:pasted.href,title:pasted.label,target:pasted.internal?null:'_blank',
              rel:pasted.internal?null:'noopener noreferrer',class:pasted.internal?'internal-page-link':'pasted-web-link',
            } }] }).insertContent(' ').run();
            return true;
          },
        },
        onUpdate: ({ editor: instance, transaction }) => {
          if (bootstrapping || generation !== mountGeneration) return;
          const change = classifyWorkNoteTransaction(transaction);
          if (!change.docChanged) return;
          const active = options.getPage();
          if (!active || active.pageId !== pageId) return;
          const json = instance.getJSON();
          const previousMeaningful = hasMeaningfulWorkNoteBlocks(active.blocks);
          active.blocks = options.normalizeSerializedBlocks?.(tiptapDocumentToBlocks(json)) || tiptapDocumentToBlocks(json);
          active.markdown = tiptapDocumentToMarkdown(json);
          const meaningful = hasMeaningfulWorkNoteBlocks(active.blocks);
          if (realtimeEnabled && (applyingRemoteUpdate > 0 || change.remote)) {
            options.onRemoteChange?.(active, { origin: "remote", meaningful });
          } else {
            localMutationVersion += 1;
            options.onChange(active, { origin: "editor", meaningful, explicitEmpty: previousMeaningful && !meaningful });
          }
          if (!applying) detectTrigger();
        },
        onSelectionUpdate: () => { selectionToolbar(); detectTrigger(); },
        onBlur: () => setTimeout(selectionToolbar, 80),
      });
    }

    function bindEditor(instance, { source = "ready" } = {}) {
      if (generation !== mountGeneration) {
        instance.destroy();
        return;
      }
      editor = instance;
      if (source === "bootstrap") editor.commands.setContent(initial, { emitUpdate: false });
      const identityTransaction = workNoteBlockIdentityTransaction(editor.state, blockId);
      if (identityTransaction) editor.view.dispatch(identityTransaction);
      if (!realtimeEnabled) {
        refreshPageLinks();
        bootstrapping = false;
        editor.setEditable(canEdit(), false);
        return;
      }
      refreshPageLinks();
      const active = options.getPage();
      if (active && active.pageId === pageId) {
        const json = editor.getJSON();
        const blocks = options.normalizeSerializedBlocks?.(tiptapDocumentToBlocks(json)) || tiptapDocumentToBlocks(json);
        active.blocks = blocks;
        active.markdown = tiptapDocumentToMarkdown(json);
        options.onRemoteChange?.(active, { origin: "bootstrap", meaningful: hasMeaningfulWorkNoteBlocks(blocks) });
      }
      bootstrapping = false;
      editor.setEditable(canEdit(), false);
    }

    if (realtimeEnabled) {
      const lifecycle = createWorkNoteRealtimeEditorLifecycle({
        generation,
        isCurrent: (value) => value === mountGeneration,
        initialContent: initial,
        createEditor: createBoundEditor,
        onBound: bindEditor,
      });
      realtime = options.connectRealtime({
        document: ydoc,
        page,
        onBootstrap: lifecycle.onBootstrap,
        onReady: lifecycle.onReady,
        onRemoteUpdateStart: () => { if (generation === mountGeneration) applyingRemoteUpdate += 1; },
        onRemoteUpdateEnd: () => { if (generation === mountGeneration) applyingRemoteUpdate = Math.max(0, applyingRemoteUpdate - 1); },
      });
    } else bindEditor(createBoundEditor({ content: initial }), { source: "bootstrap" });
  }

  function editSelectedLink() {
    if (!editor || !canEdit()) return false;
    const currentHref = String(editor.getAttributes("link").href || "");
    const href = prompt("연결할 주소를 입력하세요. 비우면 링크가 제거됩니다.", currentHref || "https://");
    if (href === null) return false;
    const chain = editor.chain().focus().extendMarkRange("link");
    return href.trim() ? chain.setLink({ href: href.trim() }).run() : chain.unsetLink().run();
  }

  ui.selectionToolbar.addEventListener("mousedown", (event) => {
    const button = event.target.closest("button");
    if (!button) return;
    event.preventDefault();
    if (button.dataset.ai !== undefined) { options.onOpenAi?.("selection"); return; }
    const mark = button.dataset.mark;
    const chain = editor.chain().focus();
    if (mark === "bold") chain.toggleBold().run();
    else if (mark === "italic") chain.toggleItalic().run();
    else if (mark === "underline") chain.toggleUnderline().run();
    else if (mark === "strike") chain.toggleStrike().run();
    else if (mark === "code") chain.toggleCode().run();
    else if (mark === "link") editSelectedLink();
    else if (button.dataset.color) chain.setColor(button.dataset.color).run();
    else if (button.dataset.background) chain.setBackgroundColor(button.dataset.background).run();
  });
  ui.tableToolbar.addEventListener("mousedown", (event) => {
    const action = event.target.closest("button")?.dataset.table;
    if (!action) return;
    event.preventDefault();
    const chain = editor.chain().focus();
    if (action === "rowAfter") chain.addRowAfter().run();
    else if (action === "colAfter") chain.addColumnAfter().run();
    else if (action === "deleteRow") chain.deleteRow().run();
    else if (action === "deleteCol") chain.deleteColumn().run();
    else if (action === "deleteTable") chain.deleteTable().run();
  });
  ui.blockRangeToolbar.addEventListener("mousedown", (event) => {
    const action = event.target.closest("button")?.dataset.blockRange;
    if (!action) return;
    event.preventDefault();
    blockRangeAction(action);
  });
  function addBlockAfter(targetId) {
    const insertedId = insertWorkNoteNestedBlockAfter(editor, targetId, blockId);
    if (!insertedId) return false;
    requestAnimationFrame(() => {
      const node = editor.view.dom.querySelector(`[data-block-id="${CSS.escape(insertedId)}"]`);
      if (!node) return;
      const position = Math.min(editor.view.posAtDOM(node, 0) + 1, editor.state.doc.content.size);
      editor.chain().focus().setTextSelection(position).run();
      showSlash("", position - 1, position);
    });
    return true;
  }
  blockInteractions = createWorkNoteBlockInteractions({
    element: options.element,
    blockHandle: ui.blockHandle,
    dropIndicator: ui.blockDropIndicator,
    getEditor: () => editor,
    getRange: () => editor ? getWorkNoteBlockRange(editor) : null,
    selectRange: selectBlockRange,
    canMoveRangeToIndex: (targetIndex) => editor && canMoveWorkNoteBlockRangeToIndex(editor, targetIndex),
    moveRangeToIndex: (targetIndex) => editor && moveWorkNoteBlockRangeToIndex(editor, targetIndex),
    canMoveBlock: (sourceId, targetId, mode) => editor && canMoveWorkNoteNestedBlock(editor, sourceId, targetId, mode, blockId),
    moveBlock: (sourceId, targetId, mode) => editor && moveWorkNoteNestedBlock(editor, sourceId, targetId, mode, blockId),
    addAfter: addBlockAfter,
    openMenu: renderBlockMenu,
    openPlacementMenu: renderPlacementMenu,
  });
  document.addEventListener("mousedown", (event) => {
    const movingToEditorBlock = blockInteractions?.isMobileMoving() && event.target.closest("#editor");
    if (!movingToEditorBlock && !event.target.closest("#slashMenu,#inlineMenu,#blockMenu,#blockHandle,#selectionToolbar,#blockRangeToolbar")) closeMenus();
    if (editor && !event.target.closest("#editor,#blockRangeToolbar,#blockHandle")) clearWorkNoteBlockRange(editor);
  });

  async function serializeCurrent({ forceChange = false } = {}) {
    const active = options.getPage();
    if (!active || active.pageId !== pageId) return { changed: false, localChanged: false, page: active };
    if (bootstrapping || !editor) return { changed: false, localChanged: false, pendingBootstrap: true, page: active };
    const json = editor.getJSON();
    const blocks = options.normalizeSerializedBlocks?.(tiptapDocumentToBlocks(json)) || tiptapDocumentToBlocks(json);
    const changed = JSON.stringify(active.blocks || []) !== JSON.stringify(blocks);
    const localChanged = localMutationVersion > 0;
    active.blocks = blocks;
    active.markdown = tiptapDocumentToMarkdown(json);
    if (forceChange && changed) {
      const detail = { origin: "serialization", meaningful: hasMeaningfulWorkNoteBlocks(blocks), explicitEmpty: false };
      if (localChanged) options.onChange(active, detail);
      else options.onRemoteChange?.(active, { ...detail, origin: "bootstrap" });
    }
    return { changed, localChanged, page: active };
  }

  async function confirmRealtimeSave() {
    if (!realtime?.flush) throw Object.assign(new Error('본문 실시간 연결이 없습니다. 현재 본문을 복사한 뒤 화면 전체를 새로고침해 주세요.'), { code: 'WORK_NOTE_REALTIME_NOT_CONNECTED' });
    return assertWorkNoteProjectionReceipt(await realtime.flush(), { requireActorId: true, code: 'WORK_NOTE_PROJECTION_RECEIPT_INVALID' });
  }

  return {
    mount,
    getRefreshSnapshot() {
      return Object.freeze({ pageId, mountGeneration, localMutationVersion,
        composing: Boolean(editor?.view?.composing), pendingBootstrap: bootstrapping,
        pendingUploads: attachmentUploads.hasBlocking(pageId) });
    },
    refreshPageLinks,
    closeMenus,
    setEditable(value) { editor?.setEditable(value === true, false); if (value !== true) { closeMenus(); ui.blockHandle.style.display = "none"; ui.blockRangeToolbar.style.display = "none"; } },
    insertFiles(files) { return insertFiles(files); },
    insertStoredAttachment(record) { return insertStoredWorkNoteAttachment(editor, canEdit(), record, blockId); },
    hasBlockingUploads(page = "") { return attachmentUploads.hasBlocking(page); },
    waitForPendingUploads(page = "") { return attachmentUploads.waitForPending(page); },
    async releaseRealtime() {
      const releaseGeneration = mountGeneration;
      const releasing = realtime;
      await releasing?.release?.();
      releasing?.destroy?.();
      if (releaseGeneration !== mountGeneration || realtime !== releasing) return;
      realtime = null;
      mountGeneration += 1;
      bootstrapping = false;
      applyingRemoteUpdate = 0;
      editor?.destroy();
      editor = null;
      ydoc?.destroy();
      ydoc = null;
      pageId = "";
    },
    async suspend() {
      await realtime?.release?.();
      realtime?.destroy?.();
      realtime = null;
      editor?.setEditable(false, false);
    },
    resume() {
      const active = options.getPage();
      if (active) mount(active);
    },
    focus() { editor?.commands.focus("end"); },
    focusMention(mentionId) { return focusWorkNoteMention(options.element, mentionId); },
    hasSelection() { return Boolean(editor && !editor.state.selection.empty); },
    isEditable() { return Boolean(editor && canEdit()); },
    captureAiTarget(scope = "auto") {
      return captureWorkNoteAiTarget({ editor, pageId, editable: canEdit(), scope, blockRange: editor ? getWorkNoteBlockRange(editor) : null,
        blockId: scope === "current" ? blockInteractions?.currentId() : "" });
    },
    applyAiProposal(snapshot, proposal) {
      const applied = applyWorkNoteAiProposal({ editor, pageId, editable: canEdit(), snapshot, proposal });
      if (applied) clearWorkNoteBlockRange(editor);
      return applied;
    },
    serializeCurrent,
    confirmRealtimeSave,
    async flush({ forceChange = false, realtimeAck = forceChange } = {}) {
      await attachmentUploads.waitForPending(pageId);
      const serialized = await serializeCurrent({ forceChange });
      if (!serialized.page || serialized.page.pageId !== pageId) return { changed: false, persisted: undefined };
      if (realtimeAck) await confirmRealtimeSave();
      const persisted = await options.flush(serialized.page);
      return { changed: serialized.changed, persisted };
    },
    retryRealtime() { realtime?.retry?.(); },
    shortcut(action) { return applyWorkNoteShortcut(editor, action, { editSelectedLink, blockAction }); },
    blockShortcut(digit) {
      if (!editor) return false;
      const chain = editor.chain().focus();
      if (digit === "0") return chain.setParagraph().run();
      if (["1", "2", "3"].includes(digit)) return chain.setHeading({ level: Number(digit) }).run();
      if (digit === "4") return chain.toggleTaskList().run();
      if (digit === "5") return chain.toggleBulletList().run();
      if (digit === "6") return chain.toggleOrderedList().run();
      if (digit === "7") return chain.insertContent({ type: "details", attrs: { open: true, blockId: blockId() }, content: [{ type: "detailsSummary", content: textNode("토글") }, { type: "detailsContent", content: [paragraph("")] }] }).run();
      if (digit === "8") return chain.toggleCodeBlock().run();
      if (digit === "9") {
        void createLinkedPage().catch((error) => options.onStatus?.(error?.message || "새 페이지를 만들지 못했습니다."));
        return true;
      }
      return false;
    },
    openBlockMenu() {
      if (!editor) return;
      const anchor = editor.view.domAtPos(editor.state.selection.from).node;
      const element = anchor.nodeType === globalThis.Node.TEXT_NODE ? anchor.parentElement : anchor;
      blockInteractions.showBlockHandle(element);
      const target = blockInteractions.currentElement();
      if (target) renderBlockMenu(target.getBoundingClientRect(), blockInteractions.currentId(), { topLevel: blockInteractions.currentIndex() >= 0 });
    },
  };
}
