export type BackupSource = {
  service?: string;
  serviceVersion?: string;
  appVersion?: string;
  pcName?: string;
  os?: string;
  arch?: string;
  createdAtMs?: number;
  dbPath?: string;
};

export type BackupItem = {
  ok?: boolean;
  tenantId?: string;
  backupId?: string;
  createdAtMs?: number;
  manifestPath?: string;
  dbPath?: string;
  kind?: string;
  generation?: number;
  source?: BackupSource;
  counts?: Record<string, number>;
  media?: {
    records?: unknown[];
    copied?: number;
    skipped?: number;
    missing?: number;
    failed?: number;
    bytes?: number;
  };
};

export type ManualBackupItem = {
  ok?: boolean;
  backupId?: string;
  createdAtMs?: number;
  manifestPath?: string;
  snapshotBytes?: number;
  bytesComplete?: boolean;
  source?: BackupSource;
};

export type BackupStatus = {
  ok: boolean;
  configured: boolean;
  tenantId?: string;
  backupRootDir?: string;
  tenantBackupDir?: string;
  lastRunAtMs?: number;
  nextRunAtMs?: number;
  latestBackup?: {
    backupId?: string;
    createdAtMs?: number;
    manifestPath?: string;
    source?: BackupSource;
    counts?: Record<string, number>;
    media?: {
      copied?: number;
      skipped?: number;
      missing?: number;
      failed?: number;
      bytes?: number;
    };
  } | null;
  lastResult?: {
    ok?: boolean;
    media?: {
      copied?: number;
      skipped?: number;
      missing?: number;
      failed?: number;
      bytes?: number;
    };
  } | null;
  backups?: BackupItem[];
  error?: string;
};

export type BackupPreview = {
  ok: boolean;
  tenantId?: string;
  backupId?: string;
  manifestPath?: string;
  createdAtMs?: number;
  source?: BackupSource;
  counts?: Record<string, number>;
  media?: {
    records?: unknown[];
    copied?: number;
    skipped?: number;
    missing?: number;
    failed?: number;
    bytes?: number;
  };
  error?: string;
};

export type BackupDiscovery = {
  ok: boolean;
  selectedPath?: string;
  backupRootDir?: string;
  namespaceDir?: string;
  tenantCount?: number;
  tenants?: Array<{
    tenantId?: string;
    tenantBackupDir?: string;
    latestBackup?: BackupItem;
    backups?: BackupItem[];
  }>;
  error?: string;
};

export type CommandResult = {
  ok: boolean;
  error?: string;
};

export type BackupStorageOverview = {
  ok: boolean;
  storageBreakdown?: {
    v5DatabaseBytes: number;
    v5MetadataBytes: number;
    legacySnapshotBytes: number;
    objectBytes: number;
    objectQuarantineBytes: number;
    legacyQuarantineBytes: number;
    archiveBundleBytes: number;
    stagingBytes: number;
    otherBytes: number;
  };
  totalLogicalBytes?: number;
  scanComplete?: boolean;
  scannedAtMs?: number;
  scanErrors?: string[];
  supportedSnapshotVersion?: number;
  latestBackupVersion?: number | null;
  snapshotPolicy?: SnapshotPolicy | null;
  maintenance?: BackupMaintenance;
  cleanupPreview?: { ok?: boolean; error?: string; items?: CleanupItem[] };
  otherEntries?: Array<{ relativePath?: string; type?: string; bytes?: number }>;
  stagingEntries?: Array<{ relativePath?: string; bytes?: number; owner?: {pcName?: string; createdAtMs?: number}; state?: string }>;
  legacyQuarantineItems?: Array<{snapshotName?: string; generation?: number; status?: string; action?: string; reason?: string; bytes?: number; purgeAfterMs?: number}> | null;
  currentOriginalBytes?: number;
  currentOriginalCount?: number;
  uniqueObjectCount?: number;
  uniqueObjectBytes?: number;
  databaseHistoryBytes?: number;
  legacySnapshotCount?: number;
  legacySnapshotBytes?: number;
  manualSnapshotCount?: number;
  manualSnapshotBytes?: number;
  manualBackups?: ManualBackupItem[];
  retention?: {
    recent?: number;
    dailyDays?: number;
    monthlyMonths?: number;
    preRestore?: number;
    manual?: number;
  };
  legacyReclaimableBytes?: number;
  legacyCleanupCandidateCount?: number;
  legacyQuarantineCount?: number | null;
  legacyQuarantineBytes?: number;
  legacyQuarantinePurgeAfterMs?: number | null;
  legacyQuarantineReviewCount?: number | null;
  legacyQuarantineError?: string | null;
  largestFiles?: Array<{ kind?: string; name?: string; localPath?: string; bytes?: number }>;
  error?: string;
};

export type SnapshotPolicy = {
  maxWritableSnapshotVersion?: number;
  reason?: string;
  checkedAtMs?: number;
  blockingDeviceCount?: number;
  blockingDevices?: Array<{deviceId?: string; deviceName?: string; snapshotFormatMax?: number; lastSeenAt?: number}>;
};
export type BackupMaintenance = {
  ok?: boolean; running?: boolean; lastAttemptAtMs?: number; lastSuccessAtMs?: number; nextRetryAtMs?: number; deferredReason?: string;
  stages?: Record<string,{ok?: boolean; error?: string; deleted?: number; deletedBytes?: number; quarantined?: number; quarantinedBytes?: number; purged?: number; purgedBytes?: number; reviewCount?: number}>;
};
export type CleanupItem = {
  manifestPath?: string; version?: number; kind?: string; generation?: number; createdAtMs?: number; deviceName?: string;
  bytes?: number; bytesComplete?: boolean; verification?: string; action?: string; plannedAction?: string; reason?: string;
};

export type LegacyCleanupPreview = {
  ok: boolean;
  previewToken?: string;
  candidateCount?: number;
  reclaimableBytes?: number;
  error?: string;
};
