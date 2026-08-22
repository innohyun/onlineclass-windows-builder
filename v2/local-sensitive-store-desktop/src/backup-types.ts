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
