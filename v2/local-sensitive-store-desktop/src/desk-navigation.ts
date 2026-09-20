import { isTauri } from "@tauri-apps/api/core";
import type { HomeStatus } from "./home-dashboard";

type DeskNavigationOptions = {
  getTenantId: () => string;
  navigate: (view: string, context?: { group?: string; sectionKey?: string; attachment?: boolean }) => Promise<boolean>;
  openTeacherHome: (path?: string) => Promise<void>;
};
type ScreenLink = { title: string; view?: string; action?: string; target?: string; attachment?: boolean; description: string };
export const DESK_SCREENS: readonly ScreenLink[] = [
  { title: "내 작업실 홈", view: "home", description: "최근 문서와 즐겨찾기" },
  { title: "컴퓨터·자료함 선택", view: "computers", description: "현재 학급과 이 PC 확인" },
  { title: "새 컴퓨터 시작 설정", view: "settings", target: "deviceAuthPanel", description: "교사 로그인·기기 연결" },
  { title: "컴퓨터·자료함 상세", view: "computers", description: "저장 위치·연결 상태" },
  { title: "수업자료 목록", view: "lesson-materials", description: "로컬 수업자료 찾아보기" },
  { title: "수업자료 편집", view: "lesson-materials", description: "목록에서 문서를 선택해 열기" },
  { title: "학생 학습자료 목록", view: "student-learning-materials", description: "기존 로컬 자료와 교사 홈" },
  { title: "학생 학습자료 편집·공개", action: "student-materials", description: "기존 교사 홈에서 검토·공개" },
  { title: "업무 노트 작성·편집", view: "work-materials", description: "문서를 선택하거나 새로 작성" },
  { title: "학생 기록 조회·수정", view: "students", description: "학생별 기록과 정정 경로" },
  { title: "빠른 관찰 기록", view: "quick-observation", description: "학생을 선택해 관찰 작성" },
  { title: "상담기록 작성·편집", action: "counseling", description: "학생 선택·로컬 상담 작성" },
  { title: "전체 검색", view: "data", description: "문서·기록·보관 보드 검색" },
  { title: "첨부파일 모음", view: "data", attachment: true, description: "첨부 있는 자료와 파일 상세" },
  { title: "백업·동기화", view: "backup", target: "deviceSyncCard", description: "게시와 다른 PC 적용 상태" },
  { title: "동기화 충돌 비교", view: "backup", action: "conflicts", description: "기존 도메인별 충돌 비교" },
  { title: "백업 복원 미리보기", view: "backup", target: "backupRestorePanel", description: "백업 선택 후 내용 확인" },
  { title: "보관소", view: "archive", description: "읽기 전용 보관본" },
  { title: "휴지통·삭제 복구", action: "trash", description: "로컬 문서 30일 보관·복구" },
  { title: "수정 이력·버전 복원", action: "history", description: "문서의 최근 30일 변경 이력" },
  { title: "서식함", view: "templates", description: "서식으로 새 문서 작성" },
  { title: "AI 변경 제안 검토", view: "ai-review", description: "원본·제안·적용 결과 확인" },
  { title: "설정", view: "settings", description: "연결·저장·앱 동작" },
  { title: "연결·저장 상태", view: "health", description: "상태 확인과 복구 안내" },
];

const icon = (name: string) => `<i class="fa-solid fa-${name}" aria-hidden="true"></i>`;
const required = <T extends HTMLElement>(id: string) => {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing desk element: ${id}`);
  return node as T;
};

export function mountDeskViews() {
  required("homeTitle").closest(".workspace-scroll")!.insertAdjacentHTML("beforeend", `
    <section class="app-view desk-computers" data-app-view="computers" aria-labelledby="deskComputersTitle" hidden>
      <header class="desk-page-heading"><div><p class="home-eyebrow">YOUR CONNECTED WORKSPACE</p><h1 id="deskComputersTitle">컴퓨터·자료함</h1><p>현재 연결된 학급과 이 PC의 자료를 확인합니다.</p></div><button class="desk-outline" data-app-view-target="settings" type="button">${icon("plus")} 새 컴퓨터 연결 안내</button></header>
      <section class="desk-surface desk-computer-card"><div class="desk-computer-heading"><span class="desk-large-icon">${icon("desktop")}</span><div><span class="desk-tag">이 PC · 현재 자료함</span><h2 id="deskComputerTitle">이 PC 자료함</h2><p id="deskComputerTenant">연결된 학급 확인 중</p></div><span id="deskComputerState" class="desk-tag">확인 중</span></div><dl class="desk-detail-grid"><div><dt>로컬 저장소</dt><dd id="deskComputerStore">확인 중</dd></div><div><dt>최근 백업</dt><dd id="deskComputerBackup">확인 중</dd></div><div><dt>계정 연결</dt><dd id="deskComputerConnection">확인 중</dd></div></dl><div class="desk-actions"><button type="button" class="desk-primary" data-app-view-target="home">자료함 열기</button><button type="button" class="desk-outline" data-action="open-data-directory">저장 위치 열기</button><button type="button" class="desk-outline" data-app-view-target="settings">연결 설정</button></div></section>
      <div class="desk-grid-two"><section class="desk-surface desk-explain"><h2>${icon("arrows-rotate")} 다른 컴퓨터의 자료</h2><p>학교·집 컴퓨터 사이의 자료 전달은 기존 백업·동기화에서 확인합니다. 다른 PC의 DB를 바로 전환해 열지는 않습니다.</p><button type="button" class="desk-outline" data-app-view-target="backup">백업·동기화 열기</button></section><section class="desk-surface desk-explain"><h2>${icon("shield-halved")} 새 컴퓨터에서 시작</h2><p>설정에서 교사 로그인으로 기기를 연결한 뒤, 백업 내용을 확인하고 복원합니다. 기존 자료는 그대로 보존합니다.</p><div class="desk-actions"><button type="button" class="desk-outline" data-app-view-target="settings">1. 기기 연결 확인</button><button type="button" class="desk-outline" data-desk-action="restore-preview">2. 복원 미리보기</button></div></section></div>
    </section>
    <section class="app-view" data-app-view="templates" aria-labelledby="deskTemplatesTitle" hidden>
      <header class="desk-page-heading"><div><p class="home-eyebrow">START WITH A TEMPLATE</p><h1 id="deskTemplatesTitle">서식함</h1><p>기본 구성으로 새 문서를 만들고 본문에서 자유롭게 작성하세요.</p></div><button type="button" class="desk-outline" data-desk-action="new">빈 문서 만들기</button></header>
      <div class="desk-template-grid">
        <button type="button" class="desk-template" data-desk-template="lesson-plan"><span class="desk-metric-icon">${icon("book-open")}</span><h2>수업 계획</h2><p>수업 목표, 활동 절차, 발문, 평가를 작성하는 빈 서식입니다.</p><small>수업자료 · 새 로컬 문서</small></button>
        <button type="button" class="desk-template" data-desk-template="meeting-note"><span class="desk-metric-icon is-violet">${icon("users")}</span><h2>회의 기록</h2><p>안건, 논의 내용, 결정 사항과 후속 업무를 정리합니다.</p><small>업무 노트 · 새 로컬 문서</small></button>
        <button type="button" class="desk-template" data-desk-template="class-note"><span class="desk-metric-icon is-amber">${icon("note-sticky")}</span><h2>학급 운영 노트</h2><p>할 일과 준비물, 전달 사항을 작성하는 빈 서식입니다.</p><small>업무 노트 · 새 로컬 문서</small></button>
      </div><p class="desk-panel-note">서식은 새 문서로 생성합니다. 기존 수업계획이나 학생 기록을 변경하지 않습니다.</p><p class="desk-inline-message" id="deskTemplateStatus" role="status"></p>
    </section>
    <section class="app-view" data-app-view="ai-review" aria-labelledby="deskAiTitle" hidden>
      <header class="desk-page-heading"><div><p class="home-eyebrow">REVIEW BEFORE APPLYING</p><h1 id="deskAiTitle">AI 변경 제안 검토</h1><p>ChatGPT·MCP가 제안한 내용을 기존 승인 화면에서 확인합니다.</p></div></header>
      <section class="desk-surface desk-ai-intro"><span class="desk-large-icon is-violet">${icon("wand-magic-sparkles")}</span><h2>계획한 전체 내용이 그대로 들어가는지 확인하세요</h2><p>요약을 요청하지 않았다면 전체 수업안과 문서 구조를 기준으로 검토합니다. 변경 요약과 실제 저장할 본문은 서로 다릅니다.</p><div class="desk-actions"><button type="button" class="desk-primary" data-desk-action="ai-approvals">변경 제안·승인 화면 열기 ${icon("arrow-up-right-from-square")}</button><button type="button" class="desk-outline" data-desk-action="ai-settings">ChatGPT·MCP 연결 설정</button></div><small>인터넷과 교사 로그인이 필요합니다. 원격 화면을 여는 것만으로 승인되지 않습니다.</small></section>
      <div class="desk-grid-three desk-ai-steps"><section class="desk-surface desk-explain"><span class="desk-step">1</span><h2>원본과 제안 비교</h2><p>대상 문서, 전체 본문, 수정 범위와 첨부 영향을 확인합니다.</p></section><section class="desk-surface desk-explain"><span class="desk-step">2</span><h2>검토 후 적용</h2><p>현재 원본과 충돌하면 다시 읽고 제안을 검토합니다.</p></section><section class="desk-surface desk-explain"><span class="desk-step">3</span><h2>실제 저장 결과 확인</h2><p>승인 대기·기기 반영 대기와 저장·재조회 완료를 구분합니다.</p></section></div>
    </section>`);
  document.body.insertAdjacentHTML("beforeend", `
    <dialog class="desk-dialog" id="deskScreenDialog" aria-labelledby="deskScreenDialogTitle"><header><div><h2 id="deskScreenDialogTitle">작업실 화면 목록</h2><p>24개 작업 흐름의 실제 화면으로 이동합니다.</p></div><button type="button" data-desk-close="deskScreenDialog" aria-label="화면 목록 닫기">${icon("xmark")}</button></header><div class="desk-screen-grid">${DESK_SCREENS.map((screen, index) => `<button type="button" data-desk-screen="${index}"><span>${String(index + 1).padStart(2, "0")}</span><div><strong>${screen.title}</strong><small>${screen.description}</small></div></button>`).join("")}</div></dialog>
    <dialog class="desk-dialog desk-new-dialog" id="deskNewDialog" aria-labelledby="deskNewTitle"><header><div><h2 id="deskNewTitle">새로 만들기</h2><p>어떤 자료를 작성할까요?</p></div><button type="button" data-desk-close="deskNewDialog" aria-label="새로 만들기 닫기">${icon("xmark")}</button></header><div class="desk-new-grid"><button type="button" data-desk-create="workNotes">${icon("pen-to-square")}<strong>업무 노트</strong><small>이 PC에서 작성·저장</small></button><button type="button" data-desk-create="lessonMaterials">${icon("book-open")}<strong>수업자료</strong><small>이 PC의 수업자료에 저장</small></button><button type="button" data-desk-action="quick">${icon("bolt")}<strong>빠른 관찰 기록</strong><small>학생을 선택해 기록</small></button><button type="button" data-desk-action="counseling">${icon("comment-dots")}<strong>상담 기록</strong><small>학생을 선택해 상담 작성</small></button><button type="button" data-desk-action="student-materials">${icon("user-graduate")}<strong>학생 학습자료</strong><small>교사 홈에서 작성·공개</small></button></div><p class="desk-inline-message" id="deskNewStatus" role="status"></p></dialog>
    <div id="deskNavigationStatus" class="desk-navigation-status" role="status" hidden></div>`);
  const settings = document.querySelector('[data-app-view="settings"] .settings-advanced-panel');
  settings?.insertAdjacentHTML("beforebegin", `<section class="settings-wide-panel desk-display-preferences"><header><h2>작업실 화면</h2></header><label><span><strong>홈 문서 제목 가리기</strong><small>화면 공유 시 최근 작업·즐겨찾기의 제목을 숨깁니다.</small></span><input id="deskHideHomeTitles" type="checkbox" role="switch"></label><label><span><strong>촘촘한 화면 간격</strong><small>목록과 메뉴의 간격을 줄입니다.</small></span><input id="deskCompactSpacing" type="checkbox" role="switch"></label></section>`);
}

export function initDeskNavigation(options: DeskNavigationOptions) {
  let status: HomeStatus | undefined;
  let notificationTimer: number | undefined;
  const notify = (message: string) => {
    const node = required("deskNavigationStatus"); node.textContent = message; node.hidden = false;
    window.clearTimeout(notificationTimer); notificationTimer = window.setTimeout(() => { node.hidden = true; }, 7000);
  };
  const closeDialogs = () => { for (const id of ["deskScreenDialog", "deskNewDialog"]) { const dialog = required<HTMLDialogElement>(id); if (dialog.open) dialog.close(); } };
  const openTeacher = async (path: string) => {
    closeDialogs();
    if (!isTauri()) { notify("교사 홈 연결은 설치형 앱에서 사용할 수 있습니다."); return; }
    try { await options.openTeacherHome(path); } catch { notify("교사 홈을 열지 못했습니다. 연결 상태를 확인해 주세요."); }
  };
  const create = (workspace: "workNotes" | "lessonMaterials", templateId?: string) => {
    if (!options.getTenantId()) { notify("설정에서 학급과 이 PC를 먼저 연결해 주세요."); return; }
    if (!isTauri()) { notify("문서 저장은 설치형 앱의 로컬 DB에서 지원합니다."); return; }
    closeDialogs();
    document.dispatchEvent(new CustomEvent("desk:create-document", { detail: { workspace, ...(templateId ? { templateId } : {}) } }));
  };
  const focusTarget = (id: string) => {
    const node = document.getElementById(id);
    if (!node || node.hidden) return;
    node.scrollIntoView({ block: "center", behavior: "smooth" });
    node.tabIndex = -1; node.focus({ preventScroll: true });
  };
  const action = async (name: string) => {
    if (name === "new") { required("deskNewStatus").textContent = options.getTenantId() ? "수업자료·업무 노트는 인터넷 연결 없이 작성할 수 있습니다." : "로컬 문서를 작성하려면 설정에서 학급과 이 PC를 연결하세요."; required<HTMLDialogElement>("deskNewDialog").showModal(); return; }
    closeDialogs();
    if (name === "trash" || name === "history") { document.dispatchEvent(new CustomEvent(`desk:open-${name}`)); return; }
    if (name === "favorites") { if (await options.navigate("home")) focusTarget("deskFavoritesPanel"); return; }
    if (name === "quick") { await options.navigate("quick-observation"); return; }
    if (name === "restore-preview") { if (await options.navigate("backup")) focusTarget("backupRestorePanel"); return; }
    if (name === "conflicts") { if (await options.navigate("backup")) required<HTMLButtonElement>("deviceSyncConflictsOpen").click(); return; }
    if (name === "student-materials") { await openTeacher("/admin/student-learning-materials"); return; }
    if (name === "counseling") { if (!options.getTenantId()) { notify("설정에서 학급과 이 PC를 먼저 연결해 주세요."); return; } window.dispatchEvent(new CustomEvent("desk:create-counseling")); return; }
    if (name === "ai-approvals") { await openTeacher("/admin/mcp-approvals"); return; }
    if (name === "ai-settings") { await openTeacher("/admin/settings?tab=ai"); }
  };
  required("deskScreenIndex").addEventListener("click", () => required<HTMLDialogElement>("deskScreenDialog").showModal());
  document.addEventListener("click", (event) => {
    const target = event.target as HTMLElement | null;
    const closer = target?.closest<HTMLElement>("[data-desk-close]");
    if (closer) { required<HTMLDialogElement>(closer.dataset.deskClose!).close(); return; }
    const creator = target?.closest<HTMLElement>("[data-desk-create]");
    if (creator) { create(creator.dataset.deskCreate as "workNotes" | "lessonMaterials"); return; }
    const template = target?.closest<HTMLElement>("[data-desk-template]");
    if (template) { const templateId = template.dataset.deskTemplate!; create(templateId === "lesson-plan" ? "lessonMaterials" : "workNotes", templateId); return; }
    const actionButton = target?.closest<HTMLElement>("[data-desk-action]");
    if (actionButton) { void action(actionButton.dataset.deskAction!); return; }
    const screenButton = target?.closest<HTMLElement>("[data-desk-screen]");
    if (screenButton) {
      const screen = DESK_SCREENS[Number(screenButton.dataset.deskScreen)]; if (!screen) return;
      closeDialogs();
      if (screen.action) { void action(screen.action); return; }
      void options.navigate(screen.view!, { attachment: screen.attachment }).then((moved) => { if (moved && screen.target) focusTarget(screen.target); });
    }
  });
  document.addEventListener("keydown", (event) => {
    if ((!event.ctrlKey && !event.metaKey) || event.altKey || document.querySelector("dialog[open]")) return;
    if (event.key.toLowerCase() === "k") { event.preventDefault(); required<HTMLInputElement>("homeSearchInput").focus(); }
    if (event.key.toLowerCase() === "j") { event.preventDefault(); void options.navigate("quick-observation"); }
  });
  window.addEventListener("desk:status", (event) => {
    status = (event as CustomEvent<HomeStatus>).detail;
    required("deskComputerTitle").textContent = status.deviceName || "이 PC 자료함";
    required("deskComputerTenant").textContent = status.tenantLabel;
    required("deskComputerState").textContent = status.connected ? "현재 사용 중" : "연결 필요";
    required("deskComputerStore").textContent = status.storeReady ? "정상" : "확인 필요";
    required("deskComputerBackup").textContent = status.backupAtMs ? new Date(status.backupAtMs).toLocaleString("ko-KR") : "아직 없음";
    required("deskComputerConnection").textContent = status.connected ? "학급 연결됨" : "연결 필요";
  });
  for (const [id, key, className] of [["deskHideHomeTitles", "classaimateDeskHideHomeTitles:v1", "desk-hide-home-titles"], ["deskCompactSpacing", "classaimateDeskCompact:v1", "desk-compact"]]) {
    const input = required<HTMLInputElement>(id);
    try { input.checked = localStorage.getItem(key) === "true"; } catch { input.checked = false; }
    document.body.classList.toggle(className, input.checked);
    input.addEventListener("change", () => { document.body.classList.toggle(className, input.checked); try { localStorage.setItem(key, String(input.checked)); } catch { notify("화면 설정을 보관하지 못했습니다. 이번 실행에만 적용됩니다."); } });
  }
  return { notify };
}
