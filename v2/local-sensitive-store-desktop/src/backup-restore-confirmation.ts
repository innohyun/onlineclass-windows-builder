type BackupRestoreConfirmation = {
  date: string;
  source: string;
  summary: string;
};

function element<T extends HTMLElement>(id: string) {
  const value = document.getElementById(id);
  if (!value) throw new Error(`missing element: ${id}`);
  return value as T;
}

export function confirmBackupRestore(input: BackupRestoreConfirmation) {
  const dialog = element<HTMLDialogElement>("backupRestoreConfirmDialog");
  element("backupConfirmDate").textContent = input.date || "-";
  element("backupConfirmSource").textContent = input.source || "-";
  element("backupConfirmSummary").textContent = input.summary;

  return new Promise<boolean>((resolve) => {
    const settle = () => {
      dialog.removeEventListener("close", settle);
      resolve(dialog.returnValue === "confirm");
    };
    dialog.addEventListener("close", settle);
    dialog.showModal();
  });
}
