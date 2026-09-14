import { initDataExplorer } from "./data-explorer";
import { initStudentTimeline } from "./student-timeline";
import { initRecordDuplicates } from "./record-duplicates";

export function initRecordBrowsers(getTenantId: () => string) {
  const dataExplorer = initDataExplorer({ getTenantId });
  const studentTimeline = initStudentTimeline({ getTenantId });
  initRecordDuplicates({
    getTenantId,
    getSelectedStudent: studentTimeline.getSelectedStudent,
    onChanged: () => Promise.all([dataExplorer.refresh(), studentTimeline.refresh()]),
  });
  return { dataExplorer, studentTimeline };
}
