export function workNoteDateValue(label) {
  const date = new Date();
  if (label === '내일') date.setDate(date.getDate() + 1);
  if (label === '다음 주') date.setDate(date.getDate() + 7);
  return new Intl.DateTimeFormat('ko-KR', {
    year: 'numeric', month: 'long', day: 'numeric', weekday: 'short',
  }).format(date);
}

export function workNoteInlineOptions({ mode, query, pages, pageId, pagePath }) {
  const normalized = query.trim().toLowerCase();
  const found = [...pages].filter((page) => page.pageId !== pageId
    && (!normalized || page.title.toLowerCase().includes(normalized))).slice(0, 7);
  const items = [];
  if (mode === 'at') {
    for (const label of ['오늘', '내일', '다음 주']) if (!normalized || label.includes(normalized)) {
      items.push({ group: '날짜', icon: 'fa-calendar-day', label, copy: workNoteDateValue(label), action: 'date', value: label });
    }
  }
  if (mode === 'plus') items.push({ group: '페이지 만들기', icon: 'fa-file-circle-plus', label: `${query || '제목 없음'} 같은 폴더의 노트 만들기`, copy: '현재 노트와 같은 폴더에 만듭니다.', action: 'child' });
  found.forEach((page) => items.push({ group: '페이지', icon: 'fa-file-lines', label: page.title, copy: pagePath(page), action: 'link', page }));
  if (mode === 'brackets') items.push({ group: '페이지 만들기', icon: 'fa-file-circle-plus', label: `${query || '제목 없음'} 새 같은 폴더의 노트`, copy: '만든 뒤 현재 문서에 연결합니다.', action: 'child' });
  if (mode === 'plus') items.push({ group: '페이지 만들기', icon: 'fa-folder-plus', label: '최상위 페이지로 만들기', copy: '현재 계층 밖에 새 페이지를 만듭니다.', action: 'root' });
  return items;
}

export function workNoteMentionOptions(result) {
  return (result?.candidates || []).slice(0, 20).map((candidate) => ({
    group: '사용자', icon: 'fa-user', label: candidate.displayName,
    copy: candidate.classLabel, action: 'user', candidate,
  }));
}

export function workNoteUserMentionContent(candidate) {
  return {
    type: 'userMention',
    attrs: {
      mentionId: globalThis.crypto?.randomUUID?.() || `mention_${Date.now()}_${Math.random().toString(36).slice(2)}`,
      recipientId: candidate.userId,
      label: candidate.displayName,
    },
  };
}

export function focusWorkNoteMention(element, mentionId) {
  const target = [...(element?.querySelectorAll?.('[data-work-note-user-mention]') || [])]
    .find((node) => node.getAttribute('data-work-note-user-mention') === mentionId);
  if (!target) return false;
  target.scrollIntoView({ block: 'center', behavior: 'smooth' });
  target.classList.add('is-notification-target');
  setTimeout(() => target.classList.remove('is-notification-target'), 5000);
  return true;
}
