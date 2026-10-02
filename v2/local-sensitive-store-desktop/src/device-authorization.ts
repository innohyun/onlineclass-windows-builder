import { invoke } from "@tauri-apps/api/core";
import { authorizationStepStates } from "./device-sync-presentation";
import { isDeskRestoreBlocked } from "./desk-restore-lock";

export type DeviceAuthorizationResult = {
  ok: boolean;
  status?: "idle" | "pending" | "approved" | "connected" | "expired" | "canceled" | "consumed" | "device_sync_failed";
  requestId?: string;
  expiresAtMs?: number;
  tenantId?: string;
  tenantName?: string;
  accountEmail?: string;
  accountDisplayName?: string;
  deviceSyncConnected?: boolean;
  deviceSyncError?: string;
  error?: string;
};

type Options = {
  setText(id: string, text: string): void;
  setActionBusy(busy: boolean): void;
  onConnected(): Promise<void>;
  onStartFailure(message: string): void;
  showSettings(): void;
  onStateChange?(result: DeviceAuthorizationResult): void;
};

function element<T extends HTMLElement>(id: string) {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element: ${id}`);
  return found as T;
}

export function deviceAuthorizationErrorMessage(code: string | undefined) {
  if (code === "local_store_service_unavailable" || code === "local_store_unavailable") {
    return "이 PC 저장 서비스가 시작되지 않았습니다. 이미 실행 중인 앱이 있으면 트레이 아이콘에서 열고, 없으면 앱을 완전히 종료한 뒤 다시 실행해 주세요.";
  }
  return "인터넷 연결과 앱 실행 상태를 확인한 뒤 다시 시도해 주세요.";
}

export function createDeviceAuthorizationController(options: Options) {
  let pollTimer = 0;
  let requestRevision = 0;
  let starting = false;

  function mountSteps() {
    const panel = element<HTMLElement>("deviceAuthPanel");
    if (document.getElementById("deviceAuthSteps")) return;
    const steps = document.createElement("ol"); steps.id = "deviceAuthSteps"; steps.className = "device-auth-steps";
    steps.setAttribute("aria-label", "새 PC 연결 순서");
    for (const [index, label] of ["교사 로그인", "연결 승인", "이 PC 연결 확인", "자료 복원 · 선택"].entries()) {
      const row = document.createElement("li"); row.dataset.deviceAuthStep = ["login", "approval", "connection", "restore"][index];
      row.dataset.state = index === 3 ? "optional" : "neutral";
      row.innerHTML = `<span aria-hidden="true">${index + 1}</span><strong>${label}</strong><small>${index === 3 ? "별도 선택" : "확인 전"}</small>`; steps.append(row);
    }
    element("deviceAuthDescription").after(steps);
    const complete = document.createElement("div"); complete.id = "deviceAuthCompleteActions"; complete.className = "device-auth-complete-actions"; complete.hidden = true;
    complete.innerHTML = '<p>기기 연결 완료는 자료 복원 완료를 뜻하지 않습니다. 기존 자료는 유지됩니다.</p><button class="primary-action" data-app-view-target="backup" type="button">백업에서 자료 복원</button><button class="secondary-action" id="deviceAuthContinue" type="button">연결된 설정 보기</button>';
    panel.append(complete);
    element("deviceAuthContinue").addEventListener("click", () => { panel.hidden = true; panel.dataset.completeDismissed = "true"; element("settingsConnectedContent").hidden = false; });
    complete.querySelector("[data-app-view-target]")?.addEventListener("click", () => window.setTimeout(() => {
      const target = document.getElementById("backupRestorePanel");
      if (target && !target.closest<HTMLElement>("[data-app-view]")?.hidden) target.scrollIntoView({ block: "start" });
    }, 0));
    window.addEventListener("desk:restore-lock-changed", () => {
      for (const id of ["deviceAuthStart", "deviceAuthReopen"]) element<HTMLButtonElement>(id).disabled = isDeskRestoreBlocked();
    });
  }

  function render(result: DeviceAuthorizationResult) {
    mountSteps();
    const panel = element<HTMLElement>("deviceAuthPanel");
    const waiting = result.status === "pending" || result.status === "approved";
    panel.hidden = false;
    if (result.status !== "connected") {
      element<HTMLElement>("settingsConnectedContent").hidden = true;
    } else {
      element<HTMLElement>("settingsConnectedContent").hidden = false;
    }
    panel.dataset.state = result.status || (result.ok ? "idle" : "error");
    delete panel.dataset.completeDismissed;
    const states = authorizationStepStates(result);
    element("deviceAuthSteps").querySelectorAll<HTMLElement>("li").forEach((row, index) => {
      row.dataset.state = states[index];
      row.querySelector("small")!.textContent = states[index] === "complete" ? "확인됨" : states[index] === "current" ? "진행 중" : states[index] === "error" ? "확인 필요" : states[index] === "optional" ? "별도 선택" : "확인 전";
    });
    element("deviceAuthCompleteActions").hidden = result.status !== "connected";
    element<HTMLElement>("deviceAuthWait").hidden = !waiting;
    element<HTMLButtonElement>("deviceAuthStart").hidden = waiting || result.status === "connected";
    element<HTMLButtonElement>("deviceAuthReopen").hidden = !waiting;
    element<HTMLButtonElement>("deviceAuthStart").textContent = result.status === "device_sync_failed"
      ? "새 요청으로 다시 연결"
      : result.status === "expired" ? "새 승인 요청 시작" : "브라우저에서 교사 로그인";
    element<HTMLButtonElement>("deviceAuthStart").disabled = isDeskRestoreBlocked();
    element<HTMLButtonElement>("deviceAuthReopen").disabled = isDeskRestoreBlocked();
    if (waiting) {
      options.setText("deviceAuthTitle", "브라우저 승인 대기 중");
      options.setText("deviceAuthDescription", "열린 웹페이지에서 교사 로그인 후 이 PC 연결을 승인하세요.");
      options.setText("deviceAuthMeta", result.status === "approved" ? "승인을 확인했습니다. 안전한 연결을 마무리하는 중입니다." : "승인 요청은 10분 뒤 자동으로 만료됩니다.");
    } else if (result.status === "connected") {
      options.setText("deviceAuthTitle", "이 PC 연결 완료");
      options.setText("deviceAuthDescription", `${result.tenantName || result.tenantId || "선택한 학급"}이 이 로컬 저장소에 연결되었습니다.`);
      options.setText("deviceAuthMeta", result.accountEmail || result.accountDisplayName || "교사 계정으로 승인됨");
    } else if (result.status === "device_sync_failed") {
      options.setText("deviceAuthTitle", "기기 동기화 연결을 완료하지 못했습니다");
      options.setText("deviceAuthDescription", "브라우저 승인은 확인했지만 동기화 연결 정보를 안전하게 저장하지 못했습니다. 다시 연결해 주세요.");
      options.setText("deviceAuthMeta", "다시 연결하기 전까지 기존 로컬 자료와 수동 백업은 그대로 유지됩니다.");
    } else if (result.status === "expired" || result.status === "canceled" || result.status === "consumed") {
      options.setText("deviceAuthTitle", result.status === "expired" ? "승인 요청이 만료되었습니다" : "승인 요청이 종료되었습니다");
      options.setText("deviceAuthDescription", "새 요청을 열어 교사 로그인으로 다시 연결하세요.");
      options.setText("deviceAuthMeta", "기존 로컬 자료는 유지됩니다. 승인 요청은 10분 뒤 만료됩니다.");
    } else if (!result.ok) {
      options.setText("deviceAuthTitle", "브라우저 연결을 시작하지 못했습니다");
      options.setText("deviceAuthDescription", deviceAuthorizationErrorMessage(result.error));
      options.setText("deviceAuthMeta", "로컬 자료는 그대로 유지됩니다.");
    } else {
      options.setText("deviceAuthTitle", "새 PC 연결");
      options.setText("deviceAuthDescription", "브라우저에서 교사 로그인 후 이 PC 연결을 승인하세요.");
      options.setText("deviceAuthMeta", "승인 요청은 10분 뒤 만료됩니다. 기존 자료는 그대로 유지됩니다.");
    }
    options.onStateChange?.(result);
  }

  async function poll(revision = requestRevision) {
    if (revision !== requestRevision) return;
    window.clearTimeout(pollTimer);
    if (isDeskRestoreBlocked()) { pollTimer = window.setTimeout(() => void poll(revision), 1_200); return; }
    try {
      const result = await invoke<DeviceAuthorizationResult>("poll_device_authorization");
      if (revision !== requestRevision) return;
      render(result);
      if (result.status === "pending" || result.status === "approved") {
        pollTimer = window.setTimeout(() => void poll(revision), 1_200);
      } else if (result.status === "connected") {
        await options.onConnected();
        if (revision === requestRevision) render(result);
      }
    } catch (error) {
      if (revision === requestRevision) render({ ok: false, error: String((error as Error)?.message || error) });
    }
  }

  async function start() {
    if (isDeskRestoreBlocked()) { options.onStartFailure("복원 작업을 마칠 때까지 새 PC 연결을 시작할 수 없습니다."); return; }
    if (starting) return;
    starting = true;
    const revision = ++requestRevision;
    window.clearTimeout(pollTimer);
    options.showSettings();
    options.setActionBusy(true);
    element<HTMLButtonElement>("deviceAuthStart").disabled = true;
    try {
      const result = await invoke<DeviceAuthorizationResult>("start_device_authorization");
      if (revision !== requestRevision) return;
      render(result);
      if (!result?.ok) options.onStartFailure(deviceAuthorizationErrorMessage(result?.error));
      else void poll(revision);
    } catch (error) {
      render({ ok: false, error: String((error as Error)?.message || error) });
    } finally {
      starting = false;
      options.setActionBusy(false);
      element<HTMLButtonElement>("deviceAuthStart").disabled = isDeskRestoreBlocked();
    }
  }

  async function reopen() {
    if (isDeskRestoreBlocked()) return;
    const result = await invoke<{ ok: boolean; error?: string }>("reopen_device_authorization")
      .catch((error) => ({ ok: false, error: String(error) }));
    if (!result.ok) options.setText("deviceAuthMeta", "승인 페이지를 다시 열지 못했습니다. 인터넷과 브라우저 상태를 확인해 주세요. 기존 승인 요청은 유지됩니다.");
  }

  mountSteps();
  return Object.freeze({ render, start, reopen });
}
