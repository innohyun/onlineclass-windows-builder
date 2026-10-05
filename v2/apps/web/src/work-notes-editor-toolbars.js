import { workNoteEditorToolMode } from "./work-notes-block-interactions.js";
import { getWorkNoteBlockRange } from "./work-notes-block-range.js";

export function renderWorkNoteSelectionToolbars({ ui, editor, editable, menuOpen, canUseAi }) {
  const mode = workNoteEditorToolMode({ editable, menuOpen,
    range: editor && getWorkNoteBlockRange(editor), focused: editor?.isFocused,
    textSelected: editor && !editor.state.selection.empty && editor.state.selection.$from.parent.isTextblock,
    inTable: editor?.isActive("table") });
  if (!["table", "text"].includes(mode)) {
    ui.selectionToolbar.style.display = "none";
    ui.tableToolbar.style.display = "none";
    return;
  }
  if (mode === "table") {
    const rect = editor.view.coordsAtPos(editor.state.selection.from);
    ui.tableToolbar.style.left = `${Math.max(8, Math.min(innerWidth - 430, rect.left))}px`;
    ui.tableToolbar.style.top = `${Math.max(8, rect.top - 44)}px`;
    ui.tableToolbar.style.display = "flex";
  } else ui.tableToolbar.style.display = "none";
  if (mode !== "text") {
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

export function bindWorkNoteToolbarActions({ ui, getEditor, canMutate, canUseAi, onOpenAi, editSelectedLink, blockRangeAction }) {
  ui.selectionToolbar.addEventListener("mousedown", (event) => {
    const button = event.target.closest("button");
    if (!button || !canMutate()) return;
    event.preventDefault();
    if (button.dataset.ai !== undefined) { if (canUseAi()) onOpenAi?.("selection"); return; }
    const mark = button.dataset.mark;
    const chain = getEditor().chain().focus();
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
    if (!action || !canMutate()) return;
    event.preventDefault();
    const chain = getEditor().chain().focus();
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
}
