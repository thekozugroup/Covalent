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

  const nodeErrorCopy = Object.freeze({
    CATALOG,
    RECOVERY,
    SUMMARIES,
    SUPPORTED_PROTOCOL_VERSION,
    describe,
    describeApi,
    describeProtocol,
    describeTransport,
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
let token = "";
let restorePlan = null;
let restorePage = null;
let restoreCursor = null;
let restoreCursorHistory = [];
let networkPairing = null;
let networkPoll = null;
const pairing = globalThis.CovalentPairingFlow;
const restore = globalThis.CovalentRestorePlanFlow;
const errorCopy = globalThis.CovalentNodeErrorCopy;
const pairingStorageKey = "covalent.pairing-session.v1";

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

function say(text, isError = false, detail = null) {
  message.textContent = text;
  message.classList.toggle("error", isError);
  messageDetail.textContent = detail ?? "";
  messageDetails.hidden = detail === null;
  messageDetails.open = false;
}

// The only path from a thrown value to the screen. It never reads `.message`.
function fail(error) {
  const failure = errorCopy.describe(error);
  console.debug("Covalent request failed", error);
  say(failure.summary, true, failure.detail);
  message.dataset.recovery = failure.recovery;
}

async function api(path, options = {}) {
  const headers = new Headers(options.headers || {});
  headers.set("Accept", "application/json");
  if (token) headers.set("Authorization", `Bearer ${token}`);
  if (options.body) headers.set("Content-Type", "application/json");
  const response = await fetch(path, { ...options, headers, cache: "no-store" });
  if (!response.ok) {
    const decoded = await response.json().catch(() => ({}));
    const body = decoded && typeof decoded === "object" ? decoded : {};
    if (body.protocolVersion !== undefined && body.protocolVersion !== PROTOCOL_VERSION) {
      throw new ProtocolMismatchError(body.protocolVersion);
    }
    throw new NodeApiError(response.status, body);
  }
  if (response.status === 204) return null;
  return response.json();
}

async function loadStatus() {
  try {
    const status = await api("/api/v1/status");
    if (status.protocolVersion !== PROTOCOL_VERSION) {
      throw new ProtocolMismatchError(status.protocolVersion);
    }
    $("[data-device-name]").textContent = status.deviceName;
    $("[data-state]").textContent = `Service state: ${status.state}. Protocol ${status.protocolVersion}.`;
    document.querySelectorAll("[data-discovery]").forEach((el) => { el.textContent = status.lanDiscovery ? "On" : "Off"; });
  } catch (error) {
    $("[data-device-name]").textContent = "Node unavailable";
    $("[data-state]").textContent = errorCopy.describe(error).summary;
    fail(error);
  }
}

async function loadBackups() {
  if (!token) return;
  const backups = await api("/api/v1/backups");
  const list = $("[data-backups-list]");
  list.replaceChildren();
  backups.forEach((backup) => {
    const item = document.createElement("li");
    const latest = backup.latestSnapshotId ? `latest ${backup.latestSnapshotId}` : "no local snapshot";
    item.textContent = `${backup.name} — ${latest}; ${backup.snapshotCount} retained; ${backup.selectedProviderIds.length} explicitly selected providers`;
    list.append(item);
  });
  $("[data-backups-empty]").hidden = backups.length > 0;
  if (backups.length === 0) $("[data-backups-empty]").textContent = "No remembered backups on this node.";
}

function formData(form) { return new FormData(form); }
function selected(form, name) { return [...form.querySelectorAll(`[name="${name}"]:checked`)].map((input) => input.value); }
function randomId(prefix) { return `${prefix}-${crypto.randomUUID().replaceAll("-", "").slice(0, 16)}`; }
function display(value) { return JSON.stringify(value, null, 2); }

function renderRestorePage(page) {
  restorePage = page;
  const first = page.entries.length === 0 ? 0 : page.entryOffset + 1;
  const last = page.entryOffset + page.entries.length;
  $("[data-restore-summary]").textContent = restorePlan.totalEntries
    + " entries are signed for " + restorePlan.authorizedRoot
    + ". Showing " + first + "–" + last + ".";
  $("[data-restore-plan]").textContent = display(page.entries);
  $("[data-restore-previous]").disabled = restoreCursorHistory.length === 0;
  $("[data-restore-next]").disabled = page.nextCursor === null;
}

async function loadRestorePage(cursor, rememberCurrent = false) {
  if (!restorePlan) return;
  const page = await restore.page(api, restorePlan, cursor, 100);
  if (rememberCurrent) restoreCursorHistory.push(restoreCursor);
  restoreCursor = cursor;
  renderRestorePage(page);
}

function clearRestorePreview() {
  restorePlan = null;
  restorePage = null;
  restoreCursor = null;
  restoreCursorHistory = [];
  $("[data-restore-result]").hidden = true;
  $("[data-restore-confirm]").checked = false;
  $("[data-restore-execute]").disabled = true;
}

// ------------------------------------------------------------ network pairing
//
// The same four steps the phone and Mac apps use: look for devices, start with
// one, compare the short code on both screens, confirm. The node runs the
// exchange itself, so nothing signed passes through this browser and there is
// no JSON to copy anywhere on this path.

function requireUnlocked() {
  if (token) return true;
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
    const updated = pending.find((item) => item.pairingId === networkPairing.pairingId);
    renderNetworkPairing(updated ?? null);
    return;
  }
  // An incoming request is the other device asking to pair with this one; it is
  // the only thing worth surfacing unprompted.
  const incoming = pending.find((item) => item.direction === "incoming" && item.state !== "failed");
  if (incoming !== undefined) renderNetworkPairing(incoming);
}

async function startNetworkPairing(candidateAddress) {
  if (!requireUnlocked()) return;
  try {
    renderNetworkPairing(await pairing.network.start(api, candidateAddress));
    say("Compare the code below on both devices before confirming.");
  } catch (error) { fail(error); }
}

$("[data-token-form]").addEventListener("submit", async (event) => {
  event.preventDefault();
  token = formData(event.currentTarget).get("token").trim();
  try {
    await api("/api/v1/config/export", { method: "POST" });
    await loadBackups();
    await refreshNetworkPairings();
    say("Console unlocked for this tab only.");
  }
  catch (error) { token = ""; fail(error); }
});

$("[data-refresh]").addEventListener("click", async () => { await loadStatus(); if (token) await loadBackups(); });
$("[data-backups-refresh]").addEventListener("click", async () => {
  try { await loadBackups(); say("Backup list refreshed from the node."); }
  catch (error) { fail(error); }
});
document.querySelectorAll("[data-tab]").forEach((tab) => tab.addEventListener("click", () => {
  document.querySelectorAll("[data-tab]").forEach((button) => button.setAttribute("aria-selected", String(button === tab)));
  document.querySelectorAll("[data-panel]").forEach((panel) => { panel.hidden = panel.dataset.panel !== tab.dataset.tab; });
}));

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
    say(confirmed.state === "complete"
      ? "Backup device added, with its signed certificate fingerprint."
      : "Confirmed here. Waiting for the other device.");
  } catch (error) { fail(error); }
});

$("[data-network-cancel]").addEventListener("click", async () => {
  if (networkPairing === null) return;
  const pairingId = networkPairing.pairingId;
  const settled = networkPairing.state === "complete" || networkPairing.state === "failed";
  renderNetworkPairing(null);
  if (settled) return;
  try { await pairing.network.cancel(api, pairingId); say("Pairing request cancelled. Nothing was trusted."); }
  catch (error) { fail(error); }
});

$("[data-pair-create]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  try {
    const invitation = await api("/api/v1/pair/invitations", { method: "POST", body: JSON.stringify({ endpoints: [data.get("endpoint")], lifetimeMs: Number(data.get("minutes")) * 60000 }) });
    say(`Invitation created. Copy this JSON to the other device:\n${display(invitation)}`);
  } catch (error) { fail(error); }
});

$("[data-pair-accept]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  try {
    const session = await api("/api/v1/pair/accept", { method: "POST", body: JSON.stringify({ invitation: JSON.parse(data.get("invitation")), responderName: data.get("name"), responderRoles: selected(event.currentTarget, "responderRole"), inviterRoles: selected(event.currentTarget, "inviterRole") }) });
    const details = showPairingSession(session);
    say(`Compare identities, exact roles, and this code on both devices: ${details.code}\nThe accepted session is preserved below. Add one confirmation, then send the updated JSON to the other device.`);
  } catch (error) { fail(error); }
});

$("[data-pair-confirm]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget); const submitter = event.submitter;
  try {
    const session = JSON.parse(data.get("session")); const side = submitter.value;
    if (submitter.dataset.pairAction === "confirm") {
      const confirmed = await pairing.confirm(api, session, side, data.get("code"));
      const details = showPairingSession(confirmed);
      say(details.mutuallyConfirmed
        ? `Both signed confirmations are present. Send this exact session to the other device, then finalize on each device:\n${display(confirmed)}`
        : `This device signed the exchange. Finalize is still locked. Send this updated session to the other device for its confirmation:\n${display(confirmed)}`);
      return;
    }
    const confirmation = await pairing.finalize(api, session, side);
    say(`Pairing finalized on this device. The other device must finalize the same mutually signed session:\n${display(confirmation)}`);
  } catch (error) { fail(error); }
});
$("[data-pair-confirm] [name=session]").addEventListener("input", (event) => {
  try { showPairingSession(JSON.parse(event.currentTarget.value)); } catch (_) {
    $("[data-pair-consent]").hidden = true;
    $("[data-pair-confirm]").querySelectorAll("[data-pair-action=finalize]").forEach((button) => { button.disabled = true; });
  }
});
$("[data-pair-clear]").addEventListener("click", () => { clearPairingSession(); say("Pairing exchange cleared from this browser tab."); });

$("[data-backup-form]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  try {
    const providers = data.get("providers").split(/\s+/).filter(Boolean);
    const result = await api("/api/v1/backups", { method: "POST", body: JSON.stringify({ sourceRoot: data.get("sourceRoot"), displayName: data.get("displayName"), snapshotId: data.get("snapshotId"), jobId: randomId("backup"), selectedProviderIds: providers }) });
    await loadBackups();
    say(`Backup complete: ${result.backupId}, ${result.entries} entries, ${result.selectedProviders} explicitly selected providers.`);
  } catch (error) { fail(error); }
});

$("[data-restore-preview]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  let candidate = null;
  try {
    const previous = restorePlan;
    candidate = restore.requireReference(await api("/api/v1/restores/preview", { method: "POST", body: JSON.stringify({ backupId: data.get("backupId"), snapshotId: data.get("snapshotId"), targetRoot: data.get("targetRoot"), conflictPolicy: data.get("conflictPolicy"), jobId: randomId("restore") }) }));
    const firstPage = await restore.page(api, candidate, null, 100);
    restorePlan = candidate;
    restoreCursor = null; restoreCursorHistory = [];
    renderRestorePage(firstPage);
    if (previous) await restore.discard(api, previous).catch(() => {});
    $("[data-restore-result]").hidden = false; $("[data-restore-confirm]").checked = false; $("[data-restore-execute]").disabled = true;
    say("Preview complete. Review the target and conflict actions before authorizing the write.");
  } catch (error) { if (candidate) await restore.discard(api, candidate).catch(() => {}); fail(error); }
});
$("[data-restore-next]").addEventListener("click", async () => {
  if (!restorePage?.nextCursor) return;
  try { await loadRestorePage(restorePage.nextCursor, true); } catch (error) { fail(error); }
});
$("[data-restore-previous]").addEventListener("click", async () => {
  if (restoreCursorHistory.length === 0) return;
  const cursor = restoreCursorHistory.pop();
  try { await loadRestorePage(cursor); } catch (error) { fail(error); }
});
$("[data-restore-discard]").addEventListener("click", async () => {
  const plan = restorePlan;
  clearRestorePreview();
  try { await restore.discard(api, plan); say("Restore preview discarded without writing files."); } catch (error) { fail(error); }
});
$("[data-restore-confirm]").addEventListener("change", (event) => { $("[data-restore-execute]").disabled = !event.currentTarget.checked || !restorePlan; });
$("[data-restore-execute]").addEventListener("click", async () => {
  if (!restorePlan) return;
  try { const plan = restorePlan; const result = await restore.execute(api, plan); await restore.discard(api, plan).catch(() => {}); clearRestorePreview(); say(`Restore complete: ${result.filesRestored} files, ${result.directoriesCreated} directories.`); }
  catch (error) { fail(error); }
});

$("[data-settings-export]").addEventListener("click", async () => {
  try { const settings = await api("/api/v1/config/export", { method: "POST" }); const blob = new Blob([display(settings)], { type: "application/json" }); const link = Object.assign(document.createElement("a"), { href: URL.createObjectURL(blob), download: "covalent-settings.json" }); link.click(); URL.revokeObjectURL(link.href); say("Safe settings downloaded."); }
  catch (error) { fail(error); }
});
$("[data-settings-import]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  try { await api("/api/v1/config/import", { method: "POST", body: JSON.stringify({ confirmed: data.get("confirmed") === "on", settings: JSON.parse(data.get("settings")) }) }); await loadStatus(); say("Safe settings imported. LAN discovery state was refreshed from the running node."); }
  catch (error) { fail(error); }
});

// Kept as the single write point for the tab-scoped exchange the loader below
// reads back. The JSON exchange is the fallback path now, so nothing on the
// network-pairing flow depends on it.
function persistPairingSession(session) {
}

function clearPairingSession() {
  const form = $("[data-pair-confirm]");
  form.reset();
  $("[data-pair-consent]").hidden = true;
  form.querySelectorAll("[data-pair-action=finalize]").forEach((button) => { button.disabled = true; });
}

function showPairingSession(session) {
  const details = pairing.summary(session);
  const form = $("[data-pair-confirm]");
  form.elements.session.value = display(session);
  $("[data-pair-inviter]").textContent = `${details.inviter.name} (${details.inviter.id}) — ${details.inviter.roles}`;
  $("[data-pair-responder]").textContent = `${details.responder.name} (${details.responder.id}) — ${details.responder.roles}`;
  $("[data-pair-code]").textContent = details.code;
  $("[data-pair-signatures]").textContent = `Creator ${details.inviter.confirmed ? "signed" : "waiting"}; accepter ${details.responder.confirmed ? "signed" : "waiting"}.`;
  $("[data-pair-consent]").hidden = false;
  form.querySelectorAll("[data-pair-action=finalize]").forEach((button) => { button.disabled = !details.mutuallyConfirmed; });
  persistPairingSession(session);
  return details;
}

try {
  const savedPairingSession = sessionStorage.getItem(pairingStorageKey);
  if (savedPairingSession) showPairingSession(JSON.parse(savedPairingSession));
} catch (_) { /* invalid or unavailable tab storage starts a fresh exchange */ }
loadStatus();
}
