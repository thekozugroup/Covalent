(function exportFolderSyncFlow(root, factory) {
  const flow = factory();
  if (typeof module === "object" && module.exports) module.exports = flow;
  root.CovalentFolderSyncFlow = flow;
})(typeof globalThis === "object" ? globalThis : this, function createFolderSyncFlow() {
  "use strict";

  const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
  const STORAGE_PREFIX = "covalent.folder-offer.v1.";
  const MAX_COLLECTION = 1024;
  const MAX_RESPONSE_BYTES = 2 * 1024 * 1024;
  const MAX_STORAGE_BYTES = 4096;
  const LIFECYCLES = new Set(["stopped", "running", "stillStopping", "needsAttention", "initialScanning"]);
  const AVAILABILITY = new Set(["available", "notPackaged", "needsAttention"]);
  const FRESHNESS = new Set(["neverObserved", "fresh", "stale"]);
  const PHASES = new Set(["offered", "awaitingCommit", "ready", "paused", "removed"]);
  const HEALTH_STATES = new Set([
    "starting", "idle", "scanning", "scan-waiting", "sync-waiting", "sync-preparing",
    "syncing", "cleaning", "clean-waiting", "error",
  ]);
  const ISSUES = new Set([
    "installation", "folderAccess", "journal", "workerLaunch", "workerHealth",
    "workerStop", "peerRevocation", "initialScan",
  ]);

  function guidance(message) {
    const error = new Error("folder sync guidance");
    error.covalentGuidance = message;
    return error;
  }

  function object(value, message) {
    if (value === null || typeof value !== "object" || Array.isArray(value)) throw guidance(message);
    return value;
  }

  function string(value, maximum, message, allowEmpty = false) {
    if (typeof value !== "string" || value.length > maximum || (!allowEmpty && value.trim().length === 0)) {
      throw guidance(message);
    }
    return value;
  }

  function uuid(value, message = "The node returned an invalid folder identifier.") {
    if (typeof value !== "string" || !UUID.test(value)) throw guidance(message);
    return value.toLowerCase();
  }

  function unsigned(value, message) {
    if (!Number.isSafeInteger(value) || value < 0) throw guidance(message);
    return value;
  }

  function boundedArray(value, message) {
    if (!Array.isArray(value) || value.length > MAX_COLLECTION) throw guidance(message);
    return value;
  }

  async function readJson(response) {
    const limit = response.ok ? MAX_RESPONSE_BYTES : 8192;
    const invalid = () => guidance("The node returned a folder response this console cannot safely read.");
    const declared = response.headers.get("content-length");
    if (declared !== null && (!/^[0-9]+$/.test(declared) || Number(declared) > limit)) {
      await response.body?.cancel();
      throw invalid();
    }
    if (!response.body) throw invalid();
    const reader = response.body.getReader();
    const chunks = [];
    let total = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        total += value.byteLength;
        if (total > limit) throw invalid();
        chunks.push(value);
      }
      const joined = new Uint8Array(total);
      let offset = 0;
      for (const chunk of chunks) { joined.set(chunk, offset); offset += chunk.byteLength; }
      return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(joined));
    } catch (_) {
      await reader.cancel().catch(() => {});
      throw invalid();
    } finally {
      reader.releaseLock();
    }
  }

  function selectedRoot(value) {
    const path = string(value, 1024, "Choose a valid absolute folder path on this server.");
    if (!path.startsWith("/") || path.includes("\0")) {
      throw guidance("Choose a valid absolute folder path on this server.");
    }
    return path;
  }

  function offerBody(value) {
    const body = object(value, "This folder offer is invalid.");
    return Object.freeze({
      peerId: uuid(body.peerId, "Choose a currently paired device."),
      folderId: uuid(body.folderId),
      label: string(body.label, 120, "Enter a folder name up to 120 characters.").trim(),
      selectedRoot: selectedRoot(body.selectedRoot),
    });
  }

  function requireStatus(value) {
    const status = object(value, "The node returned an invalid folder status.");
    if (status.schemaVersion !== 1 || !AVAILABILITY.has(status.availability)
      || !LIFECYCLES.has(status.lifecycle) || !FRESHNESS.has(status.healthFreshness)
      || !(status.issue === null || ISSUES.has(status.issue))) {
      throw guidance("The node returned a folder status this console cannot safely read.");
    }
    const peers = boundedArray(status.peers, "The node returned too many paired devices.").map((value) => {
      const peer = object(value, "The node returned an invalid paired device.");
      return Object.freeze({
        peerId: uuid(peer.peerId, "The node returned an invalid paired device."),
        displayName: string(peer.displayName, 120, "The node returned an invalid paired-device name."),
      });
    });
    const shares = boundedArray(status.shares, "The node returned too many shared folders.").map((value) => {
      const share = object(value, "The node returned an invalid shared folder.");
      if (!PHASES.has(share.phase) || typeof share.incoming !== "boolean"
        || typeof share.expired !== "boolean"
        || !(share.expiresAtUnixMs === null || share.expiresAtUnixMs === undefined
          || Number.isSafeInteger(share.expiresAtUnixMs) && share.expiresAtUnixMs >= 0)) {
        throw guidance("The node returned an invalid shared-folder state.");
      }
      return Object.freeze({
        offerId: uuid(share.offerId),
        folderId: uuid(share.folderId),
        label: string(share.label, 120, "The node returned an invalid folder name."),
        peerId: uuid(share.peerId, "The node returned an invalid paired device."),
        incoming: share.incoming,
        phase: share.phase,
        expiresAtUnixMs: share.expiresAtUnixMs ?? null,
        // The server owns expiry. Browser clock arithmetic must never override it.
        expired: share.expired,
      });
    });
    const folders = boundedArray(status.folders, "The node returned too many folder health records.").map((value) => {
      const folder = object(value, "The node returned invalid folder health.");
      if (!HEALTH_STATES.has(folder.state) || typeof folder.statusError !== "boolean"
        || typeof folder.watchError !== "boolean") {
        throw guidance("The node returned invalid folder health.");
      }
      return Object.freeze({
        folderId: uuid(folder.folderId),
        state: folder.state,
        remainingFiles: unsigned(folder.remainingFiles, "The node returned invalid folder health."),
        remainingBytes: unsigned(folder.remainingBytes, "The node returned invalid folder health."),
        scanPullErrorCount: unsigned(folder.scanPullErrorCount, "The node returned invalid folder health."),
        reportedErrorRows: unsigned(folder.reportedErrorRows, "The node returned invalid folder health."),
        statusError: folder.statusError,
        watchError: folder.watchError,
      });
    });
    return Object.freeze({
      schemaVersion: 1,
      availability: status.availability,
      lifecycle: status.lifecycle,
      issue: status.issue,
      healthFreshness: status.healthFreshness,
      peers: Object.freeze(peers),
      shares: Object.freeze(shares),
      folders: Object.freeze(folders),
    });
  }

  function mutationResponse(value) {
    const response = object(value, "The node returned an invalid folder change result.");
    if (response.schemaVersion !== 1 || !LIFECYCLES.has(response.lifecycle)
      || !(response.issue === null || ISSUES.has(response.issue))
      || !(response.offerId === null || response.offerId === undefined || UUID.test(response.offerId))) {
      throw guidance("The node returned a folder change result this console cannot safely read.");
    }
    return Object.freeze({
      schemaVersion: 1,
      offerId: response.offerId == null ? null : response.offerId.toLowerCase(),
      lifecycle: response.lifecycle,
      issue: response.issue,
    });
  }

  function statusSummary(status) {
    if (status.availability !== "available") {
      if (status.issue === "folderAccess") {
        return Object.freeze({ kind: "offline", text: "Folder sync is offline because this server cannot access a saved folder." });
      }
      return Object.freeze({ kind: "offline", text: "Folder sync is offline on this server." });
    }
    if (status.lifecycle === "initialScanning") {
      return Object.freeze({ kind: "checking", text: "Checking local folder contents before sync starts." });
    }
    if (status.lifecycle === "needsAttention" || status.issue !== null) {
      const text = status.issue === "initialScan"
        ? "The initial local folder check did not complete. Try it again before sync starts."
        : "Folder sync needs attention before it can continue.";
      return Object.freeze({ kind: "attention", text });
    }
    if (status.lifecycle !== "running") {
      return Object.freeze({ kind: "offline", text: "Folder sync is stopped on this server." });
    }
    if (status.healthFreshness !== "fresh") {
      return Object.freeze({ kind: "unknown", text: "Folder sync is running, but current folder health is not known yet." });
    }
    return Object.freeze({ kind: "ready", text: "Local folder health was checked recently." });
  }

  function shareView(status, share) {
    if (share.expired) return Object.freeze({ kind: "expired", text: "Invitation expired" });
    if (share.phase === "paused") return Object.freeze({ kind: "paused", text: "Paused" });
    if (status.lifecycle === "initialScanning") return Object.freeze({ kind: "checking", text: "Checking folder" });
    if (status.lifecycle === "needsAttention" || status.issue !== null) {
      return Object.freeze({ kind: "attention", text: "Needs attention" });
    }
    if (share.phase === "offered" || share.phase === "awaitingCommit") {
      return Object.freeze({ kind: "waiting", text: share.incoming ? "Waiting for your folder" : "Waiting for other device" });
    }
    if (status.lifecycle !== "running") return Object.freeze({ kind: "offline", text: "Folder sync offline" });
    if (status.healthFreshness !== "fresh") {
      return Object.freeze({ kind: "unknown", text: "Current folder health unknown" });
    }
    const health = status.folders.find((item) => item.folderId === share.folderId);
    if (!health) return Object.freeze({ kind: "unknown", text: "Waiting for folder status" });
    if (health.statusError || health.watchError || health.scanPullErrorCount > 0
      || health.reportedErrorRows > 0 || health.state === "error") {
      return Object.freeze({ kind: "attention", text: "Needs attention" });
    }
    if (health.state === "starting" || health.state === "scanning" || health.state === "scan-waiting") {
      return Object.freeze({ kind: "checking", text: "Checking folder" });
    }
    if (health.state === "syncing" || health.state === "sync-waiting" || health.state === "sync-preparing"
      || health.remainingFiles > 0 || health.remainingBytes > 0) {
      return Object.freeze({ kind: "syncing", text: "Syncing" });
    }
    if (health.state === "idle") return Object.freeze({ kind: "ready", text: "Folder ready" });
    return Object.freeze({ kind: "checking", text: "Checking folder" });
  }

  function storageKey(deviceId) {
    return `${STORAGE_PREFIX}${uuid(deviceId, "The server identity is invalid.")}`;
  }

  function lazySessionStorage(scope) {
    if (scope === null || (typeof scope !== "object" && typeof scope !== "function")) {
      throw new TypeError("session storage scope is required");
    }
    // Reading Window.sessionStorage can itself throw when browser storage is
    // disabled. Defer that getter until a folder receipt operation so the
    // rest of the console can still initialize and make unrelated requests.
    return Object.freeze({
      getItem(key) { return scope.sessionStorage.getItem(key); },
      setItem(key, value) { scope.sessionStorage.setItem(key, value); },
      removeItem(key) { scope.sessionStorage.removeItem(key); },
    });
  }

  function pendingRecord(deviceId, body) {
    return Object.freeze({ schemaVersion: 1, deviceId: uuid(deviceId), body: offerBody(body) });
  }

  function savePending(storage, deviceId, body) {
    const key = storageKey(deviceId);
    const encoded = JSON.stringify(pendingRecord(deviceId, body));
    if (encoded.length > MAX_STORAGE_BYTES) throw guidance("This folder offer is too large to retain safely.");
    try {
      storage.setItem(key, encoded);
      if (storage.getItem(key) !== encoded) throw new Error("storage did not retain exact value");
    } catch (_) {
      throw guidance("This browser could not retain the folder offer, so no request was sent.");
    }
    return JSON.parse(encoded).body;
  }

  function loadPending(storage, deviceId) {
    let encoded;
    try { encoded = storage.getItem(storageKey(deviceId)); }
    catch (_) { throw guidance("This browser could not read its pending folder offer. No request was sent."); }
    if (encoded === null) return null;
    if (encoded.length > MAX_STORAGE_BYTES) throw guidance("The saved folder offer is invalid. No request was sent.");
    let decoded;
    try { decoded = JSON.parse(encoded); }
    catch (_) { throw guidance("The saved folder offer is invalid. No request was sent."); }
    const record = object(decoded, "The saved folder offer is invalid. No request was sent.");
    if (Object.keys(record).length !== 3 || !["schemaVersion", "deviceId", "body"].every((key) => Object.hasOwn(record, key))
      || record.schemaVersion !== 1 || uuid(record.deviceId) !== uuid(deviceId)) {
      throw guidance("The saved folder offer belongs to a different server. No request was sent.");
    }
    return offerBody(record.body);
  }

  function clearPending(storage, deviceId) {
    const key = storageKey(deviceId);
    try {
      storage.removeItem(key);
      if (storage.getItem(key) !== null) throw new Error("storage did not clear value");
    } catch (_) {
      throw guidance("The server accepted this folder offer, but the browser could not clear its retry copy. Retry the same saved offer before creating another one.");
    }
  }

  function coordinator(options) {
    if (!options || typeof options.api !== "function" || !options.storage) {
      throw new TypeError("folder coordinator requires api and session storage");
    }
    const onStatus = typeof options.onStatus === "function" ? options.onStatus : () => {};
    const onLockChange = typeof options.onLockChange === "function" ? options.onLockChange : () => {};
    let deviceId = null;
    let unlocked = false;
    let pollingEnabled = false;
    let mutationLocked = false;
    let generation = 0;
    let currentStatus = null;

    function setAccess(value) {
      const next = object(value, "Folder access context is invalid.");
      const nextDeviceId = next.deviceId === null ? null : uuid(next.deviceId, "The server identity is invalid.");
      generation += 1;
      currentStatus = null;
      deviceId = nextDeviceId;
      unlocked = next.unlocked === true && deviceId !== null;
    }

    function setPollingEnabled(enabled) {
      pollingEnabled = enabled === true;
      if (!pollingEnabled) generation += 1;
    }

    async function refresh() {
      if (!unlocked || !pollingEnabled || mutationLocked) return Object.freeze({ applied: false, status: null });
      const requestGeneration = ++generation;
      const decoded = requireStatus(await options.api("/api/v1/sync/status"));
      if (requestGeneration !== generation || !unlocked || !pollingEnabled || mutationLocked) {
        return Object.freeze({ applied: false, status: decoded });
      }
      currentStatus = decoded;
      onStatus(decoded);
      return Object.freeze({ applied: true, status: decoded });
    }

    async function mutate(
      path,
      body,
      expectedDeviceId = deviceId,
      staleMessage = "This folder change finished for a previous server. Its result was not applied; refresh the current server before continuing.",
    ) {
      if (!unlocked || deviceId === null) throw guidance("Unlock this console before changing folder sync.");
      if (expectedDeviceId === null || deviceId !== expectedDeviceId) {
        throw guidance("The folder server changed before this request started. Refresh folders before trying again.");
      }
      if (mutationLocked) throw guidance("Another folder change is still in progress.");
      mutationLocked = true;
      generation += 1;
      onLockChange(true);
      try {
        const result = mutationResponse(await options.api(path, { method: "POST", body: JSON.stringify(body) }));
        if (!unlocked || deviceId !== expectedDeviceId) {
          throw guidance(staleMessage);
        }
        return result;
      } finally {
        mutationLocked = false;
        onLockChange(false);
      }
    }

    async function sendOffer(body) {
      if (!unlocked || deviceId === null) throw guidance("Unlock this console before sharing a folder.");
      const offerDeviceId = deviceId;
      const existing = loadPending(options.storage, offerDeviceId);
      const candidate = offerBody(body);
      if (existing !== null && JSON.stringify(existing) !== JSON.stringify(candidate)) {
        throw guidance("Retry or clear the saved folder offer before creating another one.");
      }
      const retained = existing ?? savePending(options.storage, offerDeviceId, candidate);
      const staleMessage = "This offer finished for a previous server. Its exact retry copy was retained.";
      const result = await mutate("/api/v1/sync/folders", retained, offerDeviceId, staleMessage);
      if (!unlocked || deviceId !== offerDeviceId) {
        throw guidance("This offer finished for a previous server. Its exact retry copy was retained.");
      }
      clearPending(options.storage, offerDeviceId);
      return result;
    }

    async function retryPendingOffer() {
      if (!unlocked || deviceId === null) throw guidance("Unlock this console before retrying a folder offer.");
      const pending = loadPending(options.storage, deviceId);
      if (pending === null) throw guidance("There is no saved folder offer to retry.");
      return sendOffer(pending);
    }

    async function accept(offerId, rootPath) {
      const id = uuid(offerId);
      const share = currentStatus?.shares.find((item) => item.offerId === id);
      if (!share || !share.incoming || share.phase !== "offered") {
        throw guidance("That incoming folder offer is no longer available. Refresh folders first.");
      }
      if (share.expired) throw guidance("That folder invitation has expired and cannot be accepted.");
      return mutate("/api/v1/sync/accept", { offerId: id, selectedRoot: selectedRoot(rootPath) });
    }

    function pause(offerId, paused) {
      if (typeof paused !== "boolean") throw guidance("The folder pause request is invalid.");
      return mutate("/api/v1/sync/pause", { offerId: uuid(offerId), paused });
    }

    function remove(offerId) {
      return mutate("/api/v1/sync/remove", { offerId: uuid(offerId) });
    }

    function retryService() {
      return mutate("/api/v1/sync/retry", {});
    }

    function discardPendingOffer() {
      if (!unlocked || deviceId === null) throw guidance("Unlock this console before clearing a saved folder offer.");
      clearPending(options.storage, deviceId);
    }

    return Object.freeze({
      accept,
      current: () => currentStatus,
      discardPendingOffer,
      isMutationLocked: () => mutationLocked,
      loadPending: () => deviceId === null ? null : loadPending(options.storage, deviceId),
      pause,
      refresh,
      remove,
      retryPendingOffer,
      retryService,
      sendOffer,
      setAccess,
      setPollingEnabled,
    });
  }

  return Object.freeze({
    MAX_COLLECTION,
    MAX_RESPONSE_BYTES,
    coordinator,
    guidance,
    lazySessionStorage,
    offerBody,
    pending: Object.freeze({ clear: clearPending, load: loadPending, save: savePending, storageKey }),
    requireStatus,
    readJson,
    shareView,
    statusSummary,
  });
});
