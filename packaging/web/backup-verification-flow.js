(function exposeBackupVerification(scope) {
  "use strict";

  function unreadable() {
    const error = new Error("Invalid backup verification response");
    error.covalentGuidance = "Covalent couldn't confirm this backup's condition. Refresh the backup list, then try Verify again.";
    return error;
  }

  function summarize(report, selectedProviderIds) {
    if (!report || typeof report.intact !== "boolean"
        || !Number.isSafeInteger(report.verified) || report.verified < 0
        || !Array.isArray(report.missing) || !Array.isArray(report.corrupt)
        || ![...report.missing, ...report.corrupt].every((value) => typeof value === "string")
        || !report.providerAvailability || typeof report.providerAvailability !== "object"
        || Array.isArray(report.providerAvailability)
        || report.intact !== (report.missing.length === 0 && report.corrupt.length === 0)) {
      throw unreadable();
    }
    const statuses = new Set(["complete", "degraded", "offline", "corrupt", "revoked"]);
    if (!Object.values(report.providerAvailability).every((value) => statuses.has(value))) throw unreadable();

    const unavailable = selectedProviderIds.filter((id) => report.providerAvailability[id] !== "complete");
    if (!report.intact) {
      return {
        intact: false,
        summary: "This backup needs attention. Some local data is missing or damaged. Keep the original files and check your backup devices before restoring.",
      };
    }
    if (unavailable.length > 0) {
      return {
        intact: false,
        summary: "The local backup is intact, but some selected backup devices could not confirm a complete copy. Reconnect those devices, then Verify again.",
      };
    }
    return {
      intact: true,
      summary: selectedProviderIds.length > 0
        ? "Verified: the local backup and every selected backup device are intact."
        : "Verified: the local backup is intact. Add another backup device to protect against losing this device.",
    };
  }

  async function verify(api, backup) {
    if (!backup || typeof backup.backupId !== "string" || !backup.backupId
        || typeof backup.latestSnapshotId !== "string" || !backup.latestSnapshotId
        || !Array.isArray(backup.selectedProviderIds)
        || !backup.selectedProviderIds.every((id) => typeof id === "string" && id.length > 0)) {
      throw unreadable();
    }
    // Copy the exact selection before awaiting a response; an unrelated refresh
    // must never change which copies this result promises to have checked.
    const selectedProviderIds = [...new Set(backup.selectedProviderIds)];
    const report = await api("/api/v1/backups/verify", {
      method: "POST",
      body: JSON.stringify({
        backupId: backup.backupId,
        snapshotId: backup.latestSnapshotId,
        verifyProviders: selectedProviderIds.length > 0,
        repair: false,
      }),
    });
    return summarize(report, selectedProviderIds);
  }

  const flow = Object.freeze({ verify, summarize });
  scope.CovalentBackupVerification = flow;
  if (typeof module === "object" && module.exports) module.exports = flow;
}(typeof globalThis === "object" ? globalThis : window));
