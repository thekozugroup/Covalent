(function exportRecoveryFlow(root, factory) {
  const flow = factory();
  if (typeof module === "object" && module.exports) module.exports = flow;
  root.CovalentRecoveryFlow = flow;
})(globalThis, function createRecoveryFlow() {
  "use strict";

  const MAX_KIT_BYTES = 16 * 1024 * 1024;
  const MAX_RESPONSE_BYTES = 24 * 1024 * 1024;
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  const snapshot = /^[A-Za-z0-9_-]{1,128}$/;
  const phases = new Set(["not_configured", "pending", "imported", "partial", "blocked", "no_catalogs"]);

  function guidance(text) {
    const error = new Error(text);
    error.covalentGuidance = text;
    return error;
  }

  function invalid() {
    return guidance("The recovery response was incomplete or invalid. Update Covalent and try again.");
  }

  function exactObject(value, fields) {
    return value !== null && typeof value === "object" && !Array.isArray(value)
      && Object.keys(value).length === fields.length
      && fields.every((field) => Object.hasOwn(value, field));
  }

  // This reader bounds bytes before JSON parsing. Parser errors are replaced
  // with authored copy so a browser's JSON excerpt cannot expose a recovery code.
  async function readJson(response) {
    const limit = response.ok ? MAX_RESPONSE_BYTES : 8192;
    const declared = response.headers.get("content-length");
    if (declared !== null && (!/^[0-9]+$/.test(declared) || Number(declared) > limit)) {
      await response.body?.cancel();
      throw invalid();
    }
    if (!response.body) throw invalid();
    const reader = response.body.getReader();
    const chunks = [];
    let total = 0;
    let joined;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        total += value.byteLength;
        if (total > limit) { value.fill(0); throw invalid(); }
        chunks.push(value);
      }
      joined = new Uint8Array(total);
      let offset = 0;
      for (const chunk of chunks) { joined.set(chunk, offset); offset += chunk.length; }
      return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(joined));
    } catch {
      await reader.cancel().catch(() => {});
      throw invalid();
    } finally {
      joined?.fill(0);
      chunks.forEach((chunk) => chunk.fill(0));
      reader.releaseLock();
    }
  }

  function decode(value, maximum) {
    if (typeof value !== "string" || value.length === 0
      || value.length > Math.ceil(maximum * 4 / 3)
      || !/^[A-Za-z0-9_-]+$/.test(value) || value.length % 4 === 1) throw invalid();
    let bytes;
    try {
      const binary = atob(value.replace(/-/g, "+").replace(/_/g, "/"));
      bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
      if (bytes.length > maximum
        || btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "") !== value) throw invalid();
      return bytes;
    } catch {
      bytes?.fill(0);
      throw invalid();
    }
  }

  function decodeExport(value) {
    let kit;
    let key;
    try {
      if (!exactObject(value, ["protocolVersion", "recoveryKit", "recoveryKey"])
        || value.protocolVersion !== 1 || typeof value.recoveryKey !== "string"
        || value.recoveryKey.length !== 43) throw invalid();
      key = decode(value.recoveryKey, 32);
      if (key.length !== 32) throw invalid();
      kit = decode(value.recoveryKit, MAX_KIT_BYTES);
      return { kit, code: new TextEncoder().encode(value.recoveryKey) };
    } catch {
      kit?.fill(0);
      throw invalid();
    } finally {
      key?.fill(0);
    }
  }

  function disposePair(pair) {
    pair?.kit.fill(0);
    pair?.code.fill(0);
  }

  // Neither the pair nor its download state is written to browser storage.
  // JavaScript cannot erase immutable JSON strings; keep their lifetime short
  // and wipe owned mutable buffers on every close, stale response, and failure.
  function session() {
    let pair = null;
    let generation = 0;
    let pending = null;
    return {
      async generate(api) {
        if (pair || pending) throw guidance("Finish saving or discard the current recovery files first.");
        const revision = generation;
        const controller = new AbortController();
        pending = controller;
        try {
          const result = decodeExport(await api("/api/v1/recovery/kit", {
            method: "POST", body: JSON.stringify({ confirmed: true }), signal: controller.signal,
          }));
          if (revision !== generation) { disposePair(result); return false; }
          pair = result;
          return true;
        } catch (error) {
          if (revision !== generation) return false;
          throw error;
        } finally {
          if (pending === controller) pending = null;
        }
      },
      download(kind, save) {
        if (!pair || (kind !== "kit" && kind !== "code")) {
          throw guidance("Create recovery files before downloading them.");
        }
        const bytes = pair[kind].slice();
        try { save(bytes); } finally { bytes.fill(0); }
      },
      dispose() {
        generation += 1;
        pending?.abort();
        pending = null;
        disposePair(pair);
        pair = null;
      },
    };
  }

  function ids(values) {
    if (!Array.isArray(values) || values.length > 128
      || values.some((id) => typeof id !== "string" || !uuid.test(id))
      || new Set(values).size !== values.length) throw invalid();
    return new Set(values);
  }

  function status(value) {
    if (!exactObject(value, ["protocolVersion", "phase", "recoveredBackups", "queriedProviderIds",
      "configuredProviderIds", "failures", "newerSnapshotMayExist"])
      || value.protocolVersion !== 1 || !phases.has(value.phase)
      || typeof value.newerSnapshotMayExist !== "boolean"
      || !Array.isArray(value.recoveredBackups) || value.recoveredBackups.length > 100000
      || !Array.isArray(value.failures) || value.failures.length > 256) throw invalid();
    const configured = ids(value.configuredProviderIds);
    const queried = ids(value.queriedProviderIds);
    if ([...queried].some((id) => !configured.has(id))) throw invalid();
    const backups = new Set();
    for (const backup of value.recoveredBackups) {
      if (!exactObject(backup, ["backupId", "snapshotId", "sourceProviderIds"])
        || typeof backup.backupId !== "string" || !uuid.test(backup.backupId)
        || typeof backup.snapshotId !== "string" || !snapshot.test(backup.snapshotId)
        || backups.has(backup.backupId)) throw invalid();
      const sources = ids(backup.sourceProviderIds);
      if (sources.size === 0 || [...sources].some((id) => !configured.has(id))) throw invalid();
      backups.add(backup.backupId);
    }
    for (const failure of value.failures) {
      if (!exactObject(failure, ["providerId", "snapshotId", "reason"])
        || !configured.has(failure.providerId)
        || (failure.snapshotId !== null && (typeof failure.snapshotId !== "string" || !snapshot.test(failure.snapshotId)))
        || typeof failure.reason !== "string" || !/^[a-z0-9_]{1,128}$/.test(failure.reason)) throw invalid();
    }
    const uncertain = value.newerSnapshotMayExist || value.failures.length > 0 || queried.size !== configured.size;
    if ((value.phase === "imported" && (uncertain || backups.size === 0))
      || (value.phase === "no_catalogs" && (uncertain || backups.size !== 0))
      || (value.phase === "partial" && (backups.size === 0 || !value.newerSnapshotMayExist))
      || (value.phase === "blocked" && (backups.size !== 0 || !value.newerSnapshotMayExist))
      || (value.phase === "pending" && (!value.newerSnapshotMayExist || backups.size !== 0))
      || (value.phase === "not_configured" && (backups.size || configured.size || queried.size || uncertain))) throw invalid();
    const copy = {
      not_configured: "This backup server has not been restored from recovery files.",
      pending: "Your identity is restored. Covalent still needs to check the other backup devices.",
      imported: "Recovery finished. All expected backup devices answered.",
      partial: "Some backups were recovered, but other copies could not be checked safely.",
      blocked: "Covalent could not safely recover a backup yet. Bring your backup devices online and retry.",
      no_catalogs: "The expected backup devices answered, but none had a recovery catalog for this identity.",
    };
    return Object.freeze({
      phase: value.phase,
      summary: copy[value.phase],
      detail: `${backups.size} backup${backups.size === 1 ? "" : "s"} recovered; ${queried.size} of ${configured.size} backup devices checked.`,
      warning: value.newerSnapshotMayExist ? "A newer backup may still exist on an unavailable or unchecked device." : "",
      canRetry: ["pending", "partial", "blocked", "no_catalogs"].includes(value.phase),
    });
  }

  return { MAX_KIT_BYTES, MAX_RESPONSE_BYTES, decodeExport, disposePair, readJson, session, status };
});
