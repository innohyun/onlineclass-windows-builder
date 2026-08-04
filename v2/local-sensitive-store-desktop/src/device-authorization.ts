import { invoke } from "@tauri-apps/api/core";

export type DeviceAuthorizationResult = {
  ok: boolean;
  status?: "idle" | "pending" | "approved" | "connected" | "expired" | "canceled" | "consumed";
  requestId?: string;
  expiresAtMs?: number;
  tenantId?: string;
  tenantName?: string;
  accountEmail?: string;
  accountDisplayName?: string;
  error?: string;
};

type Options = {
  setText(id: string, text: string): void;
  setActionBusy(busy: boolean): void;
  onConnected(): Promise<void>;
  onStartFailure(message: string): void;
  showSettings(): void;
};

function element<T extends HTMLElement>(id: string) {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element: ${id}`);
  return found as T;
}

export function createDeviceAuthorizationController(options: Options) {
  let pollTimer = 0;

  function render(result: DeviceAuthorizationResult) {
    const panel = element<HTMLElement>("deviceAuthPanel");
    const waiting = result.status === "pending" || result.status === "approved";
    if (result.status !== "connected") {
      panel.hidden = false;
      element<HTMLElement>("settingsConnectedContent").hidden = true;
    }
    panel.dataset.state = result.status === "connected" ? "connected" : result.ok ? (waiting ? "pending" : "idle") : "error";
    element<HTMLElement>("deviceAuthWait").hidden = !waiting;
    element<HTMLButtonElement>("deviceAuthStart").hidden = waiting || result.status === "connected";
    element<HTMLButtonElement>("deviceAuthReopen").hidden = !waiting;
    if (waiting) {
      options.setText("deviceAuthTitle", "브라우저 승인 대기 중");
      options.setText("deviceAuthDescription", "열린 웹페이지에서 교사 로그인 후 이 PC 연결을 승인하세요.");
      options.setText("deviceAuthMeta", result.status === "approved" ? "승인을 확인했습니다. 안전한 연결을 마무리하는 중입니다." : "승인 요청은 10분 뒤 자동으로 만료됩니다.");
    } else if (result.status === "connected") {
      options.setText("deviceAuthTitle", "이 PC 연결 완료");
      options.setText("deviceAuthDescription", `${result.tenantName || result.tenantId || "선택한 학급"}이 이 로컬 저장소에 연결되었습니다.`);
      options.setText("deviceAuthMeta", result.accountEmail || result.accountDisplayName || "교사 계정으로 승인됨");
    } else if (result.status === "expired" || result.status === "canceled" || result.status === "consumed") {
      options.setText("deviceAuthTitle", result.status === "expired" ? "승인 요청이 만료되었습니다" : "승인 요청이 종료되었습니다");
      options.setText("deviceAuthDescription", "새 요청을 열어 교사 로그인으로 다시 연결하세요.");
      options.setText("deviceAuthMeta", "페어링 키나 수동 코드는 필요하지 않습니다.");
    } else if (!result.ok) {
      options.setText("deviceAuthTitle", "브라우저 연결을 시작하지 못했습니다");
      options.setText("deviceAuthDescription", "인터넷 연결을 확인한 뒤 다시 시도해 주세요.");
      options.setText("deviceAuthMeta", result.error || "device_authorization_failed");
    }
  }

  async function poll() {
    window.clearTimeout(pollTimer);
    try {
      const result = await invoke<DeviceAuthorizationResult>("poll_device_authorization");
      render(result);
      if (result.status === "pending" || result.status === "approved") {
        pollTimer = window.setTimeout(() => void poll(), 1_200);
      } else if (result.status === "connected") {
        await options.onConnected();
      }
    } catch (error) {
      render({ ok: false, error: String((error as Error)?.message || error) });
    }
  }

  async function start() {
    options.showSettings();
    options.setActionBusy(true);
    try {
      const result = await invoke<DeviceAuthorizationResult>("start_device_authorization");
      render(result);
      if (!result?.ok) options.onStartFailure(`브라우저 연결 시작 실패: ${result?.error || "open_failed"}`);
      else void poll();
    } catch (error) {
      render({ ok: false, error: String((error as Error)?.message || error) });
    } finally {
      options.setActionBusy(false);
    }
  }

  async function reopen() {
    const result = await invoke<{ ok: boolean; error?: string }>("reopen_device_authorization")
      .catch((error) => ({ ok: false, error: String(error) }));
    if (!result.ok) render({ ok: false, status: "pending", error: result.error });
  }

  return Object.freeze({ render, start, reopen });
}
