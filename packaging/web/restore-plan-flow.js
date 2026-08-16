(function exportRestorePlanFlow(root, factory) {
  const flow = factory();
  if (typeof module === "object" && module.exports) module.exports = flow;
  root.CovalentRestorePlanFlow = flow;
})(typeof globalThis === "object" ? globalThis : this, function createRestorePlanFlow() {
  "use strict";

  const digestPattern = /^[0-9a-f]{64}$/;
  const cursorPattern = /^[0-9]+$/;

  function requireReference(value) {
    if (!value || typeof value !== "object"
      || !digestPattern.test(value.planId)
      || !digestPattern.test(value.planDigest)
      || !digestPattern.test(value.manifestDigest)
      || !Number.isSafeInteger(value.totalEntries)
      || value.totalEntries < 0
      || value.totalEntries > 1_000_000
      || typeof value.authorizedRoot !== "string"
      || typeof value.jobId !== "string"
      || typeof value.signature !== "string"
      || value.signature.length === 0) {
      throw new Error("The node returned an invalid durable restore-plan reference.");
    }
    return value;
  }

  function requirePage(reference, page, limit) {
    requireReference(reference);
    if (!page || typeof page !== "object"
      || page.planId !== reference.planId
      || page.planDigest !== reference.planDigest
      || page.manifestDigest !== reference.manifestDigest
      || page.backupId !== reference.backupId
      || page.snapshotId !== reference.snapshotId
      || page.authorizedRoot !== reference.authorizedRoot
      || page.conflictPolicy !== reference.conflictPolicy
      || page.jobId !== reference.jobId
      || page.signerDeviceId !== reference.signerDeviceId
      || page.signature !== reference.signature
      || page.totalEntries !== reference.totalEntries
      || !Number.isSafeInteger(page.entryOffset)
      || page.entryOffset < 0
      || !Array.isArray(page.entries)
      || page.entries.length > limit
      || page.entryOffset + page.entries.length > reference.totalEntries
      || (page.nextCursor !== null && !cursorPattern.test(page.nextCursor))) {
      throw new Error("The restore-plan page does not match the signed durable reference.");
    }
    return page;
  }

  async function page(api, reference, cursor = null, limit = 100) {
    requireReference(reference);
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 1_000) {
      throw new Error("Restore-plan page size must be between 1 and 1,000.");
    }
    if (cursor !== null && !cursorPattern.test(cursor)) {
      throw new Error("Restore-plan cursor is invalid.");
    }
    const query = new URLSearchParams({ limit: String(limit) });
    if (cursor !== null) query.set("cursor", cursor);
    const result = await api("/api/v1/restores/plans/" + reference.planId + "?" + query);
    return requirePage(reference, result, limit);
  }

  async function execute(api, reference) {
    requireReference(reference);
    return api("/api/v1/restores/execute", {
      method: "POST",
      body: JSON.stringify({ planId: reference.planId }),
    });
  }

  async function discard(api, reference) {
    if (!reference) return;
    requireReference(reference);
    await api("/api/v1/jobs/discard", {
      method: "POST",
      body: JSON.stringify({ jobId: reference.jobId }),
    });
  }

  return { discard, execute, page, requirePage, requireReference };
});
