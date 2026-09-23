// The console's single translator from a failure to words a person can act on.
//
// It is the web counterpart of NodeErrorCopy.swift and NodeFailure.kt, and it
// follows the same three rules those files follow:
//
//   * the headline is always copy written here, never a string that arrived
//     from the network, from Foundation, or from the browser's fetch stack;
//   * every mapped case names a recovery action, so the sentence tells someone
//     what to do next rather than what went wrong internally;
//   * the technical text still exists, but only as `detail`, which the console
//     renders inside a collapsed "Technical details" disclosure.
//
// It is exposed on globalThis for the console and on module.exports so the
// tests can hold it to those rules without a DOM.
(function exposeNodeErrorCopy(scope) {
  "use strict";

  const SUPPORTED_PROTOCOL_VERSION = 1;

  // Recovery names are the same set Apple's RecoveryHint uses, so the three
  // clients agree on what the next step is called for a given failure.
  const RECOVERY = Object.freeze({
    none: "none",
    retry: "retry",
    reconnect: "reconnect",
    checkNetworkSettings: "checkNetworkSettings",
    chooseAnotherDevice: "chooseAnotherDevice",
    chooseFolderAgain: "chooseFolderAgain",
    previewRestoreAgain: "previewRestoreAgain",
    freeUpSpace: "freeUpSpace",
  });

  // Machine-readable engine codes. Extracted from the node's ApiError
  // constructors in crates/covalent-node; the coverage test in
  // tests/node-error-copy.test.mjs holds this table to that list.
  const CATALOG = Object.freeze({
    // Authorization. The console is unlocked per browser tab, so "reconnect"
    // means entering the local access token again rather than re-pairing.
    authentication_required: [
      "This console is no longer unlocked. Enter the local access token again to continue.",
      RECOVERY.reconnect,
    ],
    not_authorized: [
      "Your backup server refused this request. Unlock the console again with a current local access token.",
      RECOVERY.reconnect,
    ],
    invalid_certificate: [
      "Covalent doesn't trust this server's security certificate. Enroll the node's certificate authority in this browser, then reload.",
      RECOVERY.reconnect,
    ],

    // Capacity
    insufficient_storage: [
      "Your backup server is out of space. Free some up, or choose a different device to keep this copy.",
      RECOVERY.freeUpSpace,
    ],
    resource_limit: [
      "This is larger than your backup server can handle in one pass. Try backing up a smaller folder.",
      RECOVERY.none,
    ],

    // Job lifecycle
    job_paused: ["This job is paused. Resume it to carry on where it left off.", RECOVERY.none],
    job_cancelled: ["This job was cancelled, and the progress it had saved was discarded.", RECOVERY.none],
    job_active: ["Another job is already running. Wait for it to finish, then try again.", RECOVERY.retry],
    job_conflict: ["This clashes with a job already running. Wait for it to finish, then try again.", RECOVERY.retry],
    job_not_complete: ["This job hasn't finished yet. Wait for it to complete.", RECOVERY.none],
    job_not_found: ["Covalent couldn't find this job. It may have already finished.", RECOVERY.none],
    invalid_job_id: ["Covalent couldn't read this job's identifier. Start the operation again.", RECOVERY.retry],
    node_busy: ["Your backup server is busy with something else. Try again in a moment.", RECOVERY.retry],
    node_state_locked: ["Your backup server is applying another change. Try again in a moment.", RECOVERY.retry],
    archive_processing_timeout: [
      "Your backup server took too long to work through this backup. Try again, or choose a smaller folder.",
      RECOVERY.retry,
    ],
    archive_processing_too_slow: [
      "This transfer was running too slowly to continue safely. Check the network between the two devices, then try again.",
      RECOVERY.retry,
    ],
    confirmation_required: ["This has to be confirmed on the other device before it can finish.", RECOVERY.none],
    recovery_confirmation_required: ["Confirm that you want to create recovery files or retry recovery, then continue.", RECOVERY.none],
    recovery_not_configured: ["This backup server was set up normally. To replace a lost device, start a new installation with its recovery files.", RECOVERY.none],

    // Source folder
    source_changed: ["Files changed while Covalent was copying them. Try again once they stop changing.", RECOVERY.retry],
    source_unreadable: [
      "Covalent couldn't read part of the folder you chose. Choose the folder again.",
      RECOVERY.chooseFolderAgain,
    ],
    invalid_authorized_root: [
      "The folder you chose is no longer available to Covalent. Choose it again.",
      RECOVERY.chooseFolderAgain,
    ],
    folder_sync_unavailable: [
      "Folder sync is unavailable on this server. Check its folder-sync package and mounted folder, then try again.",
      RECOVERY.retry,
    ],
    folder_sync_busy: ["Another folder change is still in progress. Try again shortly.", RECOVERY.retry],
    link_settings_conflict: ["Link settings changed on another device. Refresh and review the current settings before submitting again.", RECOVERY.none],
    link_settings_pending: ["A link-settings change is waiting for the source. Review that request before making another change.", RECOVERY.none],
    link_run_conflict: ["The link changed before this run started. Refresh link status and try again.", RECOVERY.none],
    link_run_pending: ["Another run request is waiting for the source. Refresh link status before trying again.", RECOVERY.none],
    folder_sync_needs_attention: [
      "Folder sync needs attention before it can continue. Check the folder status, then try again.",
      RECOVERY.retry,
    ],
    invalid_peer_address: [
      "Enter the device address as a numeric IP address and port, such as 192.168.1.20:8787, then try again.",
      RECOVERY.none,
    ],
    peer_address_changed: [
      "The saved device address changed before this update finished. Refresh the saved device, then try again.",
      RECOVERY.retry,
    ],
    peer_address_unreachable: [
      "Covalent could not authenticate the trusted device at the new address. Check the address and network connection, then try again.",
      RECOVERY.retry,
    ],

    // Restore
    unsafe_restore_path: [
      "This backup holds a file that would land outside the folder you chose, so Covalent stopped the restore to keep your files safe.",
      RECOVERY.none,
    ],
    restore_conflict: [
      "Some files already exist in the folder you're restoring into. Preview the restore again and choose how to handle them.",
      RECOVERY.previewRestoreAgain,
    ],
    restore_plan_mismatch: [
      "This restore changed after you previewed it. Preview it again before restoring.",
      RECOVERY.previewRestoreAgain,
    ],
    restore_plan_not_found: [
      "This restore preview has expired. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],
    invalid_restore_plan_id: [
      "Covalent couldn't read this restore plan. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],
    invalid_restore_execute_request: [
      "Covalent couldn't read this restore request. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],
    invalid_streamed_restore_plan: [
      "Covalent couldn't read the restore plan your server sent. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],

    // Restore target inventory
    invalid_target_inventory: [
      "Covalent couldn't finish checking the folder you're restoring into. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],
    target_inventory_required: [
      "Covalent needs to check the folder you're restoring into first. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],
    target_inventory_not_found: [
      "The check of your restore folder has expired. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],
    target_inventory_incomplete: [
      "Covalent didn't finish checking the folder you're restoring into. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],
    target_inventory_digest_mismatch: [
      "The folder you're restoring into changed while Covalent was checking it. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],
    target_inventory_job_mismatch: [
      "This restore check belongs to a different job. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],
    target_inventory_offset_mismatch: [
      "Covalent lost its place while checking your restore folder. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],
    target_inventory_page_mismatch: [
      "Covalent lost its place while checking your restore folder. Preview the restore again.",
      RECOVERY.previewRestoreAgain,
    ],

    // Backup contents
    backup_corrupt: [
      "Some of this backup's encrypted data is damaged. Verify the backup to see what can still be restored.",
      RECOVERY.none,
    ],
    backup_unavailable: ["This backup isn't available on your backup server right now.", RECOVERY.retry],
    invalid_archive: ["Covalent couldn't verify this backup's contents. Start the backup again.", RECOVERY.retry],
    invalid_archive_entry: [
      "Covalent couldn't verify one of the files in this backup. Start the backup again.",
      RECOVERY.retry,
    ],
    invalid_archive_metadata: ["Covalent couldn't verify this backup's details. Start the backup again.", RECOVERY.retry],
    archive_metadata_required: ["This backup arrived without its details. Start the backup again.", RECOVERY.retry],
    archive_upload_headers_required: ["This upload arrived incomplete. Start the backup again.", RECOVERY.retry],
    archive_digest_mismatch: [
      "The backup that arrived didn't match what this device sent. Start the backup again.",
      RECOVERY.retry,
    ],
    duplicate_archive_entry: ["This backup listed the same file twice. Start the backup again.", RECOVERY.retry],
    invalid_upload_digest: ["The upload didn't match what this device sent. Start the backup again.", RECOVERY.retry],
    invalid_upload_length: ["The upload lost its place. Start the backup again.", RECOVERY.retry],
    invalid_upload_offset: ["The upload lost its place. Start the backup again.", RECOVERY.retry],

    // Pairing
    invitation_unavailable: [
      "This pairing invitation has expired or was already used. Start pairing again.",
      RECOVERY.chooseAnotherDevice,
    ],
    protocol_incompatible: [
      "These two devices run versions of Covalent that can't work together. Update both, then try again.",
      RECOVERY.chooseAnotherDevice,
    ],
    pairing_endpoint_mismatch: [
      "That device answered from a different address than the one you paired with. Pair with it again.",
      RECOVERY.chooseAnotherDevice,
    ],
    pairing_endpoint_unavailable: [
      "This device has no address another device can reach it on yet. Set its peer address, then pair again.",
      RECOVERY.none,
    ],
    peer_endpoint_unavailable: [
      "This server doesn't know which address other devices should dial yet. Set the address other devices dial in its settings, then try again.",
      RECOVERY.none,
    ],
    claim_unavailable: [
      "This server already has an owner, so it can't be set up again.",
      RECOVERY.none,
    ],
    claim_code_incorrect: [
      "That setup code isn't correct. Check the code shown in your server's log and try again.",
      RECOVERY.none,
    ],
    claim_window_expired: [
      "That setup code has expired. Restart Covalent on your server to get a new one.",
      RECOVERY.none,
    ],
    claim_window_exhausted: [
      "Too many incorrect setup codes were entered. Restart Covalent on your server to get a new code.",
      RECOVERY.none,
    ],
    claim_rate_limited: [
      "Setup codes are being entered too quickly. Wait a moment, then try again.",
      RECOVERY.retry,
    ],
    claim_certificate_unavailable: [
      "This server is still preparing its security certificate. Wait a few seconds, then try again.",
      RECOVERY.retry,
    ],
    claim_state_unavailable: [
      "This server couldn't finish setting up safely. Retry the exact saved claim request; restart only if that remains unavailable.",
      RECOVERY.retry,
    ],
    pairing_peer_unreachable: [
      "Covalent couldn't reach that device. Check that it's switched on and on the same network, then pair again.",
      RECOVERY.chooseAnotherDevice,
    ],
    pairing_rejected: [
      "That device turned down the pairing request. Nothing was trusted.",
      RECOVERY.chooseAnotherDevice,
    ],
    provider_binding_mismatch: [
      "That device didn't match the identity it signed when you paired. Pair with it again.",
      RECOVERY.chooseAnotherDevice,
    ],
    invalid_provider_address: [
      "Covalent couldn't reach that device at the address given. Check the address, then try again.",
      RECOVERY.chooseAnotherDevice,
    ],

    // Request contract — this console and the node disagree
    invalid_contract: [
      "This console and your backup server don't agree on how to talk to each other. Update both to the same version.",
      RECOVERY.none,
    ],
    invalid_json: [
      "This console and your backup server don't agree on how to talk to each other. Update both to the same version.",
      RECOVERY.none,
    ],
    invalid_content_type: [
      "This console and your backup server don't agree on how to talk to each other. Update both to the same version.",
      RECOVERY.none,
    ],
    method_not_allowed: [
      "This console asked for something your backup server doesn't offer. Update both to the same version.",
      RECOVERY.none,
    ],
    route_not_found: [
      "This console asked for something your backup server doesn't offer. Update both to the same version.",
      RECOVERY.none,
    ],
    invalid_page_cursor: ["Covalent lost its place while loading this list. Try again.", RECOVERY.retry],
    invalid_page_limit: ["Covalent lost its place while loading this list. Try again.", RECOVERY.retry],
    internal_error: [
      "Something went wrong on your backup server. Try again; if it keeps happening, check its logs.",
      RECOVERY.retry,
    ],
  });

  // Status fallbacks for a code this build has never seen. Ordered so the
  // narrow cases win before the 5xx sweep.
  const STATUS = Object.freeze({
    unauthorized: [
      "Your backup server refused this request. Unlock the console again with a current local access token.",
      RECOVERY.reconnect,
    ],
    notFound: ["Covalent couldn't find that on your backup server.", RECOVERY.none],
    conflict: ["Something else changed on your backup server first. Reload the page, then try again.", RECOVERY.retry],
    busy: ["Your backup server is busy. Try again in a moment.", RECOVERY.retry],
    tooLarge: [
      "That's larger than your backup server accepts in one go. Try a smaller folder.",
      RECOVERY.none,
    ],
    rejected: [
      "Your backup server wouldn't accept what this console sent. Check the values in the form, then try again.",
      RECOVERY.none,
    ],
    outOfSpace: ["Your backup server is out of space.", RECOVERY.freeUpSpace],
    serverProblem: [
      "Something went wrong on your backup server. Try again; if it keeps happening, check its logs.",
      RECOVERY.retry,
    ],
    unavailable: [
      "Your backup server isn't ready to answer yet. Wait for it to finish starting, then try again.",
      RECOVERY.retry,
    ],
    retryable: ["Your backup server couldn't complete that request. Try again in a moment.", RECOVERY.retry],
    unknown: ["Your backup server couldn't complete that request.", RECOVERY.none],
  });

  // The browser cannot tell a stopped node from an untrusted certificate: fetch
  // reports both as one opaque TypeError. Rather than guess, the copy names
  // both causes and the one action that covers them.
  const TRANSPORT = Object.freeze({
    offline: [
      "This computer isn't connected to a network, so Covalent can't reach your backup server.",
      RECOVERY.checkNetworkSettings,
    ],
    timedOut: [
      "Your backup server didn't answer in time. Make sure it's turned on and awake, then try again.",
      RECOVERY.retry,
    ],
    unreachable: [
      "This browser couldn't reach your backup server. It may be stopped, or this browser may not trust its security certificate yet — enroll the node's certificate authority, then reload.",
      RECOVERY.reconnect,
    ],
    blocked: [
      "This browser blocked the request to your backup server. Open the console over its own HTTPS address rather than through another page.",
      RECOVERY.reconnect,
    ],
    unreadable: [
      "Covalent couldn't read that as JSON. Check that you pasted the whole thing, including its outer braces.",
      RECOVERY.none,
    ],
    protocol: [
      "This console and your backup server don't agree on how to talk to each other. Update both to the same version.",
      RECOVERY.none,
    ],
    unknown: ["Covalent couldn't finish that. Try again in a moment.", RECOVERY.retry],
  });

  // Every headline this presenter can produce on its own. A guidance error adds
  // its own authored sentence; nothing else may reach a person.
  const SUMMARIES = Object.freeze([
    ...Object.values(CATALOG).map((entry) => entry[0]),
    ...Object.values(STATUS).map((entry) => entry[0]),
    ...Object.values(TRANSPORT).map((entry) => entry[0]),
  ]);

  function failure(entry, detail) {
    return Object.freeze({ summary: entry[0], recovery: entry[1], detail: detail ?? null });
  }

  function detailText(value) {
    if (typeof value !== "string") return null;
    const collapsed = value.replace(/\s+/g, " ").trim();
    if (collapsed.length === 0) return null;
    return collapsed.length > 400 ? `${collapsed.slice(0, 399)}…` : collapsed;
  }

  function rawDetail(error) {
    if (error instanceof Error) return detailText(`${error.name}: ${error.message}`);
    try {
      return detailText(String(error));
    } catch {
      return null;
    }
  }

  function statusEntry(status, retryable) {
    if (status === 401 || status === 403) return STATUS.unauthorized;
    if (status === 404) return STATUS.notFound;
    if (status === 409) return STATUS.conflict;
    if (status === 408 || status === 429) return STATUS.busy;
    if (status === 413) return STATUS.tooLarge;
    if (status === 415 || status === 422) return STATUS.rejected;
    if (status === 507) return STATUS.outOfSpace;
    if (status === 503) return STATUS.unavailable;
    if (status >= 500 && status <= 599) return STATUS.serverProblem;
    return retryable === true ? STATUS.retryable : STATUS.unknown;
  }

  // The node's own `message` is curated and safe, but it is still text from off
  // this page, so it lands in `detail` next to the status and code rather than
  // in the headline.
  function describeApi(status, code, message, retryable) {
    const detail = detailText(`HTTP ${status} · ${code} · ${message ?? ""}`);
    const known = Object.hasOwn(CATALOG, code) ? CATALOG[code] : null;
    return failure(known ?? statusEntry(status, retryable), detail);
  }

  function describeProtocol(observedVersion) {
    return failure(
      TRANSPORT.protocol,
      detailText(`node protocol ${observedVersion} · this console speaks ${SUPPORTED_PROTOCOL_VERSION}`),
    );
  }

  function online(context) {
    if (typeof context.online === "boolean") return context.online;
    if (typeof navigator === "object" && navigator !== null && typeof navigator.onLine === "boolean") {
      return navigator.onLine;
    }
    return true;
  }

  function describeTransport(error, context = {}) {
    const name = error instanceof Error ? error.name : "";
    const detail = rawDetail(error);
    // Order matters: a browser that is offline reports the same TypeError as a
    // node that is switched off, and the offline signal is the one that is real.
    if (name === "AbortError" || name === "TimeoutError") return failure(TRANSPORT.timedOut, detail);
    if (name === "SecurityError") return failure(TRANSPORT.blocked, detail);
    if (name === "SyntaxError") return failure(TRANSPORT.unreadable, detail);
    if (name === "TypeError") {
      return failure(online(context) ? TRANSPORT.unreachable : TRANSPORT.offline, detail);
    }
    return failure(TRANSPORT.unknown, detail);
  }

  function describe(error, context = {}) {
    if (error !== null && typeof error === "object") {
      if (error.covalentFailureKind === "api") {
        return describeApi(error.status, error.code, error.serverMessage, error.retryable);
      }
      if (error.covalentFailureKind === "protocol") return describeProtocol(error.observedVersion);
      // A sentence written in this repository, marked as such by the module
      // that threw it. A runtime string can never carry this property.
      if (typeof error.covalentGuidance === "string" && error.covalentGuidance.length > 0) {
        return Object.freeze({ summary: error.covalentGuidance, recovery: RECOVERY.none, detail: null });
      }
    }
    return describeTransport(error, context);
  }

  async function initializeUnlockedConsole({ authorize, initializeLinks, refreshPairings, onLinksError }) {
    await authorize();
    try {
      await initializeLinks();
    } catch (error) {
      onLinksError(error);
    }
    await refreshPairings();
  }

  const nodeErrorCopy = Object.freeze({
    CATALOG,
    RECOVERY,
    SUMMARIES,
    SUPPORTED_PROTOCOL_VERSION,
    describe,
    describeApi,
    describeProtocol,
    describeTransport,
    initializeUnlockedConsole,
  });
  scope.CovalentNodeErrorCopy = nodeErrorCopy;
  if (typeof module === "object" && module.exports) module.exports = nodeErrorCopy;
}(typeof globalThis === "object" ? globalThis : window));

// The console itself only runs in a browser; under `node --test` this file is
// imported for the presenter above and stops here.
if (typeof document === "object" && document !== null) bootConsole();

function bootConsole() {
const $ = (selector) => document.querySelector(selector);
const PROTOCOL_VERSION = 1;
const message = $("[data-message]");
const messageDetails = $("[data-message-details]");
const messageDetail = $("[data-message-detail]");
let messageTimer = null;
let token = "";
let networkPairing = null;
let networkPoll = null;
const pairing = globalThis.CovalentPairingFlow;
const tabFlow = globalThis.CovalentTabFlow;
const errorCopy = globalThis.CovalentNodeErrorCopy;
const folderSync = globalThis.CovalentFolderSyncFlow;
let folderDeviceId = null;
const folderController = folderSync.coordinator({
  api: folderApi,
  storage: folderSync.lazySessionStorage(globalThis),
  onStatus: renderFolderStatus,
  onLockChange: setFolderMutationLock,
});

function folderApi(path, options) {
  return api(path, options, folderSync.readJson);
}

function formData(form) {
  return new FormData(form);
}

class NodeApiError extends Error {
  constructor(status, payload) {
    // The message this Error carries is a diagnostic label, not display copy.
    // Everything a person reads comes from CovalentNodeErrorCopy.
    super(`${payload.code || `http_${status}`} (HTTP ${status})`);
    this.name = "NodeApiError";
    this.covalentFailureKind = "api";
    this.status = status;
    this.code = payload.code || `http_${status}`;
    this.serverMessage = typeof payload.message === "string" ? payload.message : "";
    this.retryable = payload.retryable === true;
    this.protocolVersion = payload.protocolVersion;
  }
}

class ProtocolMismatchError extends Error {
  constructor(observedVersion) {
    super(`protocol_incompatible (${observedVersion})`);
    this.name = "ProtocolMismatchError";
    this.covalentFailureKind = "protocol";
    this.observedVersion = observedVersion;
  }
}

function say(text, isError = false, detail = null, transient = false) {
  if (messageTimer !== null) clearTimeout(messageTimer);
  message.textContent = text;
  message.classList.toggle("error", isError);
  messageDetail.textContent = detail ?? "";
  messageDetails.hidden = detail === null;
  messageDetails.open = false;
  messageTimer = !isError && transient ? setTimeout(() => {
    message.textContent = "";
    messageDetail.textContent = "";
    messageDetails.hidden = true;
    messageTimer = null;
  }, 5000) : null;
}

// The only path from a thrown value to the screen. It never reads `.message`.
function fail(error) {
  const failure = errorCopy.describe(error);
  console.debug("Covalent request failed", error);
  say(failure.summary, true, failure.detail);
  message.dataset.recovery = failure.recovery;
}

async function apiResponse(path, options = {}, readJson = (response) => response.json()) {
  const headers = new Headers(options.headers || {});
  headers.set("Accept", "application/json");
  if (token) headers.set("Authorization", `Bearer ${token}`);
  if (options.body) headers.set("Content-Type", "application/json");
  const response = await fetch(path, { ...options, headers, cache: "no-store" });
  if (!response.ok) {
    const decoded = await readJson(response).catch(() => ({}));
    const body = decoded && typeof decoded === "object" ? decoded : {};
    if (body.protocolVersion !== undefined && body.protocolVersion !== PROTOCOL_VERSION) {
      throw new ProtocolMismatchError(body.protocolVersion);
    }
    throw new NodeApiError(response.status, body);
  }
  return {
    status: response.status,
    body: response.status === 204 ? null : await readJson(response),
    headers: response.headers,
  };
}

async function api(path, options = {}, readJson) {
  return (await apiResponse(path, options, readJson)).body;
}

async function loadStatus() {
  try {
    const status = await api("/api/v1/status");
    if (status.protocolVersion !== PROTOCOL_VERSION) {
      throw new ProtocolMismatchError(status.protocolVersion);
    }
    $("[data-device-name]").textContent = status.deviceName;
    $("[data-node-state]").textContent = `Service: ${status.state}`;
    document.querySelectorAll("[data-discovery]").forEach((el) => { el.textContent = status.lanDiscovery ? "On" : "Off"; });
  } catch (error) {
    $("[data-device-name]").textContent = "Node unavailable";
    $("[data-node-state]").textContent = errorCopy.describe(error).summary;
    fail(error);
  }
}

function folderPollingEligible() {
  const foldersSelected = $("[data-tab=folders]").getAttribute("aria-selected") === "true";
  const pairSelected = $("[data-tab=pair]").getAttribute("aria-selected") === "true";
  return Boolean(
    token
    && folderDeviceId
    && document.visibilityState === "visible"
    && (foldersSelected || pairSelected),
  );
}

function syncFolderPolling(refreshNow = false) {
  const enabled = folderPollingEligible();
  folderController.setPollingEnabled(enabled);
  if (enabled && refreshNow) void loadFolders(false);
}

function renderFolderError(error) {
  const failure = errorCopy.describe(error);
  const status = $("[data-folders-status]");
  status.textContent = failure.summary;
  status.className = "folder-state";
  status.dataset.kind = "attention";
}

const folderMutationDisabledState = new WeakMap();

function setFolderMutationLock(locked) {
  document.querySelectorAll("[data-folder-mutation]").forEach((control) => {
    if (locked) {
      folderMutationDisabledState.set(control, control.disabled);
      control.disabled = true;
    } else if (folderMutationDisabledState.has(control)) {
      control.disabled = folderMutationDisabledState.get(control);
      folderMutationDisabledState.delete(control);
    }
  });
  $("[data-folder-offer-form]").setAttribute("aria-busy", String(locked));
}

function folderPeerName(status, peerId) {
  return status.peers.find((peer) => peer.peerId === peerId)?.displayName
    ?? "Confirmed paired device unavailable";
}

function folderActionButton(label, action, className = "secondary") {
  const button = document.createElement("button");
  button.type = "button";
  button.className = className;
  button.dataset.folderMutation = "";
  button.textContent = label;
  button.disabled = folderController.isMutationLocked();
  button.addEventListener("click", action);
  return button;
}

async function runFolderMutation(action, success) {
  try {
    await action();
    renderPendingFolderOffer();
    renderPendingLinkSettings();
    renderPendingLinkRun();
    await loadFolders(false);
    say(success);
  } catch (error) {
    renderPendingFolderOffer();
    renderPendingLinkSettings();
    renderPendingLinkRun();
    fail(error);
  }
}

function firstLinkMember(status, share) {
  return status.shares.find((item) => item.folderId === share.folderId
    && item.linkSettings !== null && item.phase !== "removed")?.offerId === share.offerId;
}

function cadenceControls(settings) {
  const cadenceLabel = document.createElement("label");
  cadenceLabel.textContent = "Transfer timing ";
  const cadence = document.createElement("select");
  cadence.name = "cadenceMode";
  for (const [value, label] of [["manual", "Manual"], ["scheduled", "Scheduled"], ["continuous", "Continuous"]]) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = label;
    cadence.append(option);
  }
  cadence.value = settings.cadence.mode;
  cadenceLabel.append(cadence);

  const intervalLabel = document.createElement("label");
  intervalLabel.textContent = "Run every ";
  const interval = document.createElement("input");
  interval.name = "intervalMinutes";
  interval.type = "number";
  interval.min = "15";
  interval.max = "525600";
  interval.step = "1";
  interval.setAttribute("list", "folder-schedule-options");
  interval.required = true;
  interval.value = String(settings.cadence.mode === "scheduled" ? settings.cadence.intervalMinutes : 60);
  intervalLabel.append(interval, " minutes");
  const syncInterval = () => {
    intervalLabel.hidden = cadence.value !== "scheduled";
    interval.disabled = intervalLabel.hidden;
  };
  cadence.addEventListener("change", syncInterval);
  syncInterval();
  return { cadenceLabel, cadence, intervalLabel, interval };
}

function cadenceValue(mode, interval) {
  return mode === "scheduled"
    ? { mode, intervalMinutes: Number(interval) }
    : { mode };
}

function syncOfferScheduleInput() {
  const form = $("[data-folder-offer-form]");
  const scheduled = form.elements.cadenceMode.value === "scheduled";
  const label = $("[data-folder-schedule-interval]");
  label.hidden = !scheduled;
  form.elements.intervalMinutes.disabled = !scheduled;
}

function runResultCopy(result) {
  return ({
    pending: "Waiting",
    succeeded: "Complete",
    failed: "Failed",
    timedOut: "Incomplete after 24 hours",
    interrupted: "Interrupted",
    cancelled: "Cancelled",
  })[result];
}

function runDestinationName(status, peerId) {
  if (peerId === folderDeviceId) return "This device";
  return status.peers.find((peer) => peer.peerId === peerId)?.displayName ?? "Another destination";
}

function renderLinkRun(container, status, share) {
  const run = share.linkRun;
  if (run === null || !share.linkSettings.confirmed) return;
  const section = document.createElement("section");
  section.className = "link-run";
  const heading = document.createElement("strong");
  heading.textContent = "Transfer";
  const summary = document.createElement("p");
  summary.setAttribute("role", "status");
  if (run.pendingRequest !== null) summary.textContent = "Run requested. Waiting for the source to come online and start it.";
  else if (run.phase === "preparing") summary.textContent = "Preparing this run and checking source files.";
  else if (run.phase === "running") summary.textContent = "This run is active.";
  else if (run.phase === "succeeded") summary.textContent = "The last run completed for every destination.";
  else if (run.phase === "incomplete") summary.textContent = "The last run was incomplete. Review each destination and run it again when ready.";
  else if (run.phase === "interrupted") summary.textContent = "The last run was interrupted before completion. Run it again when ready.";
  else if (run.phase === "cancelled") summary.textContent = "The last run was cancelled before completion.";
  else if (share.linkSettings.settings.cadence.mode === "manual") {
    summary.textContent = "Ready. No transfer starts until a link member chooses Run Now.";
  } else if (share.linkSettings.settings.cadence.mode === "continuous") {
    summary.textContent = "Continuous transfer is ready.";
  } else summary.textContent = "Scheduled transfer is ready.";
  section.append(heading, summary);

  if (share.linkSettings.settings.cadence.mode === "scheduled") {
    const schedule = document.createElement("p");
    schedule.className = "muted";
    schedule.textContent = run.nextDueAtUnixMs === null
      ? "The source owns this schedule. The next run time is not available yet."
      : `The source owns this schedule. Next run: ${new Date(run.nextDueAtUnixMs).toLocaleString()}.`;
    section.append(schedule);
  }
  if (run.destinations.length > 0) {
    const destinations = document.createElement("ul");
    for (const destination of run.destinations) {
      const item = document.createElement("li");
      item.textContent = `${runDestinationName(status, destination.peerId)}: ${runResultCopy(destination.result)}`;
      destinations.append(item);
    }
    section.append(destinations);
  }
  if (run.rejectedRequest !== null) {
    const rejected = document.createElement("p");
    rejected.setAttribute("role", "alert");
    rejected.textContent = "A Run Now request was not started because the link changed. Refresh and try again.";
    section.append(rejected);
  }
  // Continuous links start automatically; the source rejects explicit runs.
  if (share.linkSettings.settings.cadence.mode === "continuous") {
    container.append(section);
    return;
  }
  const active = run.pendingRequest !== null || ["preparing", "running"].includes(run.phase);
  let saved = null;
  try { saved = folderController.pendingRun(); }
  catch (error) { renderFolderError(error); }
  const button = folderActionButton(saved?.folderId === share.folderId ? "Retry Run Now" : "Run Now", () => {
    void runFolderMutation(
      () => saved?.folderId === share.folderId ? folderController.retryPendingRun() : folderController.runNow(share.folderId),
      "Run request saved. Link status shows progress for every destination.",
    );
  });
  button.disabled = button.disabled || active || share.linkSettings.settings.paused
    || (saved !== null && saved.folderId !== share.folderId);
  section.append(button);
  container.append(section);
}

const expandedLinkSettings = new Set();

function renderLinkSettings(container, status, share) {
  const state = share.linkSettings;
  const card = $("#link-settings-card").content.firstElementChild.cloneNode(true);
  const details = card.querySelector('[data-slot="card-content"]');
  card.id = `link-settings-${share.offerId}`;
  card.hidden = !expandedLinkSettings.has(share.folderId);
  const settingsButton = document.createElement("button");
  settingsButton.type = "button";
  settingsButton.className = "secondary link-settings-button";
  settingsButton.textContent = "Settings";
  settingsButton.setAttribute("aria-label", `Settings for ${share.label}`);
  settingsButton.setAttribute("aria-controls", card.id);
  settingsButton.setAttribute("aria-expanded", String(!card.hidden));
  settingsButton.addEventListener("click", () => {
    card.hidden = !card.hidden;
    settingsButton.setAttribute("aria-expanded", String(!card.hidden));
    if (card.hidden) expandedLinkSettings.delete(share.folderId);
    else expandedLinkSettings.add(share.folderId);
  });
  if (!state.confirmed) {
    const current = document.createElement("p");
    current.className = "notice";
    current.textContent = "Waiting for the source to confirm current settings. File transfer has not started.";
    details.append(current);
  }

  if (state.pendingChange !== null) {
    const pending = document.createElement("p");
    pending.setAttribute("role", "status");
    pending.textContent = `Waiting for the source to confirm this request: ${folderSync.settingsExplanation(state.pendingChange.settings)}`;
    details.append(pending);
  }
  if (state.conflictedChange !== null) {
    const conflict = document.createElement("p");
    conflict.setAttribute("role", "alert");
    conflict.textContent = `An outdated request was not applied: ${folderSync.settingsExplanation(state.conflictedChange.settings)} Review the current choices below before submitting again.`;
    details.append(conflict);
  }

  const form = document.createElement("form");
  form.className = "inline-form";
  const sourceDeletes = document.createElement("label");
  const sourceDeletesInput = document.createElement("input");
  sourceDeletesInput.type = "checkbox";
  sourceDeletesInput.name = "propagateSourceDeletions";
  sourceDeletesInput.checked = state.settings.deletionPolicy.propagateSourceDeletions;
  sourceDeletes.append(sourceDeletesInput, " Delete destination copies when source files are deleted");
  const localDeletes = document.createElement("label");
  const localDeletesInput = document.createElement("input");
  localDeletesInput.type = "checkbox";
  localDeletesInput.name = "restoreLocalDeletions";
  localDeletesInput.checked = state.settings.deletionPolicy.restoreLocalDeletions;
  localDeletes.append(localDeletesInput, " Restore files deleted at a destination");
  const timing = cadenceControls(state.settings);
  const wifi = document.createElement("label");
  const wifiInput = document.createElement("input");
  wifiInput.type = "checkbox";
  wifiInput.name = "wifiOnly";
  wifiInput.checked = state.settings.androidConditions.wifiOnly;
  wifi.append(wifiInput, " Wi-Fi only on Android devices");
  const charging = document.createElement("label");
  const chargingInput = document.createElement("input");
  chargingInput.type = "checkbox";
  chargingInput.name = "chargingOnly";
  chargingInput.checked = state.settings.androidConditions.chargingOnly;
  charging.append(chargingInput, " Charging only on Android devices");
  const androidHelp = document.createElement("p");
  androidHelp.className = "muted";
  androidHelp.textContent = "These shared conditions apply only on Android devices in this link.";
  const submit = document.createElement("button");
  submit.dataset.folderMutation = "";
  submit.disabled = folderController.isMutationLocked() || !state.confirmed || state.pendingChange !== null;
  submit.textContent = state.conflictedChange === null ? "Save link settings" : "Submit reviewed settings";
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const policy = {
      propagateSourceDeletions: sourceDeletesInput.checked,
      restoreLocalDeletions: localDeletesInput.checked,
    };
    const settings = {
      deletionPolicy: policy,
      paused: state.settings.paused,
      cadence: cadenceValue(timing.cadence.value, timing.interval.value),
      androidConditions: { wifiOnly: wifiInput.checked, chargingOnly: chargingInput.checked },
    };
    const currentPolicy = state.settings.deletionPolicy;
    const enablesDeletion = policy.propagateSourceDeletions && !currentPolicy.propagateSourceDeletions
      || policy.restoreLocalDeletions && !currentPolicy.restoreLocalDeletions;
    if (enablesDeletion && !globalThis.confirm(`${folderSync.policyExplanation(policy)} Apply these choices to every destination in this link?`)) return;
    void runFolderMutation(
      () => folderController.updateLinkSettings(share.folderId, settings),
      "Link settings request saved. Current link status shows whether the source confirmed it.",
    );
  });
  const group = (title, ...controls) => {
    const fieldset = document.createElement("fieldset");
    const legend = document.createElement("legend");
    legend.textContent = title;
    fieldset.append(legend, ...controls);
    return fieldset;
  };
  const deletionHelp = document.createElement("p");
  deletionHelp.className = "muted";
  deletionHelp.textContent = "With both options off, deleting a source file keeps the destination copy. Deleting a destination file does not download it again.";
  const android = document.createElement("details");
  android.className = "android-conditions";
  const androidSummary = document.createElement("summary");
  androidSummary.textContent = "Android conditions";
  android.append(androidSummary, androidHelp, wifi, charging);
  form.append(group("Timing", timing.cadenceLabel, timing.intervalLabel),
    group("Deleted files", sourceDeletes, localDeletes, deletionHelp), android, submit);
  details.append(form);
  container.append(settingsButton, card);
}

function renderAddDestination(container, status, share) {
  const memberIds = new Set(status.shares
    .filter((item) => item.folderId === share.folderId && item.phase !== "removed")
    .map((item) => item.peerId));
  const peers = status.peers.filter((peer) => !memberIds.has(peer.peerId));
  const details = document.createElement("details");
  details.className = "advanced";
  const summary = document.createElement("summary");
  summary.textContent = "Add destination";
  const explanation = document.createElement("p");
  explanation.className = "muted";
  explanation.textContent = "Enter the same source path used by this link. The server verifies the existing folder identity before adding a destination.";
  details.append(summary, explanation);
  if (peers.length === 0) {
    const empty = document.createElement("p");
    empty.textContent = "Every paired device is already a destination for this link.";
    details.append(empty);
    container.append(details);
    return;
  }
  const form = document.createElement("form");
  form.className = "inline-form";
  const peerLabel = document.createElement("label");
  peerLabel.textContent = "Destination device ";
  const select = document.createElement("select");
  select.name = "peerId";
  select.required = true;
  const placeholder = document.createElement("option");
  placeholder.value = "";
  placeholder.textContent = "Choose a confirmed paired device";
  select.append(placeholder);
  for (const peer of peers) {
    const option = document.createElement("option");
    option.value = peer.peerId;
    option.textContent = peer.displayName;
    select.append(option);
  }
  peerLabel.append(select);
  const pathLabel = document.createElement("label");
  pathLabel.textContent = "Existing source path on this server ";
  const path = document.createElement("input");
  path.name = "selectedRoot";
  path.maxLength = 1024;
  path.required = true;
  path.placeholder = "/sync";
  pathLabel.append(path);
  const submit = document.createElement("button");
  submit.dataset.folderMutation = "";
  submit.textContent = "Send destination invitation";
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const body = {
      peerId: select.value,
      folderId: share.folderId,
      label: share.label,
      selectedRoot: path.value,
      linkPolicy: share.linkPolicy,
      cadence: share.linkSettings.settings.cadence,
      androidConditions: share.linkSettings.settings.androidConditions,
    };
    if (!globalThis.confirm(`Confirm that ${path.value} is the same source folder already used by ${share.label}. Add this destination?`)) return;
    void runFolderMutation(
      () => folderController.sendOffer(body),
      "Destination invitation sent with this link's existing identity and settings.",
    );
  });
  form.append(peerLabel, pathLabel, submit);
  details.append(form);
  container.append(details);
}

function renderFolderActions(container, status, share, view) {
  if (share.phase === "removed") return;
  if (share.incoming && share.phase === "offered" && !share.expired) {
    const pathLabel = document.createElement("label");
    pathLabel.textContent = "Folder path on this server";
    const path = document.createElement("input");
    path.value = "/sync";
    path.maxLength = 1024;
    path.required = true;
    pathLabel.append(path);
    const accept = folderActionButton("Accept folder", () => {
      void runFolderMutation(
        () => folderController.accept(share.offerId, path.value),
        "Folder accepted. Covalent is checking its local contents.",
      );
    }, "");
    container.append(pathLabel, accept);
  }

  const firstMember = share.linkSettings === null || firstLinkMember(status, share);
  if (firstMember && !share.expired && !(share.incoming && share.phase === "offered")) {
    const paused = share.phase === "paused";
    const pause = folderActionButton(share.linkPolicy === null
      ? (paused ? "Resume" : "Pause") : (paused ? "Resume link" : "Pause link"), () => {
      void runFolderMutation(
        () => folderController.pause(share.offerId, !paused),
        share.linkPolicy === null ? (paused ? "Folder sync resumed." : "Folder sync paused.")
          : "Link pause change saved. It applies to all devices when the source confirms it.",
      );
    });
    container.append(pause);
  }

  if (share.linkSettings !== null && firstMember) {
    renderLinkRun(container, status, share);
    renderLinkSettings(container, status, share);
    if (!share.incoming && share.linkSettings.confirmed && ["ready", "paused"].includes(share.phase)) {
      renderAddDestination(container, status, share);
    }
  }

  if (share.expired && !share.incoming && ["offered", "paused"].includes(share.phase)) {
    container.append(folderActionButton("Send new invitation", () => {
      void runFolderMutation(
        () => folderController.renew(share.offerId),
        "New invitation created. Accept it on the other device to start syncing.",
      );
    }));
  }

  const removeLabel = share.incoming && share.phase === "offered" && !share.expired ? "Decline" : "Remove…";
  const confirmation = document.createElement("div");
  confirmation.className = "folder-removal-confirmation";
  confirmation.hidden = true;
  confirmation.setAttribute("role", "group");
  confirmation.setAttribute("aria-label", `Stop sharing ${share.label}`);
  const explanation = document.createElement("p");
  explanation.textContent = "Stop syncing this folder? Sync stops here now and on the other device when it reconnects. Files stay on both devices.";
  const remove = folderActionButton(removeLabel, () => {
    confirmation.hidden = false;
    cancel.focus();
  }, "quiet");
  const cancel = folderActionButton("Keep sharing", () => {
    confirmation.hidden = true;
    remove.focus();
  }, "quiet");
  const confirmed = folderActionButton("Stop sharing", () => {
    void runFolderMutation(
      () => folderController.remove(share.offerId),
      "Sync stopped here. The other device will stop when it reconnects. Files stay on both devices.",
    );
  }, "quiet");
  confirmation.append(explanation, cancel, confirmed);
  container.append(remove, confirmation);

  if (view.kind === "expired") {
    const guidance = document.createElement("p");
    guidance.textContent = share.incoming
      ? `Ask ${folderPeerName(status, share.peerId)} to send a new invitation, then choose your folder again.`
      : "Send a new invitation so the other device can choose its folder and accept again.";
    container.append(guidance);
  }
}

function renderFolderPeers(status) {
  const select = $("[data-folder-peer]");
  const previous = select.value;
  const labels = status.peers.map((peer) => [peer.peerId, peer.displayName]);
  const rendered = Array.from(select.options).slice(1).map((option) => [option.value, option.textContent]);
  const placeholderText = status.peers.length === 0
    ? "No confirmed paired devices"
    : "Choose a confirmed paired device";
  if (select.options[0]?.textContent === placeholderText && JSON.stringify(labels) === JSON.stringify(rendered)) {
    select.disabled = status.peers.length === 0;
    return;
  }
  select.replaceChildren();
  const placeholder = document.createElement("option");
  placeholder.value = "";
  placeholder.textContent = placeholderText;
  select.append(placeholder);
  for (const peer of status.peers) {
    const option = document.createElement("option");
    option.value = peer.peerId;
    // Peer-controlled labels are always assigned as text, never parsed as markup.
    option.textContent = peer.displayName;
    select.append(option);
  }
  select.value = status.peers.some((peer) => peer.peerId === previous) ? previous : "";
  select.disabled = status.peers.length === 0;
}

function pairedDeviceItem(peer) {
  const item = document.createElement("li");
  item.dataset.pairedPeerId = peer.peerId;
  const details = document.createElement("div");
  const name = document.createElement("strong");
  const address = document.createElement("span");
  details.append(name, address);

  const update = folderActionButton("Update address", () => {
    const current = folderController.current()?.peers.find((entry) => entry.peerId === peer.peerId);
    if (!current || current.address === null) return;
    const pending = folderController.pendingPeerAddressRefresh();
    if (form.hidden) {
      input.value = pending?.peerId === peer.peerId ? pending.candidateAddress : current.address;
    }
    form.hidden = false;
    update.setAttribute("aria-expanded", "true");
    input.focus();
  });
  update.setAttribute("aria-expanded", "false");

  const form = document.createElement("form");
  form.className = "inline-form";
  form.hidden = true;
  form.id = `paired-address-form-${peer.peerId}`;
  update.setAttribute("aria-controls", form.id);
  const inputId = `paired-address-${peer.peerId}`;
  const label = document.createElement("label");
  label.htmlFor = inputId;
  label.textContent = "New numeric IP address and port";
  const input = document.createElement("input");
  input.id = inputId;
  input.name = "candidateAddress";
  input.maxLength = 128;
  input.placeholder = "192.0.2.10:8787 or [2001:db8::10]:8787";
  input.autocomplete = "off";
  input.spellcheck = false;
  input.required = true;
  label.append(input);
  const explanation = document.createElement("p");
  explanation.className = "muted";
  explanation.textContent = "Covalent will contact this address and require the same paired identity and certificate before saving it.";
  const state = document.createElement("p");
  state.className = "muted";
  state.setAttribute("role", "status");
  state.setAttribute("aria-live", "polite");
  const submit = document.createElement("button");
  submit.dataset.folderMutation = "";
  submit.disabled = folderController.isMutationLocked();
  submit.textContent = "Verify and save";
  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.className = "quiet";
  cancel.dataset.folderMutation = "";
  cancel.disabled = folderController.isMutationLocked();
  cancel.textContent = "Cancel";
  cancel.addEventListener("click", () => {
    folderController.cancelPeerAddressRefresh(peer.peerId);
    form.hidden = true;
    update.setAttribute("aria-expanded", "false");
    state.textContent = "";
    update.focus();
  });
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!requireUnlocked()) return;
    state.textContent = folderController.pendingPeerAddressRefresh() === null
      ? "Verifying this address with the paired device…"
      : "Retrying the exact saved address request…";
    input.disabled = true;
    try {
      await folderController.refreshPeerAddress(peer.peerId, input.value);
      state.textContent = "Address verified and saved.";
      submit.textContent = "Verify and save";
      form.hidden = true;
      update.setAttribute("aria-expanded", "false");
      update.focus();
      say("Address verified and saved for this paired device.");
    } catch (error) {
      if (folderController.pendingPeerAddressRefresh()?.peerId === peer.peerId) {
        submit.textContent = "Retry saved update";
        state.textContent = "The update is not confirmed. Retry this exact address, or cancel it before entering another one.";
      } else if (/saved address changed/.test(error?.covalentGuidance ?? "")) {
        state.textContent = "The saved address changed. Review the current address before submitting again.";
      } else {
        state.textContent = "The update is not confirmed. Refresh paired devices before trying again.";
      }
      fail(error);
    } finally {
      input.disabled = false;
    }
  });
  form.append(label, explanation, state, submit, cancel);
  item.append(details, update, form);
  return item;
}

function renderPairedDevices(status) {
  const list = $("[data-paired-devices]");
  const retained = new Map(Array.from(list.children).map((item) => [item.dataset.pairedPeerId, item]));
  const visible = new Set(status.peers.map((peer) => peer.peerId));
  for (const [id, item] of retained) {
    if (!visible.has(id)) item.remove();
  }
  for (const [index, peer] of status.peers.entries()) {
    let item = retained.get(peer.peerId);
    if (!item) item = pairedDeviceItem(peer);
    const [details, update, form] = item.children;
    const [name, address] = details.children;
    name.textContent = peer.displayName;
    address.textContent = peer.address === null
      ? " — Address unavailable from this server version"
      : ` — ${peer.address}`;
    update.hidden = peer.address === null;
    if (peer.address === null) {
      form.hidden = true;
      update.setAttribute("aria-expanded", "false");
    }
    if (list.children[index] !== item) list.insertBefore(item, list.children[index] ?? null);
  }
  const empty = $("[data-paired-devices-empty]");
  empty.hidden = status.peers.length > 0;
  if (status.peers.length === 0) empty.textContent = "No confirmed paired devices are available.";
}

function renderFolderStatus(status) {
  const summary = folderSync.statusSummary(status);
  const statusCopy = $("[data-folders-status]");
  statusCopy.textContent = summary.text;
  statusCopy.className = "folder-state";
  statusCopy.dataset.kind = summary.kind;
  renderFolderPeers(status);
  renderPairedDevices(status);
  $("[data-peer-count]").textContent = String(status.peers.length);
  $("[data-create-guidance]").hidden = status.peers.length > 0;

  const retry = $("[data-folders-retry-service]");
  retry.hidden = !(status.lifecycle === "needsAttention" || status.issue !== null);
  const list = $("[data-folders-list]");
  const visibleShares = status.shares.filter((share) => share.phase !== "removed" || share.remoteRemovalPending);
  $("[data-link-count]").textContent = String(new Set(visibleShares.map((share) => share.folderId)).size);
  let savedSettings = null;
  try { savedSettings = folderController.pendingLinkSettings(); }
  catch (error) { renderFolderError(error); }
  const retained = new Map(Array.from(list.children).map((item) => [item.dataset.folderOfferId, item]));
  const visibleIds = new Set(visibleShares.map((share) => share.offerId));
  for (const [id, item] of retained) {
    if (!visibleIds.has(id)) item.remove();
  }
  for (const [index, share] of visibleShares.entries()) {
    const view = folderSync.shareView(status, share);
    let item = retained.get(share.offerId);
    if (!item) {
      item = document.createElement("li");
      item.dataset.folderOfferId = share.offerId;
      const details = document.createElement("div");
      const name = document.createElement("strong");
      const peer = document.createElement("span");
      const state = document.createElement("span");
      state.className = "folder-state";
      const policy = document.createElement("span");
      policy.className = "muted";
      const linkState = document.createElement("span");
      linkState.className = "muted";
      details.append(name, peer, state, policy, linkState);
      const actions = document.createElement("div");
      actions.className = "folder-actions";
      item.append(details, actions);
    }
    const [details, actions] = item.children;
    const [name, peer, state, policy, linkState] = details.children;
    name.textContent = share.label;
    const peerName = folderPeerName(status, share.peerId);
    peer.textContent = share.linkPolicy === null ? `${peerName} · Legacy two-way`
      : share.incoming ? `${peerName} → This server` : `This server → ${peerName}`;
    policy.textContent = folderSync.policyExplanation(share.linkPolicy);
    linkState.textContent = share.linkSettings === null ? ""
      : !share.linkSettings.confirmed ? "Waiting for source-confirmed link settings before transfer starts."
        : share.linkSettings.pendingChange !== null ? "A change is waiting for the source; current settings remain active."
          : share.linkSettings.conflictedChange !== null ? "A stale settings request needs review; current settings remain active."
            : share.linkSettings.settings.paused ? "Whole link paused."
              : "Link settings confirmed by the source.";
    state.dataset.kind = view.kind;
    state.textContent = view.text;
    // Routine health polling must preserve typed paths, keyboard focus, and
    // explicit confirmation. Rebuild controls only when their meaning changes.
    const linkMembers = status.shares.filter((item) => item.folderId === share.folderId && item.phase !== "removed")
      .map((item) => item.peerId);
    const actionState = JSON.stringify([
      folderDeviceId, share.incoming, share.phase, share.expired, share.linkSettings, share.linkRun,
      linkMembers, status.peers.map((item) => [item.peerId, item.displayName]),
      savedSettings?.folderId === share.folderId ? savedSettings : null,
      folderController.pendingRun()?.folderId === share.folderId,
    ]);
    if (actions.dataset.state !== actionState) {
      actions.replaceChildren();
      renderFolderActions(actions, status, share, view);
      actions.dataset.state = actionState;
    }
    if (list.children[index] !== item) list.insertBefore(item, list.children[index] ?? null);
  }
  const empty = $("[data-folders-empty]");
  empty.hidden = visibleShares.length > 0;
  if (visibleShares.length === 0) {
    empty.textContent = status.availability === "available"
      ? status.peers.length === 0
        ? "Your first link starts with a paired device. Choose Pair device to connect another Mac, Android device, or server."
        : "No links yet. Choose Create link to send a folder to a paired device."
      : "Shared folders cannot be loaded while folder sync is offline.";
  }
}

function renderPendingFolderOffer() {
  const card = $("[data-folder-pending]");
  const form = $("[data-folder-offer-form]");
  let pending = null;
  try { pending = folderController.loadPending(); }
  catch (error) { renderFolderError(error); }
  card.hidden = pending === null;
  form.querySelector("[data-folder-offer-submit]").disabled = pending !== null
    || folderController.isMutationLocked()
    || folderDeviceId === null
    || (folderController.current()?.peers.length ?? 0) === 0;
  if (pending === null) return;
  const peerName = folderController.current()
    ? folderPeerName(folderController.current(), pending.peerId)
    : "the saved confirmed device";
  $("[data-folder-pending-summary]").textContent = `${pending.label} for ${peerName}, using ${pending.selectedRoot}.`;
}

function renderPendingLinkSettings() {
  const card = $("[data-link-settings-pending]");
  let pending = null;
  try { pending = folderController.pendingLinkSettings(); }
  catch (error) { renderFolderError(error); }
  card.hidden = pending === null;
  if (pending === null) return;
  const share = folderController.current()?.shares.find((item) => item.folderId === pending.folderId);
  const label = share?.label ?? "this link";
  $("[data-link-settings-pending-summary]").textContent = `${label}: ${folderSync.settingsExplanation(pending.settings)}`;
}

function renderPendingLinkRun() {
  const card = $("[data-link-run-pending]");
  let pending = null;
  try { pending = folderController.pendingRun(); }
  catch (error) { renderFolderError(error); }
  card.hidden = pending === null;
  if (pending === null) return;
  const share = folderController.current()?.shares.find((item) => item.folderId === pending.folderId);
  $("[data-link-run-pending-summary]").textContent = `${share?.label ?? "This link"}: retry the saved request or discard it after checking current link status.`;
}

async function loadFolders(reportError = false) {
  try {
    const result = await folderController.refresh();
    if (result.busy) {
      const status = $("[data-folders-status]");
      status.textContent = "Updating status…";
      status.className = "folder-state";
      status.dataset.kind = "checking";
    }
    if (result.applied) {
      renderPendingFolderOffer();
      renderPendingLinkSettings();
      renderPendingLinkRun();
    }
  } catch (error) {
    renderFolderError(error);
    if (reportError) fail(error);
  }
}

async function initializeFolderSync() {
  const identity = await folderApi("/api/v1/transport/identity");
  folderDeviceId = identity?.deviceId ?? null;
  folderController.setAccess({ deviceId: folderDeviceId, unlocked: true });
  renderPendingFolderOffer();
  renderPendingLinkSettings();
  renderPendingLinkRun();
  syncFolderPolling(false);
  if (folderPollingEligible()) await loadFolders(false);
}

function clearFolderSyncAccess() {
  $("[data-link-count]").textContent = "—";
  $("[data-peer-count]").textContent = "—";
  folderDeviceId = null;
  folderController.setAccess({ deviceId: null, unlocked: false });
  folderController.setPollingEnabled(false);
  $("[data-link-settings-pending]").hidden = true;
  $("[data-link-run-pending]").hidden = true;
  $("[data-paired-devices]").replaceChildren();
  const empty = $("[data-paired-devices-empty]");
  empty.hidden = false;
  empty.textContent = "Unlock the console to load paired devices.";
}

function requireUnlocked() {
  if (token) return true;
  $("[data-access-panel]").open = true;
  $("#api-token").focus();
  say("Unlock the console with the local access token first.", true);
  return false;
}

function relativeExpiry(expiresAtUnixMs) {
  const seconds = Math.round((expiresAtUnixMs - Date.now()) / 1000);
  if (seconds <= 0) return "Expired";
  const format = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  return seconds < 90
    ? `Expires ${format.format(seconds, "second")}`
    : `Expires ${format.format(Math.round(seconds / 60), "minute")}`;
}

function renderNetworkCandidates(candidates) {
  const list = $("[data-network-list]");
  list.replaceChildren();
  candidates.forEach((candidate) => {
    const item = document.createElement("li");
    const label = document.createElement("span");
    label.textContent = `${candidate.where} · ${candidate.endpoint}`;
    const action = document.createElement("button");
    action.type = "button";
    action.className = "secondary";
    action.textContent = "Pair with this device";
    action.addEventListener("click", () => startNetworkPairing(candidate.endpoint));
    item.append(label, action);
    list.append(item);
  });
  $("[data-network-empty]").hidden = candidates.length > 0;
}

function stopNetworkPolling() {
  if (networkPoll === null) return;
  clearInterval(networkPoll);
  networkPoll = null;
}

function startNetworkPolling() {
  if (networkPoll !== null) return;
  networkPoll = setInterval(() => { refreshNetworkPairings().catch(() => {}); }, 3000);
}

function renderNetworkPairing(item) {
  networkPairing = item;
  const card = $("[data-network-card]");
  if (item === null) {
    card.hidden = true;
    stopNetworkPolling();
    return;
  }
  const details = pairing.network.summary(item);
  $("[data-network-peer]").textContent = details.peerName;
  $("[data-network-direction]").textContent = details.direction;
  const code = $("[data-network-code]");
  code.textContent = details.code;
  // Read the groups apart rather than as one long number, matching the Android
  // pairing card. The visible text keeps its hyphens.
  code.setAttribute("aria-label", `Comparison code ${details.spokenCode}`);
  $("[data-network-expires]").textContent = relativeExpiry(details.expiresAtUnixMs);
  const failureCopy = details.failureCode !== null && Object.hasOwn(errorCopy.CATALOG, details.failureCode)
    ? ` ${errorCopy.CATALOG[details.failureCode][0]}`
    : "";
  $("[data-network-state]").textContent = `${details.stateCopy}${failureCopy}`;
  $("[data-network-confirm]").disabled = !details.awaitingLocalConfirmation;
  $("[data-network-cancel]").textContent = details.settled ? "Dismiss" : "Cancel pairing";
  card.hidden = false;
  if (details.settled) stopNetworkPolling(); else startNetworkPolling();
}

async function refreshNetworkPairings() {
  if (!token) return;
  const pending = await pairing.network.pending(api);
  if (networkPairing !== null) {
    const priorState = networkPairing.state;
    const updated = pending.find((item) => item.pairingId === networkPairing.pairingId);
    renderNetworkPairing(updated ?? null);
    if (updated?.state === "complete" && priorState !== "complete") await loadFolders(false);
    return;
  }
  // An incoming request is the other device asking to pair with this one; it is
  // the only thing worth surfacing unprompted.
  const incoming = pending.find((item) => item.direction === "incoming" && item.state !== "failed");
  if (incoming !== undefined) {
    renderNetworkPairing(incoming);
    if (incoming.state === "complete") await loadFolders(false);
  }
}

async function startNetworkPairing(candidateAddress) {
  if (!requireUnlocked()) return;
  try {
    renderNetworkPairing(await pairing.network.start(api, candidateAddress));
    say("Compare the code below on both devices before confirming.");
  } catch (error) { fail(error); }
}

$("[data-token-file-choose]").addEventListener("click", () => $("[data-token-file]").click());
$("[data-token-file]").addEventListener("change", async (event) => {
  const input = event.currentTarget;
  const file = input.files?.[0];
  if (!file) return;
  try {
    // Read only the explicitly selected small token file, never a server path.
    if (file.size > 4096) throw new Error("invalid token file");
    const value = (await file.text()).trim();
    // Match the node's local-token bounds and printable-ASCII contract.
    if (!/^[\x21-\x7e]{32,512}$/.test(value) || /["\\]/.test(value)) {
      throw new Error("invalid token file");
    }
    $("#api-token").value = value;
    $("#api-token").focus();
    say("Token file selected. Choose Unlock console to connect.");
  } catch {
    say("Choose the local API token file from your trusted claim output, not a setup code or settings file.", true);
  } finally {
    input.value = "";
  }
});

$("[data-token-form]").addEventListener("submit", async (event) => {
  event.preventDefault();
  token = formData(event.currentTarget).get("token").trim();
  try {
    await errorCopy.initializeUnlockedConsole({
      authorize: () => api("/api/v1/config/export", { method: "POST" }),
      initializeLinks: initializeFolderSync,
      refreshPairings: refreshNetworkPairings,
      onLinksError(error) { clearFolderSyncAccess(); renderFolderError(error); },
    });
    $("[data-access-panel]").open = false;
    $("[data-access-state]").textContent = "Unlocked in this tab";
    $("[data-access-guidance]").textContent = "Unlocked for this tab";
    // Keep the credential only in the existing in-memory token variable.
    $("#api-token").value = "";
    $("[data-tab=folders]").focus();
    say("Console unlocked for this tab only.", false, null, true);
  }
  catch (error) {
    token = "";
    $("[data-access-panel]").open = true;
    $("[data-access-state]").textContent = "Console locked";
    $("[data-access-guidance]").textContent = "Unlock this browser tab";
    clearFolderSyncAccess();
    fail(error);
  }
});

$("[data-refresh]").addEventListener("click", async () => {
  await loadStatus();
  if (!token) return;
  try { await Promise.all([loadFolders(false), refreshNetworkPairings()]); }
  catch (error) { fail(error); }
});
$("[data-folders-refresh]").addEventListener("click", async () => {
  if (!requireUnlocked()) return;
  syncFolderPolling(false);
  await loadFolders(true);
});
$("[data-folders-retry-service]").addEventListener("click", () => {
  void runFolderMutation(() => folderController.retryService(), "Folder sync retry started.");
});
// A required field inside a closed disclosure must be visible for browser validation.
$("[data-folder-offer-form]").addEventListener("invalid", (event) => {
  for (let parent = event.target.parentElement; parent; parent = parent.parentElement) {
    if (parent.tagName === "DETAILS") parent.open = true;
  }
}, true);
$('[data-folder-offer-form]').addEventListener("change", (event) => {
  if (event.target.name === "cadenceMode") syncOfferScheduleInput();
});
$("[data-folder-offer-form]").addEventListener("submit", (event) => {
  event.preventDefault();
  if (!requireUnlocked()) return;
  // DOM Event.currentTarget becomes null once dispatch returns. Keep the form
  // itself across the async offer so a successful response can reset it.
  const form = event.currentTarget;
  const data = formData(form);
  const body = {
    peerId: data.get("peerId"),
    folderId: crypto.randomUUID(),
    label: data.get("label"),
    selectedRoot: data.get("selectedRoot"),
    linkPolicy: {
      propagateSourceDeletions: data.get("propagateSourceDeletions") === "on",
      restoreLocalDeletions: data.get("restoreLocalDeletions") === "on",
    },
    cadence: cadenceValue(data.get("cadenceMode"), data.get("intervalMinutes")),
    androidConditions: {
      wifiOnly: data.get("wifiOnly") === "on",
      chargingOnly: data.get("chargingOnly") === "on",
    },
  };
  if ((body.linkPolicy.propagateSourceDeletions || body.linkPolicy.restoreLocalDeletions)
    && !globalThis.confirm(`${folderSync.policyExplanation(body.linkPolicy)} Create this link?`)) return;
  void runFolderMutation(async () => {
    await folderController.sendOffer(body);
    form.reset();
    form.elements.selectedRoot.value = "/sync";
    syncOfferScheduleInput();
    $("[data-create-link]").open = false;
    $("[data-open-create]").focus();
  }, "Folder offer sent to the confirmed paired device.");
});
$("[data-folder-offer-retry]").addEventListener("click", () => {
  void runFolderMutation(() => folderController.retryPendingOffer(), "Saved folder offer sent.");
});
$("[data-folder-offer-discard]").addEventListener("click", () => {
  if (!globalThis.confirm("Discard this saved retry? This does not remove an offer that may already have reached the server.")) return;
  try {
    folderController.discardPendingOffer();
    renderPendingFolderOffer();
    say("Saved folder-offer retry discarded.");
  } catch (error) { fail(error); }
});
$('[data-link-settings-retry]').addEventListener("click", () => {
  void runFolderMutation(
    () => folderController.retryPendingLinkSettings(),
    "Saved link-settings request reconciled with the server.",
  );
});
$('[data-link-settings-discard]').addEventListener("click", () => {
  if (!globalThis.confirm("Discard this exact retry only after reviewing current link settings. The request may already have reached the server. Continue?")) return;
  try {
    folderController.discardPendingLinkSettings();
    renderPendingLinkSettings();
    say("Saved link-settings retry discarded. Current server settings were not changed.");
  } catch (error) { fail(error); }
});
$('[data-link-run-retry]').addEventListener("click", () => {
  void runFolderMutation(
    () => folderController.retryPendingRun(),
    "Saved Run Now request reconciled with the server.",
  );
});
$('[data-link-run-discard]').addEventListener("click", () => {
  if (!globalThis.confirm("Discard this exact retry only after checking current link status. The run request may already have reached the source. Continue?")) return;
  try {
    folderController.discardPendingRun();
    renderPendingLinkRun();
    say("Saved Run Now retry discarded. No new run request was sent.");
  } catch (error) { fail(error); }
});
tabFlow.install(document);
document.querySelectorAll("[data-open-tab]").forEach((button) => {
  button.addEventListener("click", () => {
    const tab = document.querySelector(`[data-tab="${button.dataset.openTab}"]`);
    tab.click();
    tab.focus();
  });
});
$("[data-open-create]").addEventListener("click", () => {
  if (!requireUnlocked()) return;
  $("[data-create-link]").open = true;
  const form = $("[data-folder-offer-form]");
  (form.elements.peerId.disabled ? form.elements.label : form.elements.peerId).focus();
});
$("[data-close-create]").addEventListener("click", () => {
  $("[data-create-link]").open = false;
  $("[data-open-create]").focus();
});
document.querySelectorAll("[data-tab]").forEach((tab) => {
  tab.addEventListener("click", () => queueMicrotask(() => syncFolderPolling(true)));
  tab.addEventListener("keydown", () => queueMicrotask(() => syncFolderPolling(true)));
});
document.addEventListener("visibilitychange", () => syncFolderPolling(true));
setInterval(() => {
  if (folderPollingEligible()) void loadFolders(false);
}, 5000);

$("[data-network-discover]").addEventListener("click", async () => {
  if (!requireUnlocked()) return;
  const state = $("[data-network-discovery-state]");
  state.textContent = "Searching…";
  try {
    const candidates = await pairing.network.candidates(api);
    renderNetworkCandidates(candidates);
    state.textContent = candidates.length === 0 ? "" : `${candidates.length} device(s) answered.`;
    await refreshNetworkPairings();
  } catch (error) { state.textContent = ""; fail(error); }
});

$("[data-network-manual]").addEventListener("submit", async (event) => {
  event.preventDefault();
  await startNetworkPairing(formData(event.currentTarget).get("candidateAddress"));
});

$("[data-network-confirm]").addEventListener("click", async () => {
  if (networkPairing === null || !requireUnlocked()) return;
  try {
    const confirmed = await pairing.network.confirm(api, networkPairing);
    renderNetworkPairing(confirmed);
    if (confirmed.state === "complete") await loadFolders(false);
    say(confirmed.state === "complete"
      ? "Device paired. Its signed identity is now available for one-way links."
      : "Confirmed here. Waiting for the other device.");
  } catch (error) { fail(error); }
});

$("[data-network-cancel]").addEventListener("click", async () => {
  if (networkPairing === null) return;
  const pairingId = networkPairing.pairingId;
  const settled = networkPairing.state === "complete" || networkPairing.state === "failed";
  try {
    await pairing.network.dismiss(api, pairingId);
    renderNetworkPairing(null);
    say(settled ? "Pairing record dismissed." : "Pairing request cancelled. Nothing was trusted.");
  }
  catch (error) { fail(error); }
});

$("[data-settings-export]").addEventListener("click", async () => {
  try { const settings = await api("/api/v1/config/export", { method: "POST" }); const blob = new Blob([JSON.stringify(settings, null, 2)], { type: "application/json" }); const link = Object.assign(document.createElement("a"), { href: URL.createObjectURL(blob), download: "covalent-settings.json" }); link.click(); URL.revokeObjectURL(link.href); say("Safe settings downloaded."); }
  catch (error) { fail(error); }
});
$("[data-settings-import]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  try { await api("/api/v1/config/import", { method: "POST", body: JSON.stringify({ confirmed: data.get("confirmed") === "on", settings: JSON.parse(data.get("settings")) }) }); await loadStatus(); say("Safe settings imported. LAN discovery state was refreshed from the running node."); }
  catch (error) { fail(error); }
});

// Sidebar collapse changes presentation only; navigation keeps its labels.
function setSidebarExpanded(expanded) {
  const sidebar = $("#console-sidebar");
  const button = $("[data-sidebar-toggle]");
  sidebar.dataset.state = expanded ? "expanded" : "collapsed";
  document.body.classList.toggle("sidebar-collapsed", !expanded);
  button.setAttribute("aria-expanded", String(expanded));
  button.setAttribute("aria-label", expanded ? "Collapse sidebar" : "Expand sidebar");
  button.title = expanded ? "Collapse sidebar" : "Expand sidebar";
}
$("[data-sidebar-toggle]").addEventListener("click", () => {
  setSidebarExpanded($("#console-sidebar").dataset.state !== "expanded");
});
const compactSidebar = globalThis.matchMedia("(max-width:700px)");
if (compactSidebar.matches) setSidebarExpanded(false);
compactSidebar.addEventListener("change", (event) => {
  if (event.matches) setSidebarExpanded(false);
});

loadStatus();
}
