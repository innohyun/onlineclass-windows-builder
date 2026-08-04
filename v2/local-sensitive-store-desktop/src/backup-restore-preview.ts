import { confirmBackupRestore } from "./backup-restore-confirmation";

type PreviewBackup = {
  date: string;
  relation: string;
  pc: string;
  os: string;
  care: number;
  attendance: number;
  learning: number;
  studentRecord: number;
  board: number;
  attachments: number;
};

const backups: PreviewBackup[] = [
  { date: "2026년 8월 3일 오후 5:58", relation: "이 PC", pc: "SONG", os: "Windows 11", care: 327, attendance: 93, learning: 874, studentRecord: 174, board: 39, attachments: 248 },
  { date: "2026년 8월 2일 오후 5:58", relation: "이 PC", pc: "SONG", os: "Windows 11", care: 322, attendance: 91, learning: 861, studentRecord: 172, board: 38, attachments: 245 },
  { date: "2026년 8월 1일 오후 5:58", relation: "이 PC", pc: "SONG", os: "Windows 11", care: 318, attendance: 89, learning: 842, studentRecord: 170, board: 37, attachments: 239 },
  { date: "2026년 7월 31일 오후 5:58", relation: "이 PC", pc: "SONG", os: "Windows 11", care: 310, attendance: 87, learning: 823, studentRecord: 167, board: 36, attachments: 231 },
  { date: "2026년 7월 30일 오후 5:58", relation: "다른 PC", pc: "SHONG-KWS", os: "Windows 11", care: 305, attendance: 85, learning: 812, studentRecord: 165, board: 35, attachments: 226 },
];

function byId<T extends HTMLElement>(id: string) {
  const element = document.getElementById(id);
  if (!element) throw new Error(`missing element: ${id}`);
  return element as T;
}

function listMarkup(selectedIndex: number) {
  return backups.map((backup, index) => `
    <button class="backup-list-row${index === selectedIndex ? " is-selected" : ""}" type="button" data-backup-preview-index="${index}" aria-pressed="${index === selectedIndex}">
      <span class="backup-list-row__radio" aria-hidden="true"></span>
      <span class="backup-list-row__device" aria-hidden="true"><i class="fa-solid fa-desktop"></i></span>
      <span class="backup-list-row__main">
        <strong class="backup-list-row__time"><span>${backup.date}</span>${index === 0 ? '<span class="backup-list-row__badge">최신</span>' : ""}</strong>
        <span class="backup-list-row__meta">${backup.relation} · ${backup.pc} · ${backup.os}</span>
        <span class="backup-list-row__counts">관찰·상담 ${backup.care}건 · 출결·증빙 ${backup.attendance}건 · 평가·학습 ${backup.learning}건 · 학생부 ${backup.studentRecord}건 · 게시판 ${backup.board}건 · 첨부 ${backup.attachments}개</span>
      </span>
    </button>
  `).join("");
}

function renderSelection(index: number) {
  const backup = backups[index] || backups[0];
  byId("backupList").innerHTML = listMarkup(index);
  byId("backupRestoreBadge").textContent = "미리보기";
  byId("backupRestoreBadge").className = "status-badge badge-ok";
  byId("backupRestoreStatus").textContent = `${backup.date} 백업을 선택했습니다.`;
  byId("backupPreviewDetails").innerHTML = `
    <div class="restore-preview-source">
      <i class="fa-solid fa-desktop" aria-hidden="true"></i>
      <strong>${backup.pc} / ${backup.relation} / 앱 v0.2.25 / ${backup.os}</strong>
      <span>${backup.date} 백업</span>
    </div>
  `;
  byId("backupPreviewCare").textContent = `${backup.care}건`;
  byId("backupPreviewAttendance").textContent = `${backup.attendance}건`;
  byId("backupPreviewLearning").textContent = `${backup.learning}건`;
  byId("backupPreviewStudentRecord").textContent = `${backup.studentRecord}건`;
  byId("backupPreviewBoard").textContent = `${backup.board}건`;
  byId("backupPreviewAttachments").textContent = `${backup.attachments}개`;
  document.querySelectorAll<HTMLButtonElement>('.backup-view button[data-action]').forEach((button) => { button.disabled = false; });
}

export function initBackupRestorePreview() {
  const previewState = new URLSearchParams(window.location.search).get("backupState") || "normal";
  let selectedIndex = previewState === "other-pc" ? 4 : 0;
  byId<HTMLInputElement>("backupTenantInput").value = "preview-tenant";
  byId("homeTenantLabel").textContent = "수영초등학교 5학년 1반";
  byId("homeConnectionText").textContent = "연결됨";
  byId("homeBackupText").textContent = "어제 오후 5:58";
  byId("backupBadge").textContent = "자동 백업 정상";
  byId("backupBadge").className = "status-badge badge-ok";
  byId("backupFolderText").textContent = "학교 OneDrive · OnlineClassLocalBackups";
  byId("backupStatus").textContent = "마지막 자동 백업과 첨부파일 복사를 정상적으로 마쳤습니다.";
  byId("backupLatestText").textContent = "어제 오후 5:58";
  byId("backupNextText").textContent = "오늘 오후 5:58";
  byId("backupMediaText").textContent = "248개 · 누락 0개";
  if (previewState === "empty" || previewState === "error") {
    byId("backupList").innerHTML = `<p class="backup-list-empty">${previewState === "error" ? "백업 폴더에 접근할 수 없습니다. OneDrive 연결과 폴더 권한을 확인하세요." : "복원 가능한 백업이 없습니다. 지금 백업을 만들거나 백업 폴더를 다시 찾아보세요."}</p>`;
    byId("backupRestoreBadge").textContent = previewState === "error" ? "접근 오류" : "백업 없음";
    byId("backupRestoreBadge").className = `status-badge badge-${previewState === "error" ? "error" : "warning"}`;
    byId("backupRestoreStatus").textContent = previewState === "error" ? "학교 OneDrive 백업 폴더를 열지 못했습니다." : "선택한 폴더에 이 학급의 백업이 아직 없습니다.";
    byId("backupPreviewDetails").innerHTML = '<p class="restore-preview-empty">복원할 백업을 선택하면 PC 정보와 자료 건수가 표시됩니다.</p>';
    ["backupPreviewCare", "backupPreviewAttendance", "backupPreviewLearning", "backupPreviewStudentRecord", "backupPreviewBoard", "backupPreviewAttachments"].forEach((id) => { byId(id).textContent = "-"; });
    document.querySelectorAll<HTMLButtonElement>('.backup-view button[data-action="restore-backup"]').forEach((button) => { button.disabled = true; });
    if (previewState === "error") {
      byId("backupBadge").textContent = "자동 백업 폴더 확인 필요";
      byId("backupBadge").className = "status-badge badge-error";
      byId("backupStatus").textContent = "OneDrive 연결 또는 폴더 권한을 확인한 뒤 다시 찾아보세요.";
    }
  } else {
    renderSelection(selectedIndex);
  }

  document.addEventListener("click", (event) => {
    const target = event.target as HTMLElement | null;
    if (!target?.closest(".backup-view")) return;
    const row = target.closest<HTMLButtonElement>("[data-backup-preview-index]");
    if (row) {
      event.preventDefault();
      event.stopImmediatePropagation();
      selectedIndex = Number(row.dataset.backupPreviewIndex || 0);
      renderSelection(selectedIndex);
      return;
    }
    const action = target.closest<HTMLButtonElement>("button[data-action]")?.dataset.action;
    if (!action) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    if (action === "choose-backup-folder") {
      byId("backupRestoreStatus").textContent = "학교 OneDrive 백업 폴더에서 최신 목록을 다시 찾았습니다.";
      return;
    }
    if (action === "run-backup") {
      byId("backupStatus").textContent = "새 보호 백업을 만들었습니다. 첨부파일 누락은 없습니다.";
      return;
    }
    if (action === "restore-backup") {
      const backup = backups[selectedIndex];
      void confirmBackupRestore({
        date: backup.date,
        source: `${backup.pc} · ${backup.relation} · ${backup.os}`,
        summary: "선택한 백업의 자료와 첨부파일을 현재 PC에 병합합니다.",
      }).then((confirmed) => {
        if (confirmed) byId("backupRestoreStatus").textContent = "보호 백업 후 선택한 백업을 안전하게 복원했습니다.";
      });
    }
  }, true);
}
