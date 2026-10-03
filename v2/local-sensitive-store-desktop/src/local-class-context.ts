export type LocalClass = {
  tenantId: string;
  uid?: string;
  deviceId?: string;
  tenantName?: string;
  schoolName?: string;
  academicYear?: number;
  grade?: number | string;
  classNumber?: number | string;
  lifecycleStatus?: string;
};

export type LocalClassRequest = { tenantId: string; revision: number };
let tenantId = "";
let revision = 0;

export function currentLocalClass() { return tenantId; }
export function captureLocalClassRequest(): LocalClassRequest { return { tenantId, revision }; }
export function isCurrentLocalClassRequest(request: LocalClassRequest) { return request.tenantId === tenantId && request.revision === revision; }
export function setCurrentLocalClass(next: string) {
  if (next === tenantId) return false;
  tenantId = next;
  revision += 1;
  return true;
}

/** Display only metadata returned by the authorized class session. */
export function localClassLabel(value: LocalClass) {
  const year = Number.isSafeInteger(value.academicYear) && Number(value.academicYear) > 0 ? `${value.academicYear}학년도` : "";
  const group = [value.grade ? `${value.grade}학년` : "", value.classNumber ? `${value.classNumber}반` : ""].filter(Boolean).join(" ");
  const name = [value.schoolName, year, group].filter(Boolean).join(" · ");
  return name || [year, value.tenantName || value.tenantId].filter(Boolean).join(" · ");
}

export function localClassLifecycleLabel(value: LocalClass) {
  return value.lifecycleStatus === "preparing" ? "준비 중" : value.lifecycleStatus === "archived" ? "지난 학급" : value.lifecycleStatus === "active" || value.lifecycleStatus === "opened" ? "운영 중" : "";
}
