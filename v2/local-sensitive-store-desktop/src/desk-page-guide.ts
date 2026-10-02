import { isDeskRestoreBlocked } from "./desk-restore-lock";

type Step = { target: string; title: string; text: string; fallback?: string };
const step = (target: string, title: string, text: string, fallback?: string): Step => ({ target, title, text, fallback });
const guides: Record<string, Step[]> = {
  home: [
    step("#deskCurrentStore", "현재 학급과 이 PC", "자료함 상세에서 학급과 저장 위치를 확인합니다. 다른 PC의 자료는 백업·동기화에서 검증하고 반영합니다."),
    step("#homeRecentWorkNotes", "최근 작업을 이어 쓰세요", "최근 수정한 수업자료와 업무 노트를 함께 표시합니다. 문서를 누르면 해당 원본을 엽니다."),
    step("#deskFavoritesPanel", "자주 쓰는 자료를 모으세요", "문서의 별표로 이 PC의 학급별 즐겨찾기를 바꿉니다. 제목 가리기는 학생 기록 가림과 별도 설정입니다."),
    step("#homeSafetyCard", "저장과 백업을 구분하세요", "문서 저장 여부는 문서 안에서 확인합니다. 백업 생성·게시·OneDrive 전달·다른 PC 확인은 각각 별도입니다."),
  ],
  "lesson-materials": [
    step("#localWorkspaceLessonQuery", "필요한 수업자료 찾기", "제목·본문·첨부로 검색합니다. 즐겨찾기와 선택한 폴더를 바꾸고, 목록으로 돌아와 작업 위치를 이어갑니다."),
    step("#localWorkspaceLessonTree", "폴더와 문서 선택", "원본 구조를 그대로 탐색합니다. 날짜나 제목이 비슷해도 서로 다른 문서를 자동으로 합치지 않습니다."),
    step("#localWorkspaceLessonOpen", "선택한 원본 편집", "로컬 자료는 인터넷 없이 편집합니다. 시간표에 연결된 문서의 제목·구조는 수업계획에서 관리합니다.", "#localWorkspaceLessonTreeTitle"),
    step("#deskNewDocument", "새 수업자료 만들기", "새로 만들기에서 저장 위치를 확인합니다. 이 안내는 문서를 생성하거나 저장하지 않습니다."),
  ],
  "work-materials": [
    step("#localWorkspaceWorkQuery", "업무 노트 찾기", "검색 조건·선택·목록 위치를 유지하며 회의 메모와 학급 운영 자료를 찾습니다."),
    step("#localWorkspaceWorkTree", "문서 구조 확인", "목록에서 문서를 선택하고 원본을 엽니다. 하위 문서와 첨부의 저장 대상을 확인하세요."),
    step("#localWorkspaceWorkOpen", "이 PC에서 작성", "본문은 로컬 DB에 저장합니다. 초안 보관과 실제 원본 저장 결과는 문서 안에서 구분합니다.", "#localWorkspaceWorkTreeTitle"),
    step("#deskMaterialsMenu", "이력·휴지통 접근", "수정 이력은 문서 안에서, 휴지통은 자료 관리에서 엽니다. 복구 전 대상과 영향을 직접 확인합니다."),
  ],
  "student-learning-materials": [
    step("#localWorkspaceStudentTitle", "학생 공개 자료의 원본", "새 학생 학습자료는 온라인 교사 홈에서 작성·검토·공개합니다. 인터넷과 교사 로그인이 필요합니다."),
    step("#localWorkspaceStudentTreeTitle", "기존 로컬 보관본", "이 PC에 남은 이전 자료는 읽기 전용입니다. 새 공개 원본을 대신하지 않습니다."),
    step("#localWorkspaceStudentOpen", "보관본 읽기", "선택한 자료와 첨부를 확인합니다. 로컬 보관본을 여는 것만으로 학생에게 공개되지 않습니다.", "#localWorkspaceStudentTreeTitle"),
    step("#deskOpenTeacher", "온라인에서 작성·공개", "교사 홈에서 대상 학생과 공개 상태를 확인합니다. 연결 실패 시 로컬 자료의 열람은 유지합니다."),
  ],
  students: [
    step("[data-student-privacy-toggle]", "학생 기록 가림", "처음에는 이름·원문·첨부를 가립니다. 가림을 해제하면 보관 중인 입력을 이어갑니다. 홈 문서 제목 가리기와 별개입니다."),
    step("#studentRosterList", "실제 학생 선택", "같은 이름이어도 실제 학생 ID로 구분합니다. 선택과 기간·종류 조건을 유지하며 기록을 확인합니다."),
    step("#studentTimelineList .student-timeline-row:first-child, #studentTimelineList .student-list-message > strong, #studentTimelineList .student-list-message > span", "관찰과 상담 상세", "선택한 정확한 기록을 다시 읽습니다. 관찰은 사유와 함께 정정하고 상담은 원본을 확인한 뒤 수정합니다."),
    step("#studentTimelineQuickToggle", "선택 학생의 빠른 관찰", "가림을 해제하고 선택한 학생을 이어받아 입력합니다. 저장 중에는 입력과 학생 변경을 잠급니다."),
  ],
  "quick-observation": [
    step("#quickRosterTitle", "기록할 학생 선택", "현재 학급의 실제 명단에서 한 명 또는 여러 명을 선택합니다. 오래된 명단은 확인 시각과 상태를 표시합니다."),
    step("#quickObservationContext", "관찰한 상황", "수업 관찰은 교시와 과목이 필요합니다. 쉬는 시간·상담·생활지도·기타는 일상 관찰로 구분합니다."),
    step("#quickObservationDate", "실제 발생 일시", "한국 시간의 발생 날짜와 시각 정확도를 기록합니다. 최초 저장 시각과 실제 발생 시각은 별도입니다."),
    step("#quickObservationSave", "저장 결과 확인", "필수 입력과 원본 재조회를 확인한 뒤 입력을 초기화합니다. 실패한 내용은 현재 창에 유지하며 자동 저장을 보장하지 않습니다."),
  ],
  data: [
    step("#dataExplorerQuery", "전체 자료 검색", "이 PC의 문서·학생 기록·첨부를 검색합니다. 검색어와 학생 이름은 이 안내에 저장하지 않습니다."),
    step("#dataExplorerGroup", "종류와 기간 좁히기", "수업자료·업무 노트를 함께 찾거나 다른 자료 종류를 선택합니다. 최신순은 저장·수정 시각 기준의 고정 정렬입니다."),
    step("#dataResultsTitle", "정확한 자료 선택", "선택과 페이지·필터·스크롤을 유지합니다. 학생 기록 가림이 켜져 있으면 원문과 첨부명도 가립니다."),
    step("#dataDetailTitle, #dataExplorerEmpty h2", "원본과 첨부 확인", "첨부 등록은 파일이 현재 열리는지 확인한 상태와 다릅니다. 문서는 원본을 다시 읽어 열고, 파일 실패는 따로 안내합니다."),
  ],
  backup: [
    step("#backupCard", "이 PC의 백업", "백업 생성은 현재 자료의 보호 사본을 만드는 작업입니다. 폴더·최근 결과·누락 파일을 확인합니다."),
    step("#deviceSyncPostedStage", "게시와 전달을 구분하세요", "게시 완료는 로컬 보관본과 서버 정보가 준비된 상태입니다. OneDrive 전달과 다른 PC의 적용 완료를 뜻하지 않습니다."),
    step("#deviceSyncTitle", "이 PC 반영과 확인 전송", "파일 도착·무결성 검증·이 PC 적용·확인 전송을 구분합니다. 대기와 실패에서는 현재 자료의 상태를 따로 확인합니다."),
    step("#backupPreviewTitle", "복원 전에 미리보기", "대상 학급·원본 PC·백업 종류와 영향을 검증합니다. 보호 백업을 확인한 뒤 병합하며, 수동 복원은 기기 동기화와 별개입니다."),
  ],
  archive: [
    step("#sharedArchiveTitle", "읽기 전용 보관소", "보관본은 원본 업무 문서·학생 원장과 별도로 유지합니다. 여기에서 원본을 수정하거나 학생에게 다시 공개하지 않습니다."),
    step("#sharedArchiveList", "보관본 선택", "목록에서 원형 보드나 자료 상세를 엽니다. 돌아오면 선택과 작업 위치를 유지합니다."),
    step("#sharedArchiveCodeInput", "보관 코드 가져오기", "사용 기한과 사용 횟수를 확인합니다. 가져오기에 실패하면 등록 결과를 확인한 뒤 유효한 코드로 다시 시도합니다."),
    step("#homeSearchForm", "보관 자료 찾기", "전체 검색의 보관 보드 조건으로 찾을 수 있습니다. 파일은 선택한 보관본의 검증된 첨부만 엽니다."),
  ],
  computers: [
    step("#deskComputerTitle", "현재 PC의 자료함", "현재 학급과 이 PC를 확인합니다. 이 화면은 다른 PC의 DB를 바로 선택해 여는 메뉴가 아닙니다."),
    step("#deskComputerStore", "저장 위치와 백업", "이 PC의 저장소 상태와 최근 백업 시각을 확인합니다. PC 표시 이름과 연결 학급 변경을 구분합니다."),
    step("#deskDevicesMenu", "새 PC 연결", "교사 로그인과 기기 승인을 완료한 뒤 백업을 확인합니다. 연결 완료만으로 원본 자료가 복원되지는 않습니다."),
    step("#deskComputerBackup", "다른 PC에서 이어 쓰기", "백업·동기화에서 게시·파일 전달·검증·적용·확인 전송을 확인합니다."),
  ],
  health: [
    step("#healthStoreCard h2", "이 PC 저장소", "실제 로컬 서비스 응답으로 상태를 확인합니다. 확인 실패를 데이터 손상이나 최신 버전으로 단정하지 않습니다."),
    step("#connectionCard h2", "계정·학급 연결", "승인된 계정과 학급을 확인합니다. 연결 전·만료·실패는 각 상태에 맞는 조작으로 이어갑니다."),
    step("#healthBackupCard h2", "백업과 기기 동기화", "백업 생성과 다른 PC의 확인 대기를 각각 표시합니다. OneDrive 폴더 오류와 로컬 사용 가능 여부도 구분합니다."),
    step("#summaryCard h2", "확인 시각과 해결 조작", "현재 응답과 확인 시각을 보고 관련 설정으로 이동합니다. 안내는 복원이나 설정 변경을 대신 실행하지 않습니다."),
  ],
  settings: [
    step("#settingsConnectionTitle", "현재 계정·학급", "연결된 실제 계정과 학급을 확인합니다. 연결 해제 전 로컬 자료·백업의 영향을 직접 확인합니다."),
    step("#deskHideHomeTitles", "화면·개인정보 설정", "홈 문서 제목 가리기와 학생 기록 가림은 별도입니다. 학생 가림은 앱을 시작할 때 켜집니다."),
    step("#settingsAdvancedPanel", "필요할 때 고급 정보", "서비스 주소와 저장 경로·실제 설치 버전을 확인합니다. 버전 표시만으로 업데이트 검사가 완료됐다고 판단하지 않습니다."),
    step("#settingsAppVersionFooter", "설치·다운로드 안내", "기존 설치 안내에서 공개 설치본을 확인합니다. 연결 설정과 앱 설치·업그레이드는 별도 작업입니다."),
  ],
  templates: [
    step("#deskTemplatesTitle", "서식으로 시작", "수업 계획·회의 기록·학급 운영 노트의 실제 빈 구성으로 시작합니다."),
    step('[data-desk-template="lesson-plan"]', "수업 계획 서식", "수업자료에 새 로컬 문서를 만듭니다. 기존 수업계획과 연결 문서를 자동으로 변경하지 않습니다."),
    step('[data-desk-template="meeting-note"]', "업무 노트 서식", "회의와 학급 운영 메모는 업무 노트에 새 문서로 저장합니다."),
    step("#deskNewDocument", "저장 대상 확인", "연결된 학급과 자료 종류를 확인한 뒤 직접 만드세요. 이 안내는 생성 버튼을 누르지 않습니다."),
  ],
  "ai-review": [
    step("#deskAiTitle", "기존 온라인 승인으로 이동", "AI 제안은 기존 교사 홈의 변경 검토 화면에서 확인합니다. 별도 AI 채팅이나 로컬 자동 승인을 만들지 않습니다."),
    step('[data-desk-action="ai-approvals"]', "원본과 제안 비교", "현재 원본·전체 본문·수정 범위·첨부 영향을 확인합니다. 요약과 실제 저장 본문을 구분합니다."),
    step('[data-desk-action="ai-settings"]', "연결과 저장 상태", "기기 연결·승인 대기·적용 대기·실제 저장 재조회는 각각 별도 상태입니다."),
    step("#deskOpenTeacher", "온라인 연결 필요", "인터넷과 교사 로그인이 필요합니다. 화면을 여는 것만으로 제안이 승인되지는 않습니다."),
  ],
  "document-workspace": [
    step("#dwTitle", "선택한 원본 편집", "현재 문서와 저장 대상을 확인하세요. 제목·수업 연결이 보호된 문서는 해당 범위를 수업계획에서 관리합니다."),
    step("#deskDocumentHost .dw-format", "본문과 블록 작성", "한글 입력이 조합 중일 때 저장·이동을 서두르지 않습니다. 표·링크·첨부 구조를 그대로 보존합니다."),
    step("#deskDocumentHost [data-doc-action=history]", "이력과 초안 확인", "현재 저장본과 내 초안을 비교합니다. 읽기만 해서 초안을 삭제하거나 원본을 덮어쓰지 않습니다."),
    step("#dwSaveState", "문서 안의 저장 상태", "저장 중·저장 실패·초안 보관·원본 재조회 결과를 확인하세요. 백업·공개 여부는 문서 저장과 별도입니다."),
  ],
};

export function initDeskPageGuide() {
  const launcher = document.getElementById("deskPageGuide")!;
  const panel = document.createElement("aside");
  panel.className = "desk-page-guide"; panel.hidden = true;
  panel.setAttribute("role", "dialog"); panel.setAttribute("aria-label", "현재 페이지 사용 안내");
  panel.innerHTML = '<header><span data-guide-count></span><button type="button" data-guide-close aria-label="안내 닫기">×</button></header><h2></h2><p></p><footer><button type="button" data-guide-previous>이전</button><button type="button" data-guide-next>다음</button><button type="button" data-guide-close>안내 종료</button></footer>';
  document.body.append(panel);
  let index = 0; let steps: Step[] = []; let target: HTMLElement | null = null; let route = "";
  const close = () => { target?.classList.remove("desk-page-guide-target"); panel.hidden = true; target = null; launcher.focus({ preventScroll: true }); };
  const render = () => {
    target?.classList.remove("desk-page-guide-target");
    const current = steps[index];
    const visible = (node: HTMLElement) => {
      const rect = node.getBoundingClientRect();
      let left = 0; let right = window.innerWidth;
      for (let parent = node.parentElement; parent; parent = parent.parentElement) {
        if (/hidden|auto|scroll|clip/.test(getComputedStyle(parent).overflowX)) {
          const bounds = parent.getBoundingClientRect(); left = Math.max(left, bounds.left); right = Math.min(right, bounds.right);
        }
      }
      return rect.width > 0 && rect.height > 0 && rect.left >= left && rect.right <= right && !node.closest("[hidden]");
    };
    const candidates=[...document.querySelectorAll<HTMLElement>(current.target), ...(current.fallback ? document.querySelectorAll<HTMLElement>(current.fallback) : []), ...document.querySelectorAll<HTMLElement>(`[data-app-view="${route}"] h1`), launcher];
    target = candidates.find(visible) || null;
    if (!target) { close(); return; }
    target.classList.add("desk-page-guide-target"); target.scrollIntoView({ block: "center", inline: "nearest", behavior: "instant" });
    panel.querySelector("h2")!.textContent = current.title;
    panel.querySelector("p")!.textContent = current.text;
    panel.querySelector("[data-guide-count]")!.textContent = `${index + 1} / ${steps.length}`;
    (panel.querySelector("[data-guide-previous]") as HTMLButtonElement).disabled = index === 0;
    panel.querySelector("[data-guide-next]")!.textContent = index === steps.length - 1 ? "안내 완료" : "다음";
    position();
  };
  const position = () => {
    if (panel.hidden || !target) return;
    const rect = target.getBoundingClientRect(); const width = Math.min(350, window.innerWidth - 24);
    panel.style.width = `${width}px`;
    const height = panel.getBoundingClientRect().height;
    const clampLeft = (left: number) => Math.max(12, Math.min(left, window.innerWidth - width - 12));
    const clampTop = (top: number) => Math.max(12, Math.min(top, window.innerHeight - height - 12));
    const candidates = [
      { left: clampLeft(rect.left), top: rect.bottom + 12 },
      { left: clampLeft(rect.left), top: rect.top - height - 12 },
      { left: rect.right + 12, top: clampTop(rect.top) },
      { left: rect.left - width - 12, top: clampTop(rect.top) },
    ];
    const position = candidates.find(({ left, top }) => left >= 12 && top >= 12 && left + width <= window.innerWidth - 12 && top + height <= window.innerHeight - 12);
    panel.style.left = `${position?.left ?? clampLeft(rect.left)}px`; panel.style.top = `${position?.top ?? clampTop(window.innerHeight)}px`;
  };
  launcher.addEventListener("click", () => {
    if (isDeskRestoreBlocked()) return;
    if (!panel.hidden) { close(); return; }
    route = document.body.dataset.appView || "home";
    if (route === "backup" && document.body.dataset.deskSection === "backupStoragePanel") { document.getElementById("backupStorageHelp")?.click(); return; }
    steps = route === "backup" && document.body.dataset.deskSection === "backupRestorePanel" ? [
      step("#backupList","백업 선택","대상 학급·백업 종류·날짜와 원본 PC를 확인하세요. 선택만으로 복원되지 않습니다."),
      step("#backupRestoreVerification","미리보기 검증","자료·첨부·원장 분기·수업 연결을 실행 직전에 다시 검증합니다. 검증 실패 시 복원을 차단합니다."),
      step('[data-action="restore-backup"]',"보호 범위 확인 후 실행","현재 상태 보호 백업이 검증된 후 병합합니다. 로그인 정보와 온라인 원본은 보호 범위에 포함되지 않습니다."),
    ] : guides[route] || [];
    if (!steps.length) return;
    index = 0; panel.hidden = false; render(); (panel.querySelector("[data-guide-next]") as HTMLButtonElement).focus({ preventScroll: true });
  });
  panel.querySelectorAll("[data-guide-close]").forEach((button) => button.addEventListener("click", close));
  panel.querySelector("[data-guide-previous]")!.addEventListener("click", () => { index = Math.max(0, index - 1); render(); });
  panel.querySelector("[data-guide-next]")!.addEventListener("click", () => {
    if (index < steps.length - 1) { index++; render(); }
    else { try { localStorage.setItem(`classaimateDeskPageGuide:${route}:v3`, "complete"); } catch { /* Session guide remains usable. */ } close(); }
  });
  window.addEventListener("desk:view-changed", () => { if (!panel.hidden) close(); });
  window.addEventListener("desk:restore-lock-changed", () => { if (!panel.hidden && isDeskRestoreBlocked()) close(); });
  window.addEventListener("resize", position);
  document.addEventListener("scroll", position, true);
  document.addEventListener("keydown", (event) => { if (!panel.hidden && event.key === "Escape" && !event.isComposing) { event.preventDefault(); close(); } });
}
