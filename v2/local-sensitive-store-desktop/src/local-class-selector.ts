import { invoke } from "@tauri-apps/api/core";
import { isDeskRestoreBlocked } from "./desk-restore-lock";
import { currentLocalClass, localClassLabel, localClassLifecycleLabel, type LocalClass } from "./local-class-context";
import "./local-class-selector.css";

export type LocalClassStorage = { tenantId?: string; ready?: boolean; layout?: string; migrationState?: string; dbPath?: string };
type ClassList = { ok?: boolean; selectedTenantId?: string; classes?: LocalClass[]; error?: string };
type ClassSelection = { ok?: boolean; tenantId?: string; storage?: LocalClassStorage; error?: string };

export function initLocalClassSelector(options: {
  canLeave: () => Promise<boolean>;
  onSelected: (tenantId: string) => Promise<void>;
  openTeacherHome: (path: string) => Promise<void>;
}) {
  const select = document.getElementById("localClassSelect") as HTMLSelectElement;
  const status = document.getElementById("localClassStatus")!;
  const refreshButton = document.getElementById("localClassRefresh") as HTMLButtonElement;
  const help = document.getElementById("localClassHelp") as HTMLButtonElement;
  const guide = document.getElementById("localClassGuide")!;
  let classes: LocalClass[] = [];
  let generation = 0;
  let changing = false;

  const setBusy = () => {
    select.disabled = changing || isDeskRestoreBlocked() || !classes.length;
    refreshButton.disabled = changing || isDeskRestoreBlocked();
    const workspace = document.querySelector<HTMLElement>(".workspace-scroll");
    if (workspace) workspace.inert = changing;
  };
  function render() {
    select.replaceChildren();
    if (!classes.length) { select.add(new Option("웹에서 학급 연결 후 선택", "")); }
    for (const item of classes) {
      const lifecycle = localClassLifecycleLabel(item);
      select.add(new Option(`${localClassLabel(item)}${lifecycle ? ` · ${lifecycle}` : ""}`, item.tenantId));
    }
    select.value = currentLocalClass();
    setBusy();
  }
  async function refresh() {
    const request = ++generation;
    try {
      const result = await invoke<ClassList>("get_local_classes");
      if (request !== generation) return;
      if (result.ok !== true || !Array.isArray(result.classes)) throw new Error(result.error || "local_classes_unavailable");
      classes = result.classes.filter(item => typeof item.tenantId === "string" && item.tenantId.trim());
      const selected = classes.some(item => item.tenantId === currentLocalClass()) ? currentLocalClass() : result.selectedTenantId || "";
      render();
      if (selected && selected !== currentLocalClass() && classes.some(item => item.tenantId === selected)) await options.onSelected(selected);
      render();
      status.textContent = classes.length ? `${classes.length}개 연결 학급 · 학생 기록과 백업은 학급별로 보관` : "교사 홈에서 새 학급을 개설하고 이 PC를 연결하세요.";
    } catch {
      if (request !== generation) return;
      status.textContent = "연결 학급을 확인하지 못했습니다. 현재 자료와 연결은 유지됩니다.";
    }
  }
  select.addEventListener("change", async () => {
    const next = select.value;
    if (changing || isDeskRestoreBlocked() || !classes.some(item => item.tenantId === next)) { render(); return; }
    changing = true; setBusy();
    try {
      if (!await options.canLeave()) { render(); status.textContent = "작성 중인 내용과 저장·동기화 상태를 확인한 뒤 학급을 전환하세요."; return; }
      ++generation;
      const result = await invoke<ClassSelection>("select_local_class", { tenantId: next });
      if (result.ok !== true || result.tenantId !== next) throw new Error(result.error || "local_class_selection_failed");
      await options.onSelected(next);
      render();
      status.textContent = result.storage?.tenantId === next && result.storage.ready === true && result.storage.layout === "class_files_v1" && result.storage.migrationState === "verified"
        ? "선택한 학급의 로컬 DB를 확인했습니다. 연결·백업 상태를 다시 읽었습니다."
        : "선택한 학급입니다. 로컬 DB 준비 상태는 연결·저장 상태에서 확인하세요.";
    } catch {
      render();
      status.textContent = "학급을 전환하지 못했습니다. 현재 학급의 자료를 유지합니다. 상태 확인 후 다시 시도하세요.";
    } finally { changing = false; setBusy(); }
  });
  refreshButton.addEventListener("click", () => void refresh());
  help.addEventListener("click", () => {
    guide.hidden = !guide.hidden;
    help.setAttribute("aria-expanded", String(!guide.hidden));
    select.classList.toggle("local-reader-tutorial-target", !guide.hidden);
  });
  document.getElementById("localClassPrepare")!.addEventListener("click", () => { void options.openTeacherHome("/admin/academic-year-preparation"); });
  window.addEventListener("desk:restore-lock-changed", setBusy);
  render();
  return { refresh, canChange: options.canLeave, renderStorage(storage?: LocalClassStorage) {
    if (!storage || storage.tenantId !== currentLocalClass()) return;
    status.textContent = storage.ready === true && storage.layout === "class_files_v1" && storage.migrationState === "verified"
      ? "학급별 로컬 DB 확인 완료 · 이전 학급 연결과 기록은 유지됩니다."
      : "학급 로컬 DB 준비를 확인하지 못했습니다. 기록을 저장하기 전 연결·저장 상태를 확인하세요.";
  } };
}
