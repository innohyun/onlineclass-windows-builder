import { initSharedArchive, type ArchiveBridge, type ArchiveDetail, type ArchiveSummary } from "./shared-archive";

const MB = 1024 * 1024;
const fixtures: ArchiveSummary[] = [
  { id: "archive-eco-assignment", sourceType: "assignment", title: "우리 동네 생태 조사", recordCount: 35, contentCount: 28, fileCount: 16, totalFileBytes: 84.2 * MB, importedAt: Date.parse("2026-08-04T14:18:00+09:00") },
  { id: "archive-environment-board", sourceType: "board", title: "환경보호 실천 게시판", recordCount: 63, contentCount: 42, fileCount: 21, totalFileBytes: 96.8 * MB, importedAt: Date.parse("2026-08-02T16:32:00+09:00") },
  { id: "archive-reading-assignment", sourceType: "assignment", title: "여름방학 독서 기록", recordCount: 29, contentCount: 24, fileCount: 5, totalFileBytes: 12.4 * MB, importedAt: Date.parse("2026-07-30T11:06:00+09:00") },
];

const detailFixtures: Record<string, ArchiveDetail> = {
  "archive-eco-assignment": {
    meta: fixtures[0],
    records: [
      { ordinal: 1, type: "assignment_submission", payload: { student_name_snapshot: "김하늘", class_no: 5, assignment_title_snapshot: "학교 주변에서 찾은 생태 연결고리", note: "학교 주변을 산책하며 개미가 이동하는 길과 주변 식물, 그늘진 곳을 관찰했습니다. 개미는 나무 아래 젖은 흙과 낙엽 근처에서 자주 발견되었습니다.\n이 식물들이 그늘을 만들어 주고 곤충들이 사는 환경을 제공한다는 것을 알게 되었습니다. 앞으로 더 다양한 생물을 관찰하고 우리 동네 생태를 지키는 방법을 실천하고 싶습니다.", student_submitted_at: Date.parse("2026-08-04T13:52:00+09:00"), teacher_feedback: "관찰한 생물과 주변 환경의 관계를 구체적으로 잘 설명했어요." } },
      { ordinal: 2, type: "assignment_submission", payload: { student_name_snapshot: "이서준", class_no: 12, assignment_title_snapshot: "운동장 나무와 새의 관계", note: "운동장 가장자리 나무에 앉는 새를 관찰하고 시간대별 변화를 기록했습니다.", student_submitted_at: Date.parse("2026-08-04T13:47:00+09:00") } },
      { ordinal: 3, type: "assignment_submission", payload: { student_name_snapshot: "박민지", class_no: 21, assignment_title_snapshot: "빗물 화단 관찰", note: "비가 온 다음 날 화단에 모인 작은 곤충과 식물의 변화를 살펴보았습니다.", student_submitted_at: Date.parse("2026-08-04T13:36:00+09:00") } },
    ],
    files: [
      { ordinal: 0, originalName: "생태지도_김하늘.pdf", contentType: "application/pdf", byteSize: 3.8 * MB },
      { ordinal: 1, originalName: "관찰사진_1.jpg", contentType: "image/jpeg", byteSize: 2.1 * MB },
      { ordinal: 2, originalName: "모둠발표자료.pptx", contentType: "application/vnd.openxmlformats-officedocument.presentationml.presentation", byteSize: 8.4 * MB },
    ],
  },
  "archive-environment-board": {
    meta: fixtures[1],
    records: [
      { ordinal: 1, type: "board_post", payload: { author_display_name: "김하늘", title: "일회용품을 줄였어요", content: "개인 물병과 장바구니를 사용한 경험을 사진과 함께 기록했습니다.", created_at: Date.parse("2026-08-02T15:20:00+09:00") } },
      { ordinal: 2, type: "board_post", payload: { author_display_name: "이서준", title: "교실 분리배출 약속", content: "우리 반에서 지킬 수 있는 분리배출 약속을 정리했습니다.", created_at: Date.parse("2026-08-02T15:04:00+09:00") } },
    ],
    files: [{ ordinal: 0, originalName: "분리배출_안내.pdf", contentType: "application/pdf", byteSize: 4.2 * MB }],
  },
  "archive-reading-assignment": {
    meta: fixtures[2],
    records: [{ ordinal: 1, type: "assignment_submission", payload: { student_name_snapshot: "박민지", class_no: 21, assignment_title_snapshot: "가장 기억에 남은 장면", note: "주인공이 친구의 마음을 이해하게 되는 장면을 중심으로 독서 기록을 작성했습니다.", student_submitted_at: Date.parse("2026-07-30T10:42:00+09:00") } }],
    files: [],
  },
};

function clone<T>(value: T): T {
  return structuredClone(value);
}

function previewBridge(state: string): ArchiveBridge {
  return {
    async list() {
      if (state === "error") return { ok: false, error: "archive_network_error" };
      return { ok: true, archives: state === "empty" ? [] : clone(fixtures) };
    },
    async detail(archiveId) {
      const archive = detailFixtures[archiveId];
      return archive ? { ok: true, archive: clone(archive) } : { ok: false, error: "archive_not_found" };
    },
    async import() {
      if (state === "expired") return { ok: false, error: "archive_http_401" };
      if (state === "manifest-verification") return { ok: false, error: "archive_manifest_verify_failed" };
      if (state === "record-verification") return { ok: false, error: "archive_record_verify_failed" };
      if (state === "file-verification") return { ok: false, error: "archive_file_verify_failed:0" };
      if (state === "resume") await new Promise((resolve) => window.setTimeout(resolve, 500));
      return { ok: true, archiveId: fixtures[0].id, title: fixtures[0].title, recordCount: 35, fileCount: 16 };
    },
    async exportArchive() {
      return state === "export-error" ? { ok: false, error: "archive_export_write_failed" } : { ok: true };
    },
    async openFile() {
      return state === "file-error" ? { ok: false, error: "archive_file_not_found" } : { ok: true };
    },
  };
}

export function initSharedArchivePreview() {
  const state = new URLSearchParams(window.location.search).get("archiveState") || "normal";
  document.getElementById("homeTenantLabel")!.textContent = "수영초등학교 5학년 1반";
  document.getElementById("homeConnectionText")!.textContent = "연결됨";
  document.getElementById("homeBackupText")!.textContent = "어제 오후 5:58";
  initSharedArchive({ bridge: previewBridge(state) });
}
