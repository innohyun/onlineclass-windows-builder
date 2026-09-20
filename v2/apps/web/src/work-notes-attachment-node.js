import { Node, mergeAttributes } from "@tiptap/core";
import { renderWorkNotePdf } from "./work-notes-pdf-preview.js";

function sizeLabel(value) {
  const bytes = Number(value || 0);
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

export function attachmentKind(contentType = "", requested = "file") {
  if (contentType.startsWith("image/")) return "image";
  if (contentType === "application/pdf") return "pdf";
  if (contentType.startsWith("video/")) return "video";
  if (contentType.startsWith("audio/")) return "audio";
  return requested === "image" || requested === "pdf" || requested === "video" || requested === "audio" ? requested : "file";
}

export const attachmentSupportsPreview = (kind) => ["image", "pdf", "video", "audio"].includes(kind);
export const normalizeAttachmentDisplayMode = (value, kind) => value === "file" || !attachmentSupportsPreview(kind) ? "file" : "preview";
const SAFE_INLINE_TYPES = new Set(["application/pdf", "image/png", "image/jpeg", "image/gif", "image/webp", "audio/mpeg", "audio/mp4", "audio/ogg", "audio/wav", "video/mp4", "video/webm", "video/ogg"]);
export const attachmentCanOpenInline = (contentType) => SAFE_INLINE_TYPES.has(String(contentType || "").toLowerCase());
const optionEnabled = (value) => typeof value === "function" ? value() === true : value !== false;

export function attachmentNode(record, requestedKind = "file", displayMode = "") {
  const kind = attachmentKind(record.contentType, requestedKind);
  return {
    type: "attachmentBlock",
    attrs: {
      attachmentId: record.attachmentId || "",
      fileName: record.fileName || "첨부파일",
      contentType: record.contentType || "application/octet-stream",
      size: Number(record.size || 0),
      kind,
      displayMode: normalizeAttachmentDisplayMode(displayMode || (kind === "file" ? "file" : "preview"), kind),
      uploadState: record.uploadState || "stored",
      uploadToken: record.uploadToken || "",
      uploadError: record.uploadError || "",
      blockId: record.blockId || record.attachmentId,
    },
  };
}

function icon(kind) {
  return ({ image: "fa-image", pdf: "fa-file-pdf", video: "fa-film", audio: "fa-file-audio" })[kind] || "fa-paperclip";
}

function actionLink(label, iconName, download = false) {
  const link = document.createElement("a");
  link.className = "attachment-action";
  link.target = "_blank";
  link.rel = "noopener";
  link.setAttribute("aria-label", label);
  if (download) link.dataset.download = "true";
  link.innerHTML = `<i class="fa-solid ${iconName}"></i>`;
  return link;
}

export function createAttachmentBlock(options) {
  return Node.create({
    name: "attachmentBlock",
    group: "block",
    atom: true,
    draggable: true,
    selectable: true,
    addAttributes() {
      return {
        attachmentId: { default: "" }, fileName: { default: "첨부파일" }, contentType: { default: "application/octet-stream" },
        size: { default: 0 }, kind: { default: "file" }, displayMode: { default: "" }, uploadState: { default: "stored" },
        uploadToken: { default: "" }, uploadError: { default: "" },
      };
    },
    parseHTML() { return [{ tag: "figure[data-work-note-attachment]" }]; },
    renderHTML({ HTMLAttributes }) {
      return ["figure", mergeAttributes(HTMLAttributes, {
        "data-work-note-attachment": HTMLAttributes.attachmentId,
        "data-attachment-display-mode": normalizeAttachmentDisplayMode(HTMLAttributes.displayMode, HTMLAttributes.kind),
        class: `worknote-attachment attachment-${HTMLAttributes.kind}`,
      }), ["span", { class: "attachment-static" }, HTMLAttributes.fileName]];
    },
    addNodeView() {
      return ({ node, deleteNode, updateAttributes }) => {
        const root = document.createElement("figure");
        root.contentEditable = "false";
        root.dataset.blockId = node.attrs.blockId || node.attrs.attachmentId;
        root.dataset.workNoteAttachment = node.attrs.attachmentId;
        root.dataset.attachmentDisplayMode = normalizeAttachmentDisplayMode(node.attrs.displayMode, node.attrs.kind);
        root.className = `worknote-attachment attachment-${node.attrs.kind}`;
        const preview = document.createElement("div");
        preview.className = "attachment-preview is-loading";
        const copy = document.createElement("div");
        copy.className = "attachment-copy";
        const title = document.createElement("strong");
        title.textContent = node.attrs.fileName;
        const meta = document.createElement("span");
        meta.textContent = `${sizeLabel(node.attrs.size)} · ${options.locationLabel || "내 PC에 저장"}`;
        copy.append(title, meta);
        const actions = document.createElement("div");
        actions.className = "attachment-actions";
        const open = actionLink("첨부파일 열기", "fa-arrow-up-right-from-square");
        const download = actionLink("첨부파일 다운로드", "fa-download", true);
        const mode = document.createElement("select");
        mode.className = "attachment-display-mode";
        mode.setAttribute("aria-label", "첨부 표시 방식");
        mode.innerHTML = '<option value="preview">미리보기</option><option value="file">파일</option>';
        mode.querySelector('[value="preview"]').disabled = !attachmentSupportsPreview(node.attrs.kind);
        mode.value = normalizeAttachmentDisplayMode(node.attrs.displayMode, node.attrs.kind);
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "attachment-action danger";
        remove.dataset.attachmentRemove = "true";
        remove.setAttribute("aria-label", node.attrs.uploadState === "failed" ? "실패한 첨부 취소" : "첨부파일 삭제");
        remove.innerHTML = '<i class="fa-solid fa-trash-can"></i>';
        if (attachmentCanOpenInline(node.attrs.contentType)) actions.append(open);
        actions.append(download);
        if (options.canEdit !== false && node.attrs.uploadState !== "failed") actions.prepend(mode);
        if (options.canRemove !== false || node.attrs.uploadState === "failed") actions.append(remove);
        root.append(preview, copy, actions);
        let objectUrl = "";
        let pdfView = null;
        let destroyed = false;

        const showFileCard = (message = "") => {
          preview.replaceChildren();
          const symbol = document.createElement("i");
          symbol.className = `fa-solid ${icon(node.attrs.kind)}`;
          preview.append(symbol);
          preview.classList.remove("is-loading");
          root.classList.add("attachment-file");
          if (message) meta.textContent = message;
        };
        const fallbackPreview = () => showFileCard("미리보기를 열 수 없어 파일로 표시합니다.");
        if (options.openAttachment && !node.attrs.uploadState) {
          open.textContent = '기본 앱으로 열기';
          open.href = '#';
          actions.append(open);
          open.addEventListener('click', (event) => {
            event.preventDefault();
            Promise.resolve(options.openAttachment(node.attrs.attachmentId)).catch(() => showFileCard('첨부파일을 열지 못했습니다. 저장 위치와 파일 상태를 확인해 주세요.'));
          });
        }
        const setLinks = (url) => {
          open.href = url;
          download.href = url;
          download.download = node.attrs.fileName;
        };
        const showPreview = async (blob, url) => {
          if (normalizeAttachmentDisplayMode(node.attrs.displayMode, node.attrs.kind) === "file") return showFileCard();
          preview.replaceChildren();
          preview.classList.remove("is-loading");
          root.classList.remove("attachment-file");
          if (node.attrs.kind === "pdf") {
            preview.classList.add("attachment-pdf-viewer");
            pdfView = await renderWorkNotePdf(preview, blob, { fileName: node.attrs.fileName });
            return;
          }
          const media = document.createElement(node.attrs.kind === "image" ? "img" : node.attrs.kind);
          if (node.attrs.kind === "image") media.alt = node.attrs.fileName;
          else { media.controls = true; media.preload = "metadata"; }
          media.addEventListener("error", fallbackPreview, { once: true });
          media.src = url;
          preview.append(media);
        };

        if (node.attrs.uploadState === "pending") {
          root.classList.add("attachment-upload-pending");
          showFileCard("업로드 중… 완료되기 전에는 본문에 저장되지 않습니다.");
          open.remove();
          download.remove();
          mode.remove();
          remove.remove();
        } else if (node.attrs.uploadState === "failed") {
          root.classList.add("attachment-upload-failed");
          showFileCard(node.attrs.uploadError || "업로드하지 못했습니다. 다시 시도하거나 취소하세요.");
          open.remove();
          download.remove();
          const retry = document.createElement("button");
          retry.type = "button";
          retry.className = "attachment-retry";
          retry.textContent = "다시 시도";
          retry.addEventListener("click", () => options.retryAttachment?.(node.attrs.uploadToken));
          actions.prepend(retry);
        } else if (options.openAttachment && Number(node.attrs.size) > Number(options.maxPreviewBytes || Infinity)) {
          showFileCard('큰 파일은 기본 앱으로 열어 볼 수 있습니다.');
          download.remove();mode.remove();
        } else {
          options.getAttachmentBlob(node.attrs.attachmentId).then(async (blob) => {
            if (destroyed) return;
            objectUrl = URL.createObjectURL(blob);
            setLinks(objectUrl);
            try { await showPreview(blob, objectUrl); } catch { if (!destroyed) fallbackPreview(); }
          }).catch(() => {
            if (destroyed) return;
            preview.classList.add("is-missing");
            showFileCard("첨부파일을 찾지 못했습니다.");
            open.removeAttribute("href");
            download.removeAttribute("href");
          });
        }
        mode.addEventListener("change", () => {
          if (!optionEnabled(options.canEdit)) return;
          updateAttributes({ displayMode: normalizeAttachmentDisplayMode(mode.value, node.attrs.kind) });
        });
        remove.addEventListener("click", async () => {
          if (!optionEnabled(node.attrs.uploadState === "failed" ? options.canEdit : options.canRemove)) return;
          if (node.attrs.uploadState === "failed") { await options.cancelAttachment?.(node.attrs.uploadToken); return; }
          if (!confirm(`‘${node.attrs.fileName}’ 첨부를 삭제할까요?`)) return;
          await options.removeAttachment(node.attrs.attachmentId);
          deleteNode();
        });
        return {
          dom: root,
          update(updated) { return updated.type.name === "attachmentBlock" && JSON.stringify(updated.attrs) === JSON.stringify(node.attrs); },
          destroy() { destroyed = true; void pdfView?.destroy?.(); if (objectUrl) URL.revokeObjectURL(objectUrl); },
        };
      };
    },
  });
}
