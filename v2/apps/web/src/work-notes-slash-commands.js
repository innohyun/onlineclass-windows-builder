export const workNoteCommandGroups = [
  ["AI", [["ai", "AI로 작성·편집", "ai, 인공지능, 작성, 편집", "선택한 글이나 현재 블록을 AI로 다듬습니다.", "fa-wand-magic-sparkles"]]],
  ["기본 블록", [
    ["text", "일반 텍스트", "text, 텍스트, 일반", "문장을 작성합니다.", "fa-font"],
    ["page", "페이지", "page, 페이지, 새페이지", "현재 문서 아래에 하위 페이지를 만듭니다.", "fa-file-circle-plus"],
    ["todo", "할 일", "todo, 할일", "체크할 수 있는 할 일을 만듭니다.", "fa-square-check"],
    ["bullet", "글머리 기호", "bullet, 글머리", "글머리 기호 목록입니다.", "fa-list-ul"],
    ["number", "번호 목록", "number, 번호", "순서가 있는 목록입니다.", "fa-list-ol"],
    ["toggle", "토글", "toggle, 토글", "안에 여러 블록을 넣고 접습니다.", "fa-caret-right"],
    ["h1", "제목 1", "h1, 제목1, #", "큰 제목입니다.", "fa-heading"],
    ["h2", "제목 2", "h2, 제목2, ##", "중간 제목입니다.", "fa-heading"],
    ["h3", "제목 3", "h3, 제목3, ###", "작은 제목입니다.", "fa-heading"],
    ["quote", "인용", "quote, 인용", "인용문을 강조합니다.", "fa-quote-left"],
    ["callout", "콜아웃", "callout, 콜아웃", "여러 블록을 담는 강조 상자입니다.", "fa-lightbulb"],
    ["divider", "구분선", "divider, 구분선", "내용 사이에 선을 넣습니다.", "fa-minus"],
  ]],
  ["고급 블록", [
    ["table", "표", "table, 표", "행과 열을 바로 편집하는 표입니다.", "fa-table-cells"],
    ["code", "코드", "code, 코드", "고정폭 코드 블록입니다.", "fa-code"],
    ["pageLink", "페이지 링크", "link, 페이지 링크", "다른 페이지를 본문에 연결합니다.", "fa-link"],
    ["toc", "목차", "toc, 목차", "현재 페이지 제목을 바탕으로 목차를 만듭니다.", "fa-list"],
  ]],
  ["미디어", [
    ["image", "이미지", "image, 이미지, 사진", "내 PC에 원본 이미지를 저장합니다.", "fa-image"],
    ["file", "파일", "file, 파일, 첨부", "용량 제한 없이 로컬 파일을 첨부합니다.", "fa-paperclip"],
    ["pdf", "PDF", "pdf, 문서", "PDF를 첨부하고 본문에서 미리 봅니다.", "fa-file-pdf"],
    ["video", "동영상", "video, 동영상, 영상", "로컬 동영상 플레이어를 넣습니다.", "fa-film"],
    ["audio", "오디오", "audio, 오디오, 음성", "로컬 오디오 플레이어를 넣습니다.", "fa-file-audio"],
  ]],
];

export const flattenWorkNoteCommands = () => workNoteCommandGroups
  .flatMap(([group, commands]) => commands.map((command) => ({ group, command })));
