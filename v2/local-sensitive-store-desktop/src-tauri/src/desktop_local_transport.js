(() => {
  if (window.location.origin !== "https://t.classaimate.com"
    || !/^\/admin(?:\/|$)/.test(window.location.pathname)) return;
  const originalFetch = window.fetch.bind(window);
  const ports = new Set([51273, 51274, 51275, 51276, 51277]);
  const encode = (bytes) => {
    let text = "";
    for (let i = 0; i < bytes.length; i += 8192) text += String.fromCharCode(...bytes.subarray(i, i + 8192));
    return btoa(text);
  };
  window.fetch = async (input, init) => {
    const url = new URL(typeof input === "string" || input instanceof URL ? input : input.url, window.location.href);
    if (url.protocol !== "http:" || url.hostname !== "127.0.0.1" || !ports.has(Number(url.port)) || !url.pathname.startsWith("/v1/")) {
      return originalFetch(input, init);
    }
    const nativeInit = init ? { ...init } : undefined;
    if (nativeInit) delete nativeInit.targetAddressSpace;
    const request = new Request(input, nativeInit);
    const abort = () => new DOMException("The operation was aborted.", "AbortError");
    if (request.signal.aborted) throw abort();
    const bytes = new Uint8Array(await request.arrayBuffer());
    if (request.signal.aborted) throw abort();
    const headers = {};
    request.headers.forEach((value, name) => { headers[name] = value; });
    const isWrite = !["GET", "HEAD"].includes(request.method);
    const dispatchedFailure = (cause) => {
      if (!isWrite) return cause;
      const error = new Error("local_store_outcome_unknown", { cause });
      error.code = "local_store_outcome_unknown";
      error.outcomeUnknown = true;
      error.mutationState = "unknown";
      return error;
    };
    return new Promise((resolve, reject) => {
      const onAbort = () => reject(dispatchedFailure(abort()));
      request.signal.addEventListener("abort", onAbort, { once: true });
      window.__TAURI_INTERNALS__.invoke("teacher_local_request", {
        request: { url: url.href, method: request.method, headers, bodyBase64: bytes.length ? encode(bytes) : null },
      }).then((result) => {
        if (request.signal.aborted) throw abort();
        const body = Uint8Array.from(atob(result.bodyBase64), (character) => character.charCodeAt(0));
        resolve(new Response(request.method === "HEAD" || result.status === 204 || result.status === 304 ? null : body, { status: result.status, headers: result.headers }));
      }).catch((error) => reject(dispatchedFailure(error))).finally(() => request.signal.removeEventListener("abort", onAbort));
    });
  };
})();
