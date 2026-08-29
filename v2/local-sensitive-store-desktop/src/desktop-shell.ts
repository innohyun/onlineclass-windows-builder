import { invoke, isTauri } from "@tauri-apps/api/core";
import { LogicalPosition, LogicalSize } from "@tauri-apps/api/dpi";
import { listen } from "@tauri-apps/api/event";
import { Webview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";

const SHELL_HEIGHT = 56;
const TEACHER_WEBVIEW_LABEL = "teacher-home";
const TEACHER_HOME_URL = "https://t.classaimate.com/admin/";
const TUTORIAL_KEY = "classaimateDesktopShellTutorial:v6";

type ShellMode = "teacher" | "local";
export type DesktopActivationIntent = "show-main" | "quick-observation";
type DesktopShellController = {
  refreshConnection: () => Promise<void>;
  startActivationHandling: (handler: (intent: DesktopActivationIntent) => void | Promise<void>) => Promise<void>;
};

type BridgeResult = {
  ok?: boolean;
  connected?: boolean;
  requestId?: string;
  tenantId?: string;
  tenantName?: string;
  expiresAtMs?: number;
  error?: string;
};

type ConnectionResult = {
  ok?: boolean;
  connected?: boolean;
  tenantId?: string;
  tenantName?: string;
};

function required<T extends HTMLElement>(id: string) {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing desktop shell element: ${id}`);
  return node as T;
}

export function buildTeacherHomeUrl(bridge: BridgeResult) {
  const url = new URL(TEACHER_HOME_URL);
  url.searchParams.set("view", "overview");
  if (bridge.tenantId) url.searchParams.set("tenantId", bridge.tenantId);
  if (bridge.connected && bridge.requestId && bridge.tenantId && bridge.expiresAtMs) {
    const fragment = new URLSearchParams({
      classaimateDesktopLocal: "1",
      requestId: bridge.requestId,
      tenantId: bridge.tenantId,
      expiresAtMs: String(bridge.expiresAtMs),
    });
    url.hash = fragment.toString();
  }
  return url.toString();
}

export function desktopShellErrorMessage(value: unknown) {
  const code = String(value instanceof Error ? value.message : value || "");
  if (code === "local_store_service_unavailable") {
    return "이 PC 저장 서비스가 시작되지 않았습니다. 이미 실행 중인 앱이 있으면 트레이 아이콘에서 열고, 없으면 앱을 완전히 종료한 뒤 다시 실행해 주세요.";
  }
  if (code === "browser_link_unavailable") {
    return "로컬 저장소 연결을 준비하지 못했습니다. 앱을 완전히 종료한 뒤 다시 실행해 주세요.";
  }
  return "교사 홈 연결을 준비하지 못했습니다. 잠시 뒤 다시 시도해 주세요.";
}

export function initDesktopShell(): DesktopShellController {
  const noop: DesktopShellController = { refreshConnection: async () => undefined, startActivationHandling: async (_handler) => undefined };
  if (!isTauri()) return noop;

  const bar = required<HTMLElement>("desktopShellBar");
  const teacherButton = required<HTMLButtonElement>("desktopTeacherHome");
  const localButton = required<HTMLButtonElement>("desktopLocalArchive");
  const reloadButton = required<HTMLButtonElement>("desktopTeacherReload");
  const helpButton = required<HTMLButtonElement>("desktopShellHelp");
  const retryButton = required<HTMLButtonElement>("desktopTeacherRetry");
  const status = required<HTMLElement>("desktopShellStatus");
  const fallback = required<HTMLElement>("desktopTeacherFallback");
  const storeCandidate = required<HTMLElement>("desktopShellTutorial").parentElement?.querySelector<HTMLElement>(".store-shell");
  const tutorial = required<HTMLDialogElement>("desktopShellTutorial");
  const tutorialBody = required<HTMLElement>("desktopShellTutorialBody");
  const tutorialStep = required<HTMLElement>("desktopShellTutorialStep");
  const tutorialPrevious = required<HTMLButtonElement>("desktopShellTutorialPrevious");
  const tutorialNext = required<HTMLButtonElement>("desktopShellTutorialNext");
  if (!storeCandidate) throw new Error("missing local store shell");
  const store = storeCandidate;

  const appWindow = getCurrentWindow();
  let teacherWebview: Webview | null = null;
  let creating: Promise<Webview> | null = null;
  let mode: ShellMode = "local";
  let tutorialIndex = 0;
  let tutorialReturnMode: ShellMode = "teacher";
  let tutorialFirstRun = false;

  bar.hidden = false;
  document.body.classList.add("is-desktop-shell");

  function setConnectionStatus(connection: ConnectionResult) {
    const label = status.querySelector("span");
    status.classList.toggle("is-connected", connection.connected === true);
    if (label) {
      label.textContent = connection.connected
        ? `${connection.tenantName || connection.tenantId || "학급"} · 이 PC 저장소 연결됨`
        : "이 PC 저장소 연결 필요";
    }
  }

  function setFallback(title: string, detail: string, failed = false) {
    fallback.hidden = false;
    const icon = fallback.querySelector("i");
    const heading = fallback.querySelector("h1");
    const paragraph = fallback.querySelector("p");
    if (icon) icon.className = failed ? "fa-solid fa-triangle-exclamation" : "fa-solid fa-spinner fa-spin";
    if (heading) heading.textContent = title;
    if (paragraph) paragraph.textContent = detail;
    retryButton.hidden = !failed;
  }

  async function webviewBounds() {
    const [physical, scaleFactor] = await Promise.all([appWindow.innerSize(), appWindow.scaleFactor()]);
    const logical = physical.toLogical(scaleFactor);
    return { width: Math.max(320, logical.width), height: Math.max(180, logical.height - SHELL_HEIGHT) };
  }

  async function sizeTeacherWebview() {
    if (!teacherWebview) return;
    const bounds = await webviewBounds();
    await teacherWebview.setPosition(new LogicalPosition(0, SHELL_HEIGHT));
    await teacherWebview.setSize(new LogicalSize(bounds.width, bounds.height));
  }

  async function createTeacherWebview(url: string) {
    const bounds = await webviewBounds();
    await invoke<void>("create_teacher_home_webview", {
      options: {
        url,
        x: 0,
        y: SHELL_HEIGHT,
        width: bounds.width,
        height: bounds.height,
      },
    });
    const webview = await Webview.getByLabel(TEACHER_WEBVIEW_LABEL);
    if (!webview) throw new Error("teacher_webview_create_failed");
    return webview;
  }

  async function ensureTeacherWebview() {
    if (teacherWebview) return teacherWebview;
    if (creating) return creating;
    creating = (async () => {
      const bridge = await invoke<BridgeResult>("prepare_teacher_home_bridge");
      if (bridge?.ok === false) throw new Error(bridge.error || "teacher_home_bridge_failed");
      setConnectionStatus(bridge);
      const view = await createTeacherWebview(buildTeacherHomeUrl(bridge));
      teacherWebview = view;
      return view;
    })();
    try {
      return await creating;
    } finally {
      creating = null;
    }
  }

  function renderMode(next: ShellMode) {
    mode = next;
    const teacher = next === "teacher";
    document.body.classList.toggle("is-teacher-home", teacher);
    teacherButton.classList.toggle("is-active", teacher);
    localButton.classList.toggle("is-active", !teacher);
    teacherButton.setAttribute("aria-pressed", String(teacher));
    localButton.setAttribute("aria-pressed", String(!teacher));
    reloadButton.hidden = !teacher;
    store.inert = teacher;
    store.setAttribute("aria-hidden", String(teacher));
  }

  async function selectMode(next: ShellMode) {
    renderMode(next);
    localStorage.setItem("classaimateDesktopShellMode:v1", next);
    if (next === "local") {
      fallback.hidden = true;
      await teacherWebview?.hide();
      return;
    }
    setFallback("교사 홈을 여는 중입니다.", "현재 교사 웹 화면과 안전하게 연결하고 있습니다.");
    try {
      const view = await ensureTeacherWebview();
      if (mode !== "teacher" || tutorial.open) return;
      await sizeTeacherWebview();
      await view.show();
      await view.setFocus();
      fallback.hidden = true;
    } catch (error) {
      setFallback("교사 홈을 열지 못했습니다.", desktopShellErrorMessage(error), true);
    }
  }

  async function reloadTeacherHome() {
    reloadButton.disabled = true;
    try {
      if (teacherWebview) await teacherWebview.close();
      teacherWebview = null;
      await selectMode("teacher");
    } finally {
      reloadButton.disabled = false;
    }
  }

  const tutorialSteps = [
    {
      target: teacherButton,
      text: "교사 홈은 현재 사용 중인 웹 화면을 그대로 엽니다. TV 현황판·발표 화면처럼 독립 실행이 필요한 기능은 로그인 상태를 유지한 별도 앱 창으로 열립니다.",
    },
    {
      target: localButton,
      text: "로컬 자료함은 같은 SQLite를 수업자료·업무자료·학생별 보기·빠른 관찰로 나눠 쓰는 공간입니다. 일반 바로가기는 현재 화면을, 빠른 관찰기록 바로가기는 이 앱의 관찰 화면을 곧바로 엽니다. 데이터나 백업이 복제되지는 않습니다.",
    },
  ];

  function renderTutorial() {
    tutorialSteps.forEach((step, index) => step.target.classList.toggle("desktop-shell-tutorial-target", index === tutorialIndex));
    tutorialBody.textContent = tutorialSteps[tutorialIndex].text;
    tutorialStep.textContent = `${tutorialIndex + 1} / ${tutorialSteps.length}`;
    tutorialPrevious.disabled = tutorialIndex === 0;
    tutorialNext.textContent = tutorialIndex === tutorialSteps.length - 1 ? "안내 완료" : "다음";
  }

  async function openTutorial(firstRun = false) {
    tutorialReturnMode = mode;
    tutorialFirstRun = firstRun;
    tutorialIndex = 0;
    await teacherWebview?.hide();
    fallback.hidden = true;
    renderTutorial();
    tutorial.showModal();
  }

  async function closeTutorial() {
    tutorialSteps.forEach((step) => step.target.classList.remove("desktop-shell-tutorial-target"));
    localStorage.setItem(TUTORIAL_KEY, "complete");
    tutorial.close();
    await selectMode(tutorialFirstRun ? "teacher" : tutorialReturnMode);
  }

  teacherButton.addEventListener("click", () => void selectMode("teacher"));
  localButton.addEventListener("click", () => void selectMode("local"));
  reloadButton.addEventListener("click", () => void reloadTeacherHome());
  retryButton.addEventListener("click", () => void reloadTeacherHome());
  helpButton.addEventListener("click", () => void openTutorial(false));
  tutorialPrevious.addEventListener("click", () => {
    tutorialIndex = Math.max(0, tutorialIndex - 1);
    renderTutorial();
  });
  tutorialNext.addEventListener("click", () => {
    if (tutorialIndex < tutorialSteps.length - 1) {
      tutorialIndex += 1;
      renderTutorial();
      return;
    }
    void closeTutorial();
  });
  void appWindow.onResized(() => { void sizeTeacherWebview(); });

  const refreshConnection = async () => {
    const connection = await invoke<ConnectionResult>("get_device_connection_status");
    setConnectionStatus(connection);
  };

  const startActivationHandling = async (handler: (intent: DesktopActivationIntent) => void | Promise<void>) => {
    const consume = async (intent: DesktopActivationIntent | null | undefined) => {
      if (intent !== "quick-observation") return;
      if (tutorial.open) {
        tutorialSteps.forEach((step) => step.target.classList.remove("desktop-shell-tutorial-target"));
        tutorial.close();
      }
      await selectMode("local");
      await handler(intent);
    };
    await listen<DesktopActivationIntent>("desktop-activation", (event) => {
      void invoke<DesktopActivationIntent | null>("take_desktop_activation_intent")
        .catch(() => null)
        .then(() => consume(event.payload));
    });
    const pending = await invoke<DesktopActivationIntent | null>("take_desktop_activation_intent");
    await consume(pending);
  };

  renderMode("local");
  void refreshConnection();
  if (localStorage.getItem(TUTORIAL_KEY) !== "complete") {
    void openTutorial(true);
  } else {
    void selectMode("teacher");
  }
  return { refreshConnection, startActivationHandling };
}
