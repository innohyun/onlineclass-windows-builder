import { invoke } from "@tauri-apps/api/core";
import { initDeskNavigation } from "./desk-navigation";
import { initDocumentWorkspace } from "./document-workspace";
import { loadHomeOverview, type initHomeDashboard } from "./home-dashboard";
import { initLocalWorkspaces } from "./local-workspaces";
import type { initDesktopShell } from "./desktop-shell";

export function initTeacherDeskDocuments(options: {
  getTenantId: () => string;
  openTeacherHome: (path: string) => void | Promise<void>;
}) {
  const localWorkspaces = initLocalWorkspaces({
    getTenantId: options.getTenantId,
    openDocument: (input) => { document.dispatchEvent(new CustomEvent("desk:open-document", { detail: input })); },
  });
  const documentWorkspace = initDocumentWorkspace({
    getTenantId: options.getTenantId,
    openTeacherHome: (path) => { void options.openTeacherHome(path); },
    onChanged: () => { void localWorkspaces.refresh(); void loadHomeOverview(options.getTenantId()); },
  });
  return { localWorkspaces, documentWorkspace };
}

export function bindTeacherDeskLifecycle(options: {
  getTenantId: () => string;
  desktopShell: ReturnType<typeof initDesktopShell>;
  homeDashboard: ReturnType<typeof initHomeDashboard>;
  canLeave: () => Promise<boolean>;
  waitForPaint: () => Promise<void>;
}) {
  const { desktopShell, homeDashboard, canLeave, waitForPaint } = options;
  const deskNavigation = initDeskNavigation({ getTenantId: options.getTenantId, navigate: homeDashboard.navigate, openTeacherHome: desktopShell.openTeacherHome });
  window.addEventListener("desk:close-error", () => deskNavigation.notify("저장 확인 중 문제가 생겨 앱을 닫지 않았습니다. 편집 내용과 저장 상태를 확인해 주세요."));
  void desktopShell.startCloseHandling(canLeave).catch(() => deskNavigation.notify("앱 종료 보호를 준비하지 못했습니다. 문서를 저장한 뒤 창을 닫아 주세요."));
  void desktopShell.startActivationHandling(async (intent) => {
    const shortcutTarget = document.querySelector<HTMLButtonElement>('.sidebar-link[data-app-view-target="quick-observation"]');
    if (!shortcutTarget) throw new Error("quick_observation_target_missing");
    if (!await homeDashboard.navigate("quick-observation")) return;
    await waitForPaint();
    if (document.body.dataset.appView !== "quick-observation") throw new Error("quick_observation_view_not_activated");
    await invoke<boolean>("acknowledge_desktop_activation_intent", { intent });
  });
}
