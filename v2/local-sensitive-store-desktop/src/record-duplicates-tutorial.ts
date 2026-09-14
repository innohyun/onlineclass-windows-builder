const TUTORIAL_KEY = "localRecordDuplicatesTutorial:v1";

export function initDuplicatesTutorial(dialog: HTMLDialogElement, prepare: () => void) {
  const panel = dialog.querySelector<HTMLElement>("#recordDuplicatesTutorial")!;
  const steps = [
    ["recordDuplicatesScope", "검색 범위", "현재 학급의 전체 기간을 확인합니다. 학생별 보기에서 열면 선택한 학생으로 범위를 좁힐 수 있습니다."],
    ["recordDuplicatesScan", "읽기 전용 검색", "중복 검색은 자료를 바꾸지 않습니다. 학생·기록일·본문·기록 맥락이 정확히 같은 관찰을 찾습니다."],
    ["recordDuplicatesSummary", "남길 기록과 보관할 기록", "검색 결과에서 원문, 저장 시간, 남길 한 건과 보관 대상을 확인합니다. 학생기록 근거로 사용 중인 자료는 보호합니다."],
    ["recordDuplicatesApply", "검토한 중복 정리", "선택한 중복만 보관합니다. 원문과 이력은 유지되며, 변경된 자료가 있으면 다시 검토해야 합니다. 이 안내는 검색이나 정리를 실행하지 않습니다."],
    ["recordDuplicatesHistoryTab", "정리 내역과 되돌리기", "앱을 다시 실행해도 정리 내역을 확인할 수 있습니다. 정리 이후 자료가 바뀌지 않았을 때 해당 정리를 되돌릴 수 있습니다."],
  ];
  let index = -1;
  const clear = () => dialog.querySelectorAll(".duplicates-tutorial-target").forEach(node => node.classList.remove("duplicates-tutorial-target"));
  const position = () => {
    if (index < 0) return;
    const target = dialog.querySelector<HTMLElement>(`#${steps[index][0]}`)!;
    let rect = target.getBoundingClientRect();
    const box = panel.getBoundingClientRect();
    const margin = 16;
    if (rect.bottom + box.height + margin * 2 > innerHeight && rect.top < box.height + margin * 2) {
      dialog.scrollTop += rect.top - dialog.getBoundingClientRect().top - margin;
      rect = target.getBoundingClientRect();
    }
    const below = rect.bottom + margin;
    const top = below + box.height <= innerHeight - margin ? below : Math.max(margin, rect.top - box.height - margin);
    panel.style.top = `${top}px`;
    panel.style.left = `${Math.max(margin, Math.min(rect.left, innerWidth - box.width - margin))}px`;
  };
  const render = () => {
    clear();
    const [id, title, copy] = steps[index];
    panel.querySelector<HTMLElement>("[data-tutorial-step]")!.textContent = `${index + 1} / ${steps.length}`;
    panel.querySelector<HTMLElement>("h3")!.textContent = title;
    panel.querySelector<HTMLElement>("p")!.textContent = copy;
    panel.querySelector<HTMLButtonElement>("[data-tutorial-previous]")!.disabled = index === 0;
    panel.querySelector<HTMLElement>("[data-tutorial-next]")!.textContent = index === steps.length - 1 ? "안내 완료" : "다음";
    panel.hidden = false;
    dialog.classList.add("duplicates-tutorial-active");
    const target = dialog.querySelector<HTMLElement>(`#${id}`)!;
    target.classList.add("duplicates-tutorial-target");
    target.scrollIntoView({ block: "center", behavior: "instant" });
    position();
    requestAnimationFrame(position);
  };
  const close = (complete = false) => {
    clear(); index = -1; panel.hidden = true; dialog.classList.remove("duplicates-tutorial-active");
    if (complete) { try { localStorage.setItem(TUTORIAL_KEY, "complete"); } catch { /* Guidance remains available without storage. */ } }
  };
  const open = () => { prepare(); index = 0; render(); panel.querySelector<HTMLButtonElement>("[data-tutorial-close]")?.focus({ preventScroll: true }); };
  dialog.querySelector("#recordDuplicatesHelp")!.addEventListener("click", open);
  panel.querySelector("[data-tutorial-close]")!.addEventListener("click", () => close());
  panel.querySelector("[data-tutorial-previous]")!.addEventListener("click", () => { index = Math.max(0, index - 1); render(); });
  panel.querySelector("[data-tutorial-next]")!.addEventListener("click", () => { if (index === steps.length - 1) close(true); else { index += 1; render(); } });
  dialog.addEventListener("click", event => {
    if (index >= 0 && !panel.contains(event.target as Node)) { event.preventDefault(); event.stopImmediatePropagation(); }
  }, true);
  dialog.addEventListener("keydown", event => {
    if (index < 0) return;
    if (event.key === "Escape") { event.preventDefault(); event.stopImmediatePropagation(); close(); }
    else if (!panel.contains(event.target as Node)) { event.preventDefault(); panel.querySelector<HTMLButtonElement>("[data-tutorial-close]")?.focus(); }
  }, true);
  window.addEventListener("resize", position);
  dialog.addEventListener("scroll", position, true);
  return { close, offer() { try { if (localStorage.getItem(TUTORIAL_KEY) !== "complete") open(); } catch { open(); } } };
}
