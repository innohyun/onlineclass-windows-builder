export const documentTemplates: Record<string, {title:string; markdown:string}> = {
  'lesson-plan': {title:'새 수업계획', markdown:'# 수업 개요\n\n- 대상: \n- 주제: \n- 수업 시간: \n\n## 학습 목표\n\n배움을 마친 뒤 학생이 할 수 있는 일을 작성하세요.\n\n## 수업 흐름\n\n| 단계 | 시간 | 교사와 학생 활동 | 자료와 유의점 |\n| --- | --- | --- | --- |\n| 도입 |  |  |  |\n| 전개 |  |  |  |\n| 정리 |  |  |  |\n\n## 발문과 예상 반응\n\n## 평가와 피드백\n\n## 수업 자료\n\n## 수업 후 성찰'},
  'meeting-note': {title:'새 회의록', markdown:'# 회의 개요\n\n- 일시: \n- 참석자: \n- 안건: \n\n## 논의 내용\n\n## 결정 사항\n\n## 후속 업무\n\n- [ ] 담당자와 기한을 입력하세요.'},
  'class-note': {title:'새 학급 운영 노트', markdown:'# 학급 운영\n\n## 오늘의 계획\n\n- [ ] 할 일\n\n## 준비 사항\n\n## 운영 메모\n\n## 다음에 이어 할 일'},
};
