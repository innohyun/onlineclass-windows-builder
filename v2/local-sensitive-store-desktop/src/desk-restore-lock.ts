// A window-scoped write boundary. No document, student, or path data is stored here.
let restoreBusy = false;

export function isDeskRestoreBlocked() { return restoreBusy; }

export function beginDeskRestore(): () => void {
  if (restoreBusy) throw new Error("desktop_restore_already_running");
  restoreBusy = true;
  publish();
  let released = false;
  return () => {
    if (released) return;
    released = true;
    restoreBusy = false;
    publish();
  };
}

function publish() {
  document.body.dataset.deskRestoreBusy = String(restoreBusy);
  window.dispatchEvent(new CustomEvent("desk:restore-lock-changed", { detail: { busy: restoreBusy } }));
}
