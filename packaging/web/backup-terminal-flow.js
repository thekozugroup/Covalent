(function exportBackupTerminalFlow(root, factory) {
  const flow = factory();
  if (typeof module === "object" && module.exports) module.exports = flow;
  root.CovalentBackupTerminalFlow = flow;
})(typeof globalThis === "object" ? globalThis : this, function createBackupTerminalFlow() {
  "use strict";

  // A backup is terminal only after this client decoded its result and the
  // server accepted an explicit acknowledgement. Keep exactly one bounded,
  // origin-bound receipt in durable browser storage across tab/browser restarts.
  // Pairing remains intentionally tab-scoped in app.js.
  const STORAGE_KEY = "covalent.backup-terminal.v2";
  const LOCK_NAME = "covalent.backup-terminal.v2.lock";
  const SCHEMA_VERSION = 2;
  const RECEIPT_REVIEW_AFTER_MS = 7 * 24 * 60 * 60 * 1_000;
  const MAX_CLOCK_SKEW_MS = 5 * 60 * 1_000;
  const MAX_RECORD_BYTES = 32 * 1_024;
  const DEVICE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  const JOB_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
  const SNAPSHOT_ID = /^[A-Za-z0-9_-]{1,256}$/;
  const SHA256 = /^[0-9a-f]{64}$/;
  const ATTEMPT_KEYS = new Set([
    "sourceRoot", "backupId", "displayName", "snapshotId", "jobId", "selectedProviderIds",
  ]);
  const ATTEMPT_REQUIRED_KEYS = Object.freeze([
    "sourceRoot", "displayName", "snapshotId", "jobId", "selectedProviderIds",
  ]);
  const CONTEXT_KEYS = new Set(["origin", "protocolVersion"]);
  const RESULT_KEYS = new Set([
    "backupId", "snapshotId", "entries", "bytesRead", "chunksStored", "chunksDeduplicated",
    "selectedProviders", "degradedFailures",
  ]);
  const RESPONSE_KEYS = new Set(["result", "acknowledgementRequired"]);
  const REQUEST_RECORD_KEYS = new Set([
    "schemaVersion", "phase", "createdAtUnixMs", "reviewAfterUnixMs", "serverContext", "attempt", "integrity",
  ]);
  const RECEIPT_RECORD_KEYS = new Set([...REQUEST_RECORD_KEYS, "result"]);

  function guidance(text) {
    const error = new Error(text);
    error.covalentGuidance = text;
    return error;
  }

  function lockGuidance(text, kind) {
    const error = guidance(text);
    error.covalentBackupLockFailure = kind;
    return error;
  }

  function plainObject(value) {
    return value !== null && typeof value === "object" && !Array.isArray(value)
      && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
  }

  function onlyKnownKeys(value, keys) {
    return Object.keys(value).every((key) => keys.has(key));
  }

  function hasExactKeys(value, keys) {
    return Object.keys(value).length === keys.size && onlyKnownKeys(value, keys);
  }

  function visibleString(value, maximum) {
    return typeof value === "string" && value.length > 0 && value.length <= maximum
      && !/[\u0000-\u001f\u007f]/.test(value);
  }

  function counter(value) {
    return Number.isSafeInteger(value) && value >= 0;
  }

  function requireNow(value) {
    if (!Number.isSafeInteger(value) || value < 0) {
      throw guidance("This browser clock cannot safely validate the saved backup receipt.");
    }
    return value;
  }

  function requireContext(value) {
    if (!plainObject(value) || !hasExactKeys(value, CONTEXT_KEYS)
      || !visibleString(value.origin, 2_048)
      || !Number.isSafeInteger(value.protocolVersion)
      || value.protocolVersion < 1 || value.protocolVersion > 65_535) {
      throw guidance("This backup server context cannot be retained safely.");
    }
    let parsed;
    try { parsed = new URL(value.origin); }
    catch (_) { throw guidance("This backup server context cannot be retained safely."); }
    const loopbackHttp = parsed.protocol === "http:"
      && (parsed.hostname === "localhost" || parsed.hostname === "127.0.0.1" || parsed.hostname === "[::1]");
    if (parsed.origin !== value.origin || parsed.username !== "" || parsed.password !== ""
      || (parsed.protocol !== "https:" && !loopbackHttp)) {
      throw guidance("This backup server context cannot be retained safely.");
    }
    return Object.freeze({ origin: value.origin, protocolVersion: value.protocolVersion });
  }

  function requireAttempt(value) {
    if (!plainObject(value) || !onlyKnownKeys(value, ATTEMPT_KEYS)
      || !ATTEMPT_REQUIRED_KEYS.every((key) => Object.hasOwn(value, key))
      || !visibleString(value.sourceRoot, 4_096)
      || !(value.backupId === undefined || value.backupId === null || (typeof value.backupId === "string" && DEVICE_ID.test(value.backupId)))
      || !visibleString(value.displayName, 120)
      || !SNAPSHOT_ID.test(value.snapshotId)
      || !JOB_ID.test(value.jobId)
      || !Array.isArray(value.selectedProviderIds)
      || value.selectedProviderIds.length > 128
      || !value.selectedProviderIds.every((peerId) => typeof peerId === "string" && DEVICE_ID.test(peerId))
      || new Set(value.selectedProviderIds).size !== value.selectedProviderIds.length) {
      throw guidance("This backup request cannot be retained safely. Review the selected folder and backup devices, then start again.");
    }
    return Object.freeze({
      sourceRoot: value.sourceRoot,
      backupId: value.backupId,
      displayName: value.displayName,
      snapshotId: value.snapshotId,
      jobId: value.jobId,
      selectedProviderIds: Object.freeze([...value.selectedProviderIds]),
    });
  }

  function requireResult(value, attempt) {
    if (!plainObject(value) || !hasExactKeys(value, RESULT_KEYS)
      || !DEVICE_ID.test(value.backupId)
      || value.snapshotId !== attempt.snapshotId
      || ![
        value.entries, value.bytesRead, value.chunksStored, value.chunksDeduplicated,
        value.selectedProviders, value.degradedFailures,
      ].every(counter)
      || value.selectedProviders !== attempt.selectedProviderIds.length) {
      throw guidance("The backup server returned a completion Covalent cannot verify. Its retained result was not acknowledged.");
    }
    return Object.freeze({
      backupId: value.backupId,
      snapshotId: value.snapshotId,
      entries: value.entries,
      bytesRead: value.bytesRead,
      chunksStored: value.chunksStored,
      chunksDeduplicated: value.chunksDeduplicated,
      selectedProviders: value.selectedProviders,
      degradedFailures: value.degradedFailures,
    });
  }

  function sameContext(left, right) {
    return left.origin === right.origin && left.protocolVersion === right.protocolVersion;
  }

  function sameAttempt(left, right) {
    return JSON.stringify(left) === JSON.stringify(right);
  }

  function utf8Bytes(value) {
    try { return new TextEncoder().encode(value); }
    catch (_) { throw guidance("This browser cannot safely encode the backup receipt."); }
  }

  async function sha256(value) {
    if (!globalThis.crypto || !globalThis.crypto.subtle) {
      throw guidance("This browser cannot verify durable backup receipts securely.");
    }
    let digest;
    try { digest = await globalThis.crypto.subtle.digest("SHA-256", utf8Bytes(value)); }
    catch (_) { throw guidance("This browser cannot verify durable backup receipts securely."); }
    return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  }

  function payloadFor(phase, createdAtUnixMs, serverContext, attempt, result) {
    const payload = {
      schemaVersion: SCHEMA_VERSION,
      phase,
      createdAtUnixMs,
      reviewAfterUnixMs: createdAtUnixMs + RECEIPT_REVIEW_AFTER_MS,
      serverContext,
      attempt,
    };
    if (phase === "receipt") payload.result = result;
    return payload;
  }

  async function sealRecord(phase, createdAtUnixMs, serverContext, attempt, result) {
    const payload = payloadFor(phase, createdAtUnixMs, serverContext, attempt, result);
    const integrity = await sha256(JSON.stringify(payload));
    return Object.freeze({ ...payload, integrity });
  }

  function encodedRecord(record) {
    let encoded;
    try { encoded = JSON.stringify(record); }
    catch (_) { throw guidance("This browser could not retain the backup receipt safely. Its result was not acknowledged."); }
    if (utf8Bytes(encoded).byteLength > MAX_RECORD_BYTES) {
      throw guidance("This backup receipt is too large to retain safely. Its result was not acknowledged.");
    }
    return encoded;
  }

  // localStorage setItem is synchronous. Read-after-write verification occurs
  // before any backup request or acknowledgement is allowed onto the network.
  function persist(storage, record) {
    const encoded = encodedRecord(record);
    try {
      storage.setItem(STORAGE_KEY, encoded);
      if (storage.getItem(STORAGE_KEY) !== encoded) throw new Error("storage readback mismatch");
    } catch (_) {
      throw guidance("This browser could not retain the backup receipt safely. Its result was not acknowledged.");
    }
  }

  async function requireRecord(value, expectedContext, nowUnixMs) {
    if (!plainObject(value) || value.schemaVersion !== SCHEMA_VERSION
      || (value.phase !== "request" && value.phase !== "receipt")) {
      throw guidance("The saved backup receipt is not safe to resume. No new backup was started.");
    }
    const keys = value.phase === "request" ? REQUEST_RECORD_KEYS : RECEIPT_RECORD_KEYS;
    if (!hasExactKeys(value, keys)
      || !Number.isSafeInteger(value.createdAtUnixMs)
      || !Number.isSafeInteger(value.reviewAfterUnixMs)
      || value.createdAtUnixMs < 0
      || value.createdAtUnixMs > Number.MAX_SAFE_INTEGER - RECEIPT_REVIEW_AFTER_MS
      || value.reviewAfterUnixMs !== value.createdAtUnixMs + RECEIPT_REVIEW_AFTER_MS
      || typeof value.integrity !== "string" || !SHA256.test(value.integrity)) {
      throw guidance("The saved backup receipt is not safe to resume. No new backup was started.");
    }
    if (value.createdAtUnixMs > nowUnixMs + MAX_CLOCK_SKEW_MS) {
      throw guidance("The saved backup receipt is dated in the future. No new backup was started.");
    }
    const serverContext = requireContext(value.serverContext);
    if (!sameContext(serverContext, expectedContext)) {
      throw guidance("The saved backup receipt belongs to a different backup server. No request was sent.");
    }
    const attempt = requireAttempt(value.attempt);
    const result = value.phase === "receipt" ? requireResult(value.result, attempt) : undefined;
    const payload = payloadFor(value.phase, value.createdAtUnixMs, serverContext, attempt, result);
    const expectedIntegrity = await sha256(JSON.stringify(payload));
    if (value.integrity !== expectedIntegrity) {
      throw guidance("The saved backup receipt was changed or corrupted. It was retained and no request was sent.");
    }
    return Object.freeze({ ...payload, integrity: value.integrity });
  }

  async function load(storage, context, nowUnixMs = Date.now()) {
    const expectedContext = requireContext(context);
    const now = requireNow(nowUnixMs);
    let encoded;
    try { encoded = storage.getItem(STORAGE_KEY); }
    catch (_) { throw guidance("This browser could not read its durable backup receipt. No request was sent."); }
    if (encoded === null) return null;
    if (typeof encoded !== "string" || utf8Bytes(encoded).byteLength > MAX_RECORD_BYTES) {
      throw guidance("The saved backup receipt is not safe to resume. No new backup was started.");
    }
    let parsed;
    try { parsed = JSON.parse(encoded); }
    catch (_) { throw guidance("The saved backup receipt is corrupted. It was retained and no request was sent."); }
    return requireRecord(parsed, expectedContext, now);
  }

  async function prepare(storage, context, attempt, nowUnixMs = Date.now()) {
    const serverContext = requireContext(context);
    const checked = requireAttempt(attempt);
    const now = requireNow(nowUnixMs);
    const existing = await load(storage, serverContext, now);
    if (existing !== null) {
      if (!sameAttempt(existing.attempt, checked)) {
        throw guidance("Finish confirming the previous backup result before starting another backup.");
      }
      return existing;
    }
    const pending = await sealRecord("request", now, serverContext, checked);
    persist(storage, pending);
    return pending;
  }

  async function accept(storage, context, attempt, response, nowUnixMs = Date.now()) {
    const serverContext = requireContext(context);
    const now = requireNow(nowUnixMs);
    const pending = await load(storage, serverContext, now);
    if (pending === null || pending.phase !== "request" || !sameAttempt(pending.attempt, attempt)) {
      throw guidance("This backup response does not match the durable request receipt, so it was not acknowledged.");
    }
    if (!plainObject(response) || !hasExactKeys(response, RESPONSE_KEYS)
      || response.acknowledgementRequired !== true) {
      throw guidance("This backup response did not require the expected receipt confirmation, so it was not accepted.");
    }
    const result = requireResult(response.result, pending.attempt);
    const receipt = await sealRecord("receipt", now, serverContext, pending.attempt, result);
    // Persist before returning a result to the UI. The caller must render and
    // accept it before calling acknowledge(); a storage failure leaves the
    // server-side terminal result retained and the original request retryable.
    persist(storage, receipt);
    return receipt;
  }

  async function submit(storage, context, attempt, request, clock = Date.now) {
    const pending = await prepare(storage, context, attempt, clock());
    if (pending.phase === "receipt") return pending;
    const response = await request(pending.attempt);
    return accept(storage, context, pending.attempt, response, clock());
  }

  async function withExclusiveLock(callback, locks = globalThis.navigator?.locks) {
    if (typeof callback !== "function") {
      throw guidance("The durable backup operation is invalid.");
    }
    if (!locks || typeof locks.request !== "function") {
      throw lockGuidance(
        "This browser cannot coordinate durable backups across tabs. Use a current browser with Web Locks support; no backup request was sent.",
        "unavailable",
      );
    }
    let callbackEntered = false;
    try {
      return await locks.request(
        LOCK_NAME,
        { mode: "exclusive", ifAvailable: true },
        async (lock) => {
          if (lock === null) {
            throw lockGuidance(
              "Another tab is already handling this backup receipt. Return to that tab, or retry here after it closes.",
              "busy",
            );
          }
          callbackEntered = true;
          return callback();
        },
      );
    } catch (error) {
      if (error?.covalentBackupLockFailure) throw error;
      if (callbackEntered) throw error;
      throw lockGuidance(
        "This browser could not acquire the durable backup lock. No backup request was sent.",
        "unavailable",
      );
    }
  }

  function clearAfterAcknowledgement(storage) {
    try {
      storage.removeItem(STORAGE_KEY);
      if (storage.getItem(STORAGE_KEY) !== null) throw new Error("storage deletion mismatch");
    } catch (_) {
      throw guidance("The server confirmed the receipt, but this browser could not clear its local copy. Retry confirmation before starting another backup.");
    }
  }

  async function acknowledge(storage, context, apiResponse, nowUnixMs = Date.now()) {
    const receipt = await load(storage, context, nowUnixMs);
    if (receipt === null || receipt.phase !== "receipt") return null;
    // Re-write and synchronously verify the exact receipt before the ACK. A
    // failed request preserves it; only an explicit 204 clears durable state.
    persist(storage, receipt);
    const response = await apiResponse("/api/v1/jobs/acknowledge", {
      method: "POST",
      body: JSON.stringify({ jobId: receipt.attempt.jobId }),
    });
    if (!plainObject(response) || response.status !== 204) {
      throw guidance("The backup server did not confirm receipt with the expected response. The durable receipt was retained.");
    }
    clearAfterAcknowledgement(storage);
    return receipt;
  }

  return Object.freeze({
    MAX_RECORD_BYTES,
    LOCK_NAME,
    RECEIPT_REVIEW_AFTER_MS,
    SCHEMA_VERSION,
    STORAGE_KEY,
    accept,
    acknowledge,
    guidance,
    load,
    prepare,
    requireAttempt,
    requireContext,
    requireResult,
    submit,
    withExclusiveLock,
  });
});
