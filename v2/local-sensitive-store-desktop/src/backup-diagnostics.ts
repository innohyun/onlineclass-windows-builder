import type { BackupStorageOverview, SnapshotPolicy } from './backup-types';

const time = (value?: number | null) => value ? new Date(value).toLocaleString('ko-KR') : '확인 전';
const bytes = (value?: number, complete = true) => typeof value === 'number'
  ? `${(value / 1024 ** 2).toFixed(1)} MB${complete ? '' : ' 이상 · 일부 집계'}` : '용량 확인 필요';
export function snapshotPolicyDescription(policy?: SnapshotPolicy | null) {
  if (!policy) return '서버 쓰기 정책 조회 전';
  const version = policy.maxWritableSnapshotVersion;
  const reason = policy.reason === 'snapshot_policy_missing' ? '서버 정책 누락으로 v4 기본값 사용'
    : version === 4 ? `active 기기 ${policy.blockingDeviceCount ?? '?'}개의 형식 호환성 유지` : '연결 기기 형식 호환 확인';
  const devices = policy.blockingDevices?.map(device => `${device.deviceName || '이름 미확인'} (v${device.snapshotFormatMax ?? '?'}, 마지막 접속 ${time(device.lastSeenAt)})`).join(' · ');
  return `서버 쓰기 정책 v${version ?? '?'} · ${reason} · 조회 ${time(policy.checkedAtMs)}${devices ? `\n${devices}` : ''}${version === 4 ? '\n해당 기기를 업데이트하거나, 더 이상 사용하지 않는 기기는 교사 설정에서 확인 후 연결을 해제해 주세요.' : ''}`;
}

const reasons: Record<string,string> = {
  recent:'최근 보관',daily:'일별 보관',monthly:'월별 보관',manual:'수동 보관',pre_restore:'복원 전 보호',
  latest_recoverable_format:'해당 형식의 최신 복원점',protected_generation:'서버·게시 대기·현재 적용 세대 보호',outside_retention:'공통 보관 한도 밖',
  staging_owner_unconfirmed:'생성·동기화 작업 종료 여부 미확인',legacy_or_incomplete_manifest:'구조 또는 manifest 확인 필요',
  snapshot_incomplete:'백업 불완전',creation_time_unconfirmed:'생성 시각 확인 필요',unknown_backup_purpose:'백업 목적 확인 필요',
};
export function renderBackupDiagnostics(panel: HTMLElement, storage: BackupStorageOverview | null) {
  let details = panel.querySelector<HTMLDetailsElement>('#backupDiagnostics');
  if (!details) { details = document.createElement('details'); details.id='backupDiagnostics'; panel.append(details); }
  details.replaceChildren();
  const summary = document.createElement('summary'); summary.textContent='백업 형식·자동 정리·정리 미리보기'; details.append(summary);
  const line = (value: string) => { const node=document.createElement('p'); node.textContent=value; node.style.whiteSpace='pre-wrap'; details!.append(node); };
  if (!storage?.ok) { line('진단 정보를 읽지 못했습니다.'); return; }
  line(`지원 v${storage.supportedSnapshotVersion ?? '?'} · 최근 생성 v${storage.latestBackupVersion ?? '?'}\n${snapshotPolicyDescription(storage.snapshotPolicy)}`);
  const maintenance=storage.maintenance;
  line(`자동 정리: ${maintenance?.running ? '진행 중 또는 중단 후 재시도 대기' : maintenance?.ok === true ? '성공' : maintenance?.ok === false ? '일부 실패·보류' : '실행 전'}\n마지막 시도 ${time(maintenance?.lastAttemptAtMs)} · 마지막 성공 ${time(maintenance?.lastSuccessAtMs)} · 다음 실행·재시도 ${time(maintenance?.nextRetryAtMs)}${maintenance?.deferredReason ? `\n보류: ${maintenance.deferredReason}` : ''}`);
  const stages: Record<string,string>={safety:'공통 안전 조건',cache:'복구 캐시',manualRetention:'수동 복원점',snapshots:'자동 복원점',objects:'첨부 객체',legacyQuarantine:'이전 백업 격리',legacyPurge:'만료 격리 삭제'};
  for (const [key,result] of Object.entries(maintenance?.stages || {})) {
    line(`${stages[key] || key}: ${result.ok ? '성공' : '실패·보류'}${result.error ? ` · ${result.error}` : ''} · 격리 ${result.quarantined ?? (result.ok ? 0 : "확인 필요")}개 ${bytes(result.quarantinedBytes ?? (result.ok ? 0 : undefined))} · 실제 삭제 ${result.deleted ?? result.purged ?? (result.ok ? 0 : "확인 필요")}개 ${bytes(result.deletedBytes ?? result.purgedBytes ?? (result.ok ? 0 : undefined))}${result.reviewCount ? ` · 확인 필요 ${result.reviewCount}개` : ''}`);
  }
  if (storage.scanErrors?.length) line(`용량 조회 오류:\n${storage.scanErrors.join('\n')}`);
  line('정리 미리보기는 조회만 수행합니다. 한도 밖 후보는 실행 시 DB·첨부 무결성, 보호 세대와 대체 복원점을 다시 검증합니다. 격리 이동은 실제 확보 용량에 포함하지 않습니다.');
  if (storage.cleanupPreview?.error) line(`미리보기 조회 실패: ${storage.cleanupPreview.error}`);
  const list=document.createElement('ul'); details.append(list);
  for (const item of storage.cleanupPreview?.items || []) {
    const node=document.createElement('li');
    const action=item.action==='keep' ? '보존' : item.action==='review' ? '확인 필요' : item.plannedAction==='quarantine' ? '검증 후 격리 예정' : '검증 후 삭제 예정';
    node.textContent=`${action} · v${item.version ?? '?'} · ${item.kind || '유형 미확인'} · 세대 ${item.generation ?? '-'} · ${time(item.createdAtMs)} · ${item.deviceName || '기기 미확인'} · ${bytes(item.bytes,item.bytesComplete===true)} · ${(item.reason || '').split(',').map(reason=>reasons[reason] || reason).join(', ')} · ${item.verification==='pending' ? '무결성 검증 전' : item.verification || '검증 미확인'}`;
    node.title=item.manifestPath || ''; list.append(node);
  }
  for (const item of storage.legacyQuarantineItems || []) line(`격리: ${item.snapshotName || '경로 확인 필요'} · ${item.status} · ${bytes(item.bytes)} · 만료 ${time(item.purgeAfterMs)} · ${item.action==='verify_before_delete' ? '안전 검증 후 삭제 예정' : item.reason || '유예 기간 보존'}`);
  for (const item of storage.stagingEntries || []) line(`임시 작업: ${item.relativePath} · ${bytes(item.bytes,storage.scanComplete===true)} · ${item.owner?.pcName || '생성 기기 미확인'} · ${item.state === "interrupted_local" ? "이 PC에서 중단된 작업 잔여물 · 확인 후 정리" : "다른 PC 생성·동기화 가능 · 활동 종료 확인 전 삭제 보류"}`);
  for (const item of storage.otherEntries || []) line(`기타: ${item.relativePath} · ${item.type} · ${bytes(item.bytes)} · 확인 필요, 자동 삭제 제외`);
}
