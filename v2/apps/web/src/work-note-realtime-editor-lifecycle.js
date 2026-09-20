export function createWorkNoteRealtimeEditorLifecycle({
  generation,
  isCurrent,
  initialContent,
  createEditor,
  onBound = () => {},
} = {}) {
  if (typeof isCurrent !== "function" || typeof createEditor !== "function") {
    throw new TypeError("realtime editor lifecycle requires isCurrent and createEditor");
  }

  let bound = false;

  function bind(source) {
    if (bound || !isCurrent(generation)) return null;
    const editor = createEditor(source === "bootstrap" ? { content: initialContent } : {});
    bound = true;
    onBound(editor, { source });
    return editor;
  }

  return Object.freeze({
    onBootstrap: () => bind("bootstrap"),
    onReady: () => bind("ready"),
    isBound: () => bound,
  });
}
