(function exportBackupSelectionFlow(root, factory) {
  const flow = factory();
  if (typeof module === "object" && module.exports) module.exports = flow;
  root.CovalentBackupSelectionFlow = flow;
})(typeof globalThis === "object" ? globalThis : this, function createBackupSelectionFlow() {
  "use strict";

  const BACKUP_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  const SNAPSHOT_ID = /^[A-Za-z0-9_-]{1,256}$/;
  const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

  function guidance(text) {
    const error = new Error(text);
    error.covalentGuidance = text;
    return error;
  }

  function readable(value, fallback) {
    return typeof value === "string" && value.trim().length > 0 && !/[\u0000-\u001f\u007f]/.test(value)
      ? value.trim()
      : fallback;
  }

  function identifier(value, pattern) {
    return typeof value === "string" && pattern.test(value);
  }

  function choice(backupId, snapshotId, name, snapshotCount, source) {
    if (!identifier(backupId, BACKUP_ID) || !identifier(snapshotId, SNAPSHOT_ID)) return null;
    const label = readable(name, "Unnamed backup");
    const count = Number.isSafeInteger(snapshotCount) && snapshotCount >= 0 ? snapshotCount : 0;
    return Object.freeze({
      key: `${backupId}:${snapshotId}`,
      backupId,
      snapshotId,
      label: source === "receipt" ? `Recent backup: ${label}` : label,
      detail: source === "receipt"
        ? "Completed in this browser; it will appear in remembered backups after its receipt is confirmed."
        : `${count} retained snapshot${count === 1 ? "" : "s"}; latest completed backup.`,
      source,
    });
  }

  function choices(backups, receipt = null) {
    if (!Array.isArray(backups)) throw guidance("The node returned an invalid remembered-backup list.");
    const result = [];
    const seen = new Set();
    for (const backup of backups) {
      if (backup === null || typeof backup !== "object") continue;
      const item = choice(
        backup.backupId,
        backup.latestSnapshotId,
        backup.name,
        backup.snapshotCount,
        "remembered",
      );
      if (item !== null && !seen.has(item.key)) {
        seen.add(item.key);
        result.push(item);
      }
    }
    const recent = receipt?.phase === "receipt"
      ? choice(receipt.result?.backupId, receipt.result?.snapshotId, receipt.attempt?.displayName, 1, "receipt")
      : null;
    if (recent !== null && !seen.has(recent.key)) result.unshift(recent);
    return Object.freeze(result);
  }

  function resolve(available, selectedKey, manual) {
    if (!Array.isArray(available)) throw guidance("The restore choices are unavailable. Refresh remembered backups before previewing.");
    if (typeof selectedKey === "string" && selectedKey.length > 0) {
      const selected = available.find((item) => item.key === selectedKey);
      if (selected === undefined) {
        throw guidance("That remembered backup is no longer available. Refresh the list and choose it again.");
      }
      return Object.freeze({ backupId: selected.backupId, snapshotId: selected.snapshotId });
    }
    if (!manual || !identifier(manual.backupId, BACKUP_ID) || !identifier(manual.snapshotId, SNAPSHOT_ID)) {
      throw guidance("Choose a completed backup, or enter valid backup and snapshot IDs in Manual recovery.");
    }
    return Object.freeze({ backupId: manual.backupId, snapshotId: manual.snapshotId });
  }

  function defaultBackupName(sourceRoot) {
    const source = readable(sourceRoot, "");
    const folder = source.split(/[\\/]+/).filter(Boolean).at(-1);
    const name = readable(folder, "Selected files").slice(0, 113);
    return `${name} backup`;
  }

  function nextSnapshotId(randomUuid) {
    const value = randomUuid();
    if (!UUID.test(value)) throw guidance("This browser could not create a safe snapshot identifier. Reload the console before starting a backup.");
    return `snapshot-${value.replaceAll("-", "")}`;
  }

  return { choices, defaultBackupName, guidance, nextSnapshotId, resolve };
});
