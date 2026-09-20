import { Extension, Node, mergeAttributes } from "@tiptap/core";
import { Plugin } from "@tiptap/pm/state";

import { blockId, indentValue } from "./work-notes-tiptap-serialization.js";
import { workNoteBlockIdentityTransaction } from "./work-notes-nested-blocks.js";

export const PageLinkBlock = Node.create({
  name: "pageLinkBlock",
  group: "block",
  atom: true,
  selectable: true,
  addAttributes() {
    return {
      pageId: { default: "" },
      title: { default: "제목 없음" },
    };
  },
  parseHTML() { return [{ tag: "button[data-work-note-page]" }]; },
  renderHTML({ HTMLAttributes }) {
    return ["button", mergeAttributes(HTMLAttributes, {
      type: "button",
      class: "page-card tiptap-page-card",
      "data-work-note-page": HTMLAttributes.pageId,
      contenteditable: "false",
    }), ["i", { class: "fa-regular fa-file-lines" }], ["strong", {}, HTMLAttributes.title], ["i", { class: "fa-solid fa-chevron-right" }]];
  },
});

export const UserMention = Node.create({
  name: 'userMention',
  group: 'inline',
  inline: true,
  atom: true,
  selectable: false,
  addAttributes() {
    return {
      mentionId: {
        default: '',
        parseHTML: (element) => element.getAttribute('data-work-note-user-mention') || '',
        renderHTML: (attributes) => ({ 'data-work-note-user-mention': attributes.mentionId }),
      },
      recipientId: {
        default: '',
        parseHTML: (element) => element.getAttribute('data-recipient-id') || '',
        renderHTML: (attributes) => ({ 'data-recipient-id': attributes.recipientId }),
      },
      label: {
        default: '',
        parseHTML: (element) => element.getAttribute('data-mention-label') || '',
        renderHTML: (attributes) => ({ 'data-mention-label': attributes.label }),
      },
    };
  },
  parseHTML() { return [{ tag: 'span[data-work-note-user-mention]' }]; },
  renderHTML({ node, HTMLAttributes }) {
    const label = String(node.attrs.label || '').trim();
    return ['span', mergeAttributes(HTMLAttributes, {
      class: 'work-note-user-mention',
      'aria-label': `${label}님 멘션`,
      contenteditable: 'false',
    }), `@${label}`];
  },
});

export const Callout = Node.create({
  name: "callout",
  group: "block",
  content: "block+",
  defining: true,
  addAttributes() {
    return {
      icon: { default: "💡" },
      tone: { default: "purple" },
    };
  },
  parseHTML() { return [{ tag: "aside[data-callout]" }]; },
  renderHTML({ HTMLAttributes }) {
    return ["aside", mergeAttributes(HTMLAttributes, { class: `tiptap-callout tone-${HTMLAttributes.tone}`, "data-callout": "true" }),
      ["span", { class: "callout-icon", contenteditable: "false" }, HTMLAttributes.icon],
      ["div", { class: "callout-content" }, 0]];
  },
});

export const BlockIdentity = Extension.create({
  name: "blockIdentity",
  addGlobalAttributes() {
    return [{
      types: ["paragraph", "heading", "blockquote", "codeBlock", "horizontalRule", "bulletList", "orderedList", "taskList", "listItem", "taskItem", "table", "details", "callout", "pageLinkBlock", "attachmentBlock"],
      attributes: {
        blockId: {
          default: null,
          parseHTML: (element) => element.getAttribute("data-block-id"),
          renderHTML: (attributes) => attributes.blockId ? { "data-block-id": attributes.blockId } : {},
        },
        indent: {
          default: 0,
          parseHTML: (element) => indentValue(element.getAttribute("data-block-indent")),
          renderHTML: (attributes) => indentValue(attributes.indent) ? { "data-block-indent": indentValue(attributes.indent) } : {},
        },
      },
    }];
  },
  addProseMirrorPlugins() {
    return [new Plugin({
      appendTransaction(transactions, _oldState, newState) {
        if (!transactions.some((transaction) => transaction.docChanged)) return null;
        return workNoteBlockIdentityTransaction(newState, blockId);
      },
    })];
  },
});
