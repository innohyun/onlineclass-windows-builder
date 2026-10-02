import type { BackupStorageOverview } from "./backup-types";

export function summarizeBackupStorage(storage: BackupStorageOverview | null) {
  const fields = storage?.storageBreakdown;
  const keys = ["v5DatabaseBytes", "v5MetadataBytes", "legacySnapshotBytes", "objectBytes", "objectQuarantineBytes", "legacyQuarantineBytes", "archiveBundleBytes", "stagingBytes", "otherBytes"] as const;
  const valid = Boolean(fields) && keys.every((key) => typeof fields?.[key] === "number" && Number.isSafeInteger(fields[key]) && fields[key]! >= 0);
  const sum = valid ? keys.reduce((total, key) => total + fields![key]!, 0) : null;
  const complete = storage?.ok === true && storage.scanComplete === true && sum !== null && sum === storage.totalLogicalBytes;
  const add = (...values: (number | null | undefined)[]) => values.every((value) => typeof value === "number" && Number.isSafeInteger(value) && value >= 0) ? values.reduce<number>((total, value) => total + value!, 0) : null;
  return {
    complete,
    total: sum,
    database: fields?.v5DatabaseBytes,
    attachments: fields?.objectBytes,
    archives: fields?.archiveBundleBytes,
    other: add(fields?.v5MetadataBytes, fields?.legacySnapshotBytes, fields?.stagingBytes, fields?.otherBytes),
    quarantine: add(fields?.objectQuarantineBytes, fields?.legacyQuarantineBytes),
  };
}
