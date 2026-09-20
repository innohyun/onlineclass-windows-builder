const PDF_MODULE_URL = "/work-note-vendor/pdf.min.mjs";
const PDF_WORKER_URL = "/work-note-vendor/pdf.worker.min.mjs";
let pdfModulePromise = null;

async function pdfModule() {
  pdfModulePromise ||= import(PDF_MODULE_URL).then((module) => {
    module.GlobalWorkerOptions.workerSrc = PDF_WORKER_URL;
    return module;
  });
  return pdfModulePromise;
}

export async function renderWorkNotePdf(container, blob, { fileName = "PDF" } = {}) {
  const pdfjs = await pdfModule();
  const loadingTask = pdfjs.getDocument({
    data: new Uint8Array(await blob.arrayBuffer()),
    enableScripting: false,
    enableXfa: false,
    isEvalSupported: false,
  });
  const pdf = await loadingTask.promise;
  const pages = globalThis.document.createDocumentFragment();
  let cancelled = false;
  for (let pageNumber = 1; pageNumber <= pdf.numPages; pageNumber += 1) {
    if (cancelled) break;
    const page = await pdf.getPage(pageNumber);
    const natural = page.getViewport({ scale: 1 });
    const available = Math.max(280, container.clientWidth || 760);
    const viewport = page.getViewport({ scale: Math.min(2, available / natural.width) });
    const canvas = globalThis.document.createElement("canvas");
    canvas.width = Math.ceil(viewport.width);
    canvas.height = Math.ceil(viewport.height);
    canvas.setAttribute("aria-label", `${fileName} ${pageNumber}쪽`);
    await page.render({ canvasContext: canvas.getContext("2d", { alpha: false }), viewport }).promise;
    pages.append(canvas);
    page.cleanup();
  }
  if (!cancelled) container.replaceChildren(pages);
  return {
    async destroy() {
      cancelled = true;
      await loadingTask.destroy().catch(() => {});
    },
  };
}
