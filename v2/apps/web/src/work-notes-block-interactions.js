import { workNoteHandleBlocks } from "./work-notes-nested-blocks.js";

const mobileQuery = "(max-width: 740px)";

export function workNoteBlockHandlePosition(editorRect, blockRect, leadingControlRect = null) {
  const leadingLeft = Number.isFinite(leadingControlRect?.left) ? leadingControlRect.left : blockRect.left;
  return { left: Math.max(4, leadingLeft - 42), top: blockRect.top + 2 };
}

export function workNoteFloatingMenuPosition(anchorRect, menuRect, viewportRect, margin = 8, gap = 5) {
  const left = Math.max(margin, Math.min(viewportRect.width - menuRect.width - margin, anchorRect.left));
  const below = anchorRect.bottom + gap;
  const above = anchorRect.top - gap - menuRect.height;
  const top = below + menuRect.height <= viewportRect.height - margin
    ? below
    : above >= margin ? above : Math.max(margin, Math.min(viewportRect.height - menuRect.height - margin, anchorRect.top));
  return { left, top };
}

export function workNoteBlockHandleCorridorContains(pointer, handleRect, blockRect) {
  const values = [pointer?.clientX, pointer?.clientY, handleRect?.left, handleRect?.right, handleRect?.top, handleRect?.bottom,
    blockRect?.left, blockRect?.top, blockRect?.bottom];
  if (!values.every(Number.isFinite)) return false;
  return pointer.clientX >= Math.min(handleRect.left, blockRect.left) - 2
    && pointer.clientX <= Math.max(handleRect.right, blockRect.left) + 2
    && pointer.clientY >= Math.min(handleRect.top, blockRect.top) - 4
    && pointer.clientY <= Math.max(handleRect.bottom, blockRect.bottom) + 4;
}

export function createWorkNoteBlockInteractions(options) {
  let blockIndex = -1;
  let blockId = "";
  let blockElement = null;
  let rangePointerActive = false;
  let mobileLongPressTimer = 0;
  let mobilePointerStart = null;
  let suppressClick = false;
  let mobileMoveSourceId = "";
  let dragging = false;
  let dragged = false;
  let dragSource = null;
  let dropTarget = null;

  const editorBlocks = () => [...(options.getEditor()?.view.dom.children || [])];
  const canEdit = () => options.getEditor()?.isEditable === true;
  const records = () => {
    const editor = options.getEditor();
    return editor ? workNoteHandleBlocks(editor.state.doc) : [];
  };
  const actionableElement = (target) => {
    const editor = options.getEditor();
    if (!editor || !target?.closest) return null;
    const ids = new Set(records().map((record) => record.id).filter(Boolean));
    let element = target.closest("[data-block-id]");
    while (element && editor.view.dom.contains(element)) {
      if (ids.has(element.dataset.blockId)) return element;
      element = element.parentElement?.closest("[data-block-id]");
    }
    return null;
  };
  const topLevelIndex = (element) => {
    const top = element?.closest(".tiptap > *");
    return top === element ? editorBlocks().indexOf(top) : -1;
  };
  const firstLineRect = (element) => {
    const line = element.matches("li")
      ? element.querySelector(":scope > p, :scope > div > p")
      : element.matches("aside") ? element.querySelector(".callout-content > *") : null;
    return (line || element).getBoundingClientRect();
  };
  const blockIndexAtClientY = (clientY) => {
    const blocks = editorBlocks();
    if (!blocks.length) return -1;
    let closest = 0;
    let distance = Number.POSITIVE_INFINITY;
    blocks.forEach((block, index) => {
      const rect = block.getBoundingClientRect();
      const nextDistance = clientY >= rect.top && clientY <= rect.bottom ? 0 : Math.min(Math.abs(clientY - rect.top), Math.abs(clientY - rect.bottom));
      if (nextDistance < distance) { closest = index; distance = nextDistance; }
    });
    return closest;
  };
  const hideDropIndicator = () => {
    dropTarget = null;
    options.dropIndicator.style.display = "none";
    options.dropIndicator.removeAttribute("data-placement");
  };
  const placementAt = (element, clientY) => {
    const rect = element.getBoundingClientRect();
    const ratio = rect.height ? (clientY - rect.top) / rect.height : 0;
    return ratio < 0.3 ? "before" : ratio > 0.7 ? "after" : "inside";
  };
  const showDropIndicator = (element, mode) => {
    const editor = options.getEditor();
    if (!editor || !element || element.dataset.blockId === dragSource?.id) return hideDropIndicator();
    const rect = element.getBoundingClientRect();
    const editorRect = editor.view.dom.getBoundingClientRect();
    dropTarget = { id: element.dataset.blockId, mode };
    options.dropIndicator.dataset.placement = mode;
    options.dropIndicator.style.left = `${mode === "inside" ? rect.left : editorRect.left}px`;
    options.dropIndicator.style.top = `${mode === "before" ? rect.top - 1 : mode === "after" ? rect.bottom - 1 : rect.top}px`;
    options.dropIndicator.style.width = `${mode === "inside" ? rect.width : editorRect.width}px`;
    options.dropIndicator.style.height = `${mode === "inside" ? rect.height : 2}px`;
    options.dropIndicator.style.display = "block";
  };
  const rangeTargetIndex = (element, mode) => {
    const target = editorBlocks().indexOf(element?.closest(".tiptap > *"));
    return target < 0 ? -1 : target + (mode === "after" ? 1 : 0);
  };

  function showBlockHandle(target) {
    const editor = options.getEditor();
    const element = actionableElement(target);
    if (!editor || !canEdit() || !element) {
      options.blockHandle.style.display = "none";
      return false;
    }
    blockElement = element;
    blockId = element.dataset.blockId || "";
    blockIndex = topLevelIndex(element);
    const rect = firstLineRect(element);
    const leadingControl = element.querySelector(":scope > label input[type='checkbox']");
    const position = workNoteBlockHandlePosition(editor.view.dom.getBoundingClientRect(), rect, leadingControl?.getBoundingClientRect());
    options.blockHandle.style.left = `${position.left}px`;
    options.blockHandle.style.top = `${position.top}px`;
    options.blockHandle.style.display = "flex";
    return true;
  }

  document.addEventListener("pointermove", (event) => {
    if (!rangePointerActive || !options.getEditor()) return;
    if (event.clientY < 80) scrollBy(0, -12);
    else if (event.clientY > innerHeight - 80) scrollBy(0, 12);
    const index = blockIndexAtClientY(event.clientY);
    if (index >= 0) options.selectRange(index, true);
  });
  document.addEventListener("pointerup", () => { rangePointerActive = false; });

  options.element.addEventListener("pointerdown", (event) => {
    const editor = options.getEditor();
    if (!editor || !canEdit() || matchMedia(mobileQuery).matches || event.button !== 0) return;
    const editorRect = editor.view.dom.getBoundingClientRect();
    const elementRect = options.element.getBoundingClientRect();
    if (event.clientX < elementRect.left || event.clientX >= editorRect.left - 44) return;
    event.preventDefault();
    const index = blockIndexAtClientY(event.clientY);
    if (index >= 0) { options.selectRange(index, event.shiftKey); rangePointerActive = true; }
  });

  options.element.addEventListener("pointerdown", (event) => {
    if (!options.getEditor() || !canEdit() || !matchMedia(mobileQuery).matches || event.button > 0) return;
    const element = actionableElement(event.target);
    if (!element) return;
    mobilePointerStart = { x: event.clientX, y: event.clientY, element };
    clearTimeout(mobileLongPressTimer);
    mobileLongPressTimer = setTimeout(() => {
      if (!mobilePointerStart) return;
      suppressClick = true;
      showBlockHandle(mobilePointerStart.element);
      options.openMenu(firstLineRect(mobilePointerStart.element), blockId, { mobile: true, topLevel: blockIndex >= 0 });
      navigator.vibrate?.(20);
    }, 450);
  });
  options.element.addEventListener("pointermove", (event) => {
    if (!mobilePointerStart) return;
    if (Math.hypot(event.clientX - mobilePointerStart.x, event.clientY - mobilePointerStart.y) > 8) {
      clearTimeout(mobileLongPressTimer);
      mobilePointerStart = null;
    }
  });
  options.element.addEventListener("pointerup", () => {
    clearTimeout(mobileLongPressTimer);
    mobilePointerStart = null;
  });
  options.element.addEventListener("click", (event) => {
    if (!options.getEditor() || !matchMedia(mobileQuery).matches) return;
    if (suppressClick) {
      suppressClick = false;
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (mobileMoveSourceId) {
      const target = actionableElement(event.target);
      if (!target || target.dataset.blockId === mobileMoveSourceId) return;
      event.preventDefault();
      event.stopPropagation();
      options.openPlacementMenu(firstLineRect(target), mobileMoveSourceId, target.dataset.blockId);
      return;
    }
    if (!options.getRange()) return;
    const index = editorBlocks().indexOf(event.target.closest(".tiptap > *"));
    if (index < 0) return;
    event.preventDefault();
    event.stopPropagation();
    options.selectRange(index, true);
  }, true);

  const grip = options.blockHandle.querySelector("[data-block-grip]");
  const add = options.blockHandle.querySelector("[data-block-add]");
  grip.draggable = true;
  grip.addEventListener("click", () => {
    if (!dragged && blockElement) options.openMenu(grip.getBoundingClientRect(), blockId, { mobile: false, topLevel: blockIndex >= 0 });
    dragged = false;
  });
  add.addEventListener("mousedown", (event) => {
    event.preventDefault();
    if (blockId) options.addAfter(blockId);
  });
  grip.addEventListener("dragstart", (event) => {
    if (!canEdit() || !blockId) return event.preventDefault();
    const range = options.getRange();
    if (blockIndex >= 0 && range && blockIndex >= range.from && blockIndex <= range.to) {
      event.dataTransfer.setData("application/x-classaimate-worknote-range", String(blockIndex));
      dragSource = { id: blockId, kind: "range", index: blockIndex };
    } else {
      event.dataTransfer.setData("application/x-classaimate-worknote-block", blockId);
      dragSource = { id: blockId, kind: "block", index: blockIndex };
    }
    dragging = true;
    dragged = true;
    event.dataTransfer.effectAllowed = "move";
  });
  options.element.addEventListener("mousemove", (event) => {
    const editor = options.getEditor();
    if (dragging || !editor?.view.dom.contains(event.target)) return;
    if (blockElement && workNoteBlockHandleCorridorContains(event, options.blockHandle.getBoundingClientRect(), firstLineRect(blockElement))) return;
    const element = actionableElement(event.target);
    if (element) showBlockHandle(element);
  });
  options.element.addEventListener("dragover", (event) => {
    if (!dragging || !dragSource) return;
    const target = actionableElement(event.target);
    if (!target) return;
    const mode = placementAt(target, event.clientY);
    const targetIndex = rangeTargetIndex(target, mode);
    if (dragSource.kind === "range"
      ? mode === "inside" || targetIndex < 0 || !options.canMoveRangeToIndex(targetIndex)
      : !options.canMoveBlock(dragSource.id, target.dataset.blockId, mode)) {
      hideDropIndicator();
      return;
    }
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    showDropIndicator(target, mode);
  });
  options.element.addEventListener("drop", (event) => {
    if (!dragging || !dragSource) return;
    event.preventDefault();
    event.stopPropagation();
    if (dropTarget) {
      if (dragSource.kind === "range" && ["before", "after"].includes(dropTarget.mode)) {
        const targetElement = options.getEditor().view.dom.querySelector(`[data-block-id="${CSS.escape(dropTarget.id)}"]`);
        const targetIndex = rangeTargetIndex(targetElement, dropTarget.mode);
        if (targetIndex >= 0) options.moveRangeToIndex(targetIndex);
      } else options.moveBlock(dragSource.id, dropTarget.id, dropTarget.mode);
    }
    dragging = false;
    dragSource = null;
    hideDropIndicator();
  });
  grip.addEventListener("dragend", () => {
    dragging = false;
    dragSource = null;
    hideDropIndicator();
    setTimeout(() => { dragged = false; }, 150);
  });

  return {
    currentIndex: () => blockIndex,
    currentId: () => blockId,
    currentElement: () => blockElement,
    showBlockHandle,
    isMobileMoving: () => Boolean(mobileMoveSourceId),
    beginMobileMove(sourceId) { mobileMoveSourceId = sourceId; },
    finishMobileMove() { mobileMoveSourceId = ""; },
  };
}
