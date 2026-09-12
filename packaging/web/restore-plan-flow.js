(function exportRestorePlanFlow(root, factory) {
  const flow = factory();
  if (typeof module === "object" && module.exports) module.exports = flow;
  root.CovalentRestorePlanFlow = flow;
})(typeof globalThis === "object" ? globalThis : this, function createRestorePlanFlow() {
  "use strict";

  const digestPattern = /^[0-9a-f]{64}$/;
  const cursorPattern = /^[0-9]+$/;

  // Authored copy, marked so the console's error presenter can tell a sentence
  // written in this repository apart from a runtime string it must never show.
  function guidance(text) {
    const error = new Error(text);
    error.covalentGuidance = text;
    return error;
  }

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
      throw guidance("The node returned an invalid durable restore-plan reference.");
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
      throw guidance("The restore-plan page does not match the signed durable reference.");
    }
    return page;
  }

  function fullDestination(authorizedRoot, destinationPath) {
    if (typeof authorizedRoot !== "string" || authorizedRoot.length === 0
      || typeof destinationPath !== "string" || destinationPath.length === 0) {
      throw guidance("The signed restore preview has an invalid destination.");
    }
    return authorizedRoot.endsWith("/") ? authorizedRoot + destinationPath : authorizedRoot + "/" + destinationPath;
  }

  function describeEntry(entry, authorizedRoot) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)
      || typeof entry.sourcePath !== "string" || entry.sourcePath.length === 0
      || typeof entry.destinationPath !== "string" || entry.destinationPath.length === 0) {
      throw guidance("The signed restore preview has an invalid entry.");
    }
    let action;
    if (entry.kind === "directory" && entry.action === "create_directory") {
      action = "Create this folder.";
    } else if (entry.kind === "directory" && entry.action === "keep_directory") {
      action = "Use the existing folder.";
    } else if (entry.kind === "file" && entry.action === "create_file") {
      action = "Restore this file.";
    } else if (entry.kind === "file" && entry.action === "skip_file") {
      action = "Conflict: skip this file and keep the existing file.";
    } else if (entry.kind === "file" && entry.action === "replace_file") {
      action = "Conflict: replace the existing file.";
    } else if (entry.kind === "file" && entry.action === "rename_file") {
      action = "Conflict: restore a renamed copy.";
    } else {
      throw guidance("The signed restore preview contains an action this console cannot safely explain.");
    }
    return Object.freeze({
      action,
      destination: fullDestination(authorizedRoot, entry.destinationPath),
      source: entry.sourcePath,
      renamed: entry.action === "rename_file",
    });
  }

  function describePage(page, authorizedRoot) {
    if (!page || !Array.isArray(page.entries)) {
      throw guidance("The signed restore preview has no entries to display.");
    }
    return Object.freeze(page.entries.map((entry) => describeEntry(entry, authorizedRoot)));
  }

  async function page(api, reference, cursor = null, limit = 100) {
    requireReference(reference);
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 1_000) {
      throw guidance("Restore-plan page size must be between 1 and 1,000.");
    }
    if (cursor !== null && !cursorPattern.test(cursor)) {
      throw guidance("Restore-plan cursor is invalid.");
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

  return { describeEntry, describePage, discard, execute, guidance, page, requirePage, requireReference };
});
