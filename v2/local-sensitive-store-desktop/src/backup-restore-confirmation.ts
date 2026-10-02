type BackupRestoreConfirmation = {
  date: string;
  source: string;
  summary: string;
  kind?: string;
};

function element<T extends HTMLElement>(id: string) {
  const value = document.getElementById(id);
  if (!value) throw new Error(`missing element: ${id}`);
  return value as T;
}

export function confirmBackupRestore(input: BackupRestoreConfirmation) {
  const dialog = element<HTMLDialogElement>("backupRestoreConfirmDialog");
  if (dialog.open) return Promise.resolve(false);
  const returnFocus = document.activeElement as HTMLElement | null;
  element("backupConfirmDate").textContent = input.date || "-";
  element("backupConfirmSource").textContent = input.source || "-";
  element("backupConfirmSummary").textContent = input.summary;
  if (!document.getElementById("backupConfirmKind")) dialog.querySelector("dl")?.insertAdjacentHTML("beforeend", '<div><dt>백업 종류</dt><dd id="backupConfirmKind"></dd></div>');
  const kind = document.getElementById("backupConfirmKind"); if (kind) kind.textContent = input.kind || "선택 백업 목록에서 확인";
  if (!document.getElementById("backupConfirmAcknowledged")) {
    const actions = dialog.querySelector("form > div:last-child");
    actions?.insertAdjacentHTML("beforebegin", '<label class="restore-confirm-ack"><input id="backupConfirmAcknowledged" type="checkbox"><span>현재 상태 보호 백업을 검증한 뒤 로컬 자료와 첨부파일을 병합하는 것을 확인했습니다.</span></label>');
  }
  const acknowledged = document.getElementById("backupConfirmAcknowledged") as HTMLInputElement | null;
  const confirm = dialog.querySelector<HTMLButtonElement>('button[value="confirm"]');
  if (acknowledged && confirm) {
    acknowledged.checked = false;
    confirm.disabled = true;
    acknowledged.onchange = () => { confirm.disabled = !acknowledged.checked; };
  }
  dialog.returnValue = "";

  return new Promise<boolean>((resolve) => {
    const settle = () => {
      dialog.removeEventListener("close", settle);
      returnFocus?.focus({ preventScroll: true });
      resolve(dialog.returnValue === "confirm" && acknowledged?.checked === true);
    };
    dialog.addEventListener("close", settle);
    dialog.showModal();
  });
}
