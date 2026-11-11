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
    // means entering the server access token again rather than re-pairing.
    authentication_required: [
      "This console is no longer unlocked. Enter the server access token again to continue.",
      RECOVERY.reconnect,
    ],
    not_authorized: [
      "This server refused this request. Unlock the console again with a current access token.",
      RECOVERY.reconnect,
    ],
    invalid_certificate: [
      "This browser doesn't trust this server's security certificate. Add the server's certificate authority to this browser, then reload.",
      RECOVERY.reconnect,
    ],

    // Capacity
    insufficient_storage: [
      "This server is out of space. Free some up, or choose a different device to keep this copy.",
      RECOVERY.freeUpSpace,
    ],
    resource_limit: [
      "This is larger than this server can handle in one pass. Try a smaller folder.",
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
    node_busy: ["This server is busy with something else. Try again in a moment.", RECOVERY.retry],
    node_state_locked: ["This server is applying another change. Try again in a moment.", RECOVERY.retry],
    archive_processing_timeout: [
      "This server took too long to work through this backup. Try again, or choose a smaller folder.",
      RECOVERY.retry,
    ],
    archive_processing_too_slow: [
      "This transfer was running too slowly to continue safely. Check the network between the two devices, then try again.",
      RECOVERY.retry,
    ],
    confirmation_required: ["This has to be confirmed on the other device before it can finish.", RECOVERY.none],
    recovery_confirmation_required: ["Confirm that you want to create recovery files or retry recovery, then continue.", RECOVERY.none],
    recovery_not_configured: ["This server was set up normally. To replace a lost device, start a new installation with its recovery files.", RECOVERY.none],

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
    link_settings_conflict: ["Link settings changed on another device. Refresh, then check the current settings before saving again.", RECOVERY.none],
    link_settings_pending: ["A settings change is already waiting for the source. Wait for it, or check it, before making another change.", RECOVERY.none],
    link_run_conflict: ["The link changed before this run started. Refresh, then choose Run Now again.", RECOVERY.none],
    link_run_pending: ["Another Run Now request is waiting for the source. Refresh before trying again.", RECOVERY.none],
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
      "Covalent could not confirm the paired device at the new address. Check the address and network connection, then try again.",
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
      "Covalent couldn't read the restore plan this server sent. Preview the restore again.",
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
    backup_unavailable: ["This backup isn't available on this server right now.", RECOVERY.retry],
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
      "This pairing request has expired or was already used. Start pairing again.",
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
      "This device has no address another device can reach it on yet. Set its device address, then pair again.",
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

    // Request contract — this console and the server disagree
    invalid_contract: [
      "This page and this server don't agree on how to talk to each other. Reload the page; if it keeps happening, update the server.",
      RECOVERY.none,
    ],
    invalid_json: [
      "This page and this server don't agree on how to talk to each other. Reload the page; if it keeps happening, update the server.",
      RECOVERY.none,
    ],
    invalid_content_type: [
      "This page and this server don't agree on how to talk to each other. Reload the page; if it keeps happening, update the server.",
      RECOVERY.none,
    ],
    method_not_allowed: [
      "This page asked for something this server doesn't offer. Reload the page; if it keeps happening, update the server.",
      RECOVERY.none,
    ],
    route_not_found: [
      "This page asked for something this server doesn't offer. Reload the page; if it keeps happening, update the server.",
      RECOVERY.none,
    ],
    invalid_page_cursor: ["Covalent lost its place while loading this list. Try again.", RECOVERY.retry],
    invalid_page_limit: ["Covalent lost its place while loading this list. Try again.", RECOVERY.retry],
    internal_error: [
      "Something went wrong on this server. Try again; if it keeps happening, check its logs.",
      RECOVERY.retry,
    ],
  });

  // Status fallbacks for a code this build has never seen. Ordered so the
  // narrow cases win before the 5xx sweep.
  const STATUS = Object.freeze({
    unauthorized: [
      "This server refused this request. Unlock the console again with a current access token.",
      RECOVERY.reconnect,
    ],
    notFound: ["Covalent couldn't find that on this server.", RECOVERY.none],
    conflict: ["Something else changed on this server first. Reload the page, then try again.", RECOVERY.retry],
    busy: ["This server is busy. Try again in a moment.", RECOVERY.retry],
    tooLarge: [
      "That's larger than this server accepts in one go. Try a smaller folder.",
      RECOVERY.none,
    ],
    rejected: [
      "This server wouldn't accept what this console sent. Check the values in the form, then try again.",
      RECOVERY.none,
    ],
    outOfSpace: ["This server is out of space.", RECOVERY.freeUpSpace],
    serverProblem: [
      "Something went wrong on this server. Try again; if it keeps happening, check its logs.",
      RECOVERY.retry,
    ],
    unavailable: [
      "This server isn't ready to answer yet. Wait for it to finish starting, then try again.",
      RECOVERY.retry,
    ],
    retryable: ["This server couldn't complete that request. Try again in a moment.", RECOVERY.retry],
    unknown: ["This server couldn't complete that request.", RECOVERY.none],
  });

  // The browser cannot tell a stopped server from an untrusted certificate: fetch
  // reports both as one opaque TypeError. Rather than guess, the copy names
  // both causes and the one action that covers them.
  const TRANSPORT = Object.freeze({
    offline: [
      "This computer isn't connected to a network, so Covalent can't reach this server.",
      RECOVERY.checkNetworkSettings,
    ],
    timedOut: [
      "This server didn't answer in time. Make sure it's turned on and awake, then try again.",
      RECOVERY.retry,
    ],
    unreachable: [
      "This browser couldn't reach this server. It may be stopped, or this browser may not trust its security certificate yet. If the server is running, add its certificate authority to this browser, then reload.",
      RECOVERY.reconnect,
    ],
    blocked: [
      "This browser blocked the request to this server. Open the console at the server's own HTTPS address rather than through another page.",
      RECOVERY.reconnect,
    ],
    unreadable: [
      "Covalent couldn't read that as JSON. Check that you pasted the whole thing, including its outer braces.",
      RECOVERY.none,
    ],
    protocol: [
      "This page and this server don't agree on how to talk to each other. Reload the page; if it keeps happening, update the server.",
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

  // The transport outcomes in which this server gave no answer at all. The
  // console shows these as a connection state with a way to try again, because
  // they describe the link to the server rather than the result of one request.
  const UNANSWERED = new Set([TRANSPORT.offline, TRANSPORT.timedOut, TRANSPORT.unreachable, TRANSPORT.blocked]);

  // What the console says about one request the server never answered. A
  // change may or may not have been applied, so the person checks before
  // repeating it. A read changed nothing, so it just failed to load; the
  // connection notice above it offers Try again.
  const UNANSWERED_ACTION = "This server didn't answer, so that change may not have gone through. Check before repeating it.";
  const UNANSWERED_READ = "This server didn't answer, so that information couldn't be loaded. Choose Try again once it's reachable.";

  function failure(entry, detail) {
    return Object.freeze({
      summary: entry[0],
      recovery: entry[1],
      detail: detail ?? null,
      unanswered: UNANSWERED.has(entry),
    });
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
        return Object.freeze({ summary: error.covalentGuidance, recovery: RECOVERY.none, detail: null, unanswered: false });
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

  // The server's version and the result of its own once-a-day release check,
  // both read from GET /api/v1/status. The server checks; this page only
  // reports. Older servers send neither field, which shows no version and no
  // note. Nothing here is ever an error: a check that could not run is at most
  // a quiet line beside the version.
  const RELEASE_URL_PREFIX = "https://github.com/thekozugroup/Covalent/releases/tag/v";
  const UPDATE_CHECK_FAILURES = Object.freeze({
    network: "Couldn't check for updates: this server couldn't reach GitHub. Check its internet connection.",
    timeout: "Couldn't check for updates: GitHub took too long to answer. Covalent tries again tomorrow.",
    secureConnection: "Couldn't check for updates over a secure connection. On a filtered network, update checks can be turned off.",
    rateLimited: "Couldn't check for updates: GitHub is limiting requests from this network. Covalent tries again tomorrow.",
    unexpectedResponse: "Couldn't check for updates: GitHub's answer wasn't one Covalent understands. Covalent tries again tomorrow.",
  });

  function serverVersion(status) {
    const body = status !== null && typeof status === "object" ? status : {};
    const version = typeof body.version === "string" && /^[0-9][0-9A-Za-z.+-]{0,39}$/.test(body.version) ? body.version : null;
    const update = body.update !== null && typeof body.update === "object" ? body.update : {};
    const latest = typeof update.latestVersion === "string" && /^\d{1,6}\.\d{1,6}\.\d{1,6}$/.test(update.latestVersion)
      ? update.latestVersion : null;
    let note = null;
    let available = null;
    if (update.state === "upToDate") note = "Up to date";
    else if (update.state === "disabled") note = "Update checks are off";
    else if (update.state === "checkFailed") {
      const code = typeof update.failure?.code === "string" && Object.hasOwn(UPDATE_CHECK_FAILURES, update.failure.code)
        ? update.failure.code : "unexpectedResponse";
      note = UPDATE_CHECK_FAILURES[code];
    } else if (update.state === "updateAvailable" && latest !== null) {
      // The link is used only when it is exactly the release page for the
      // version being announced; anything else is shown without a link.
      const releaseUrl = update.releaseUrl === `${RELEASE_URL_PREFIX}${latest}` ? update.releaseUrl : null;
      available = Object.freeze({ version: latest, releaseUrl });
      note = `Version ${latest} is available`;
    }
    return Object.freeze({ version, note, available, pending: update.state === "pending" });
  }

  const nodeErrorCopy = Object.freeze({
    CATALOG,
    RECOVERY,
    SUMMARIES,
    SUPPORTED_PROTOCOL_VERSION,
    UNANSWERED_ACTION,
    UNANSWERED_READ,
    describe,
    describeApi,
    describeProtocol,
    describeTransport,
    initializeUnlockedConsole,
    serverVersion,
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
const connection = $("[data-connection]");
const connectionText = $("[data-connection-text]");
let messageTimer = null;
let token = "";
let accessReady = false;
let networkPairing = null;
let networkPoll = null;
const pairing = globalThis.CovalentPairingFlow;
const tabFlow = globalThis.CovalentTabFlow;
const errorCopy = globalThis.CovalentNodeErrorCopy;
const folderSync = globalThis.CovalentFolderSyncFlow;
let folderDeviceId = null;
// Why runs failed on this server, by link, from GET /api/v1/sync/run-failures.
// Older servers answer 404; the page then stops asking and shows "Failed".
let runFailures = new Map();
let runFailuresAvailable = true;
let runFailuresRefreshing = false;
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
  // A failure interrupts; a confirmation waits its turn. Set before the text so
  // the change is announced with the right urgency.
  message.setAttribute("aria-live", isError ? "assertive" : "polite");
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
  if (failure.unanswered) {
    // api() has already updated the connection notice, which says why the
    // server could not be reached. This message stays behind after that notice
    // clears, so a change that got no answer is never silently forgotten. An
    // error api() did not mark is treated as a change: the cautious reading.
    const note = error?.covalentRequest === "read" ? errorCopy.UNANSWERED_READ : errorCopy.UNANSWERED_ACTION;
    say(note, true, failure.detail);
  } else {
    say(failure.summary, true, failure.detail);
  }
  message.dataset.recovery = failure.recovery;
}

// Connecting has no result of its own to report: when the server gives no
// answer, the connection notice already says so and offers "Try again", and a
// second message would only repeat it. Anything the server did answer is
// reported like every other failure.
function failToConnect(error) {
  const failure = errorCopy.describe(error);
  if (!failure.unanswered) {
    fail(error);
    return;
  }
  // api() has already raised the connection notice.
  console.debug("Covalent could not reach its server", error);
}

// One place owns "this server is not answering". Every request reports here:
// an unanswered one raises the notice, any answer at all clears it, so the
// notice follows the server's real state without a timer of its own.
function renderConnection(failure) {
  // The header's one-line state goes with the notice, so the page never says
  // "Connected" above a notice that says it cannot reach the server.
  const serverState = $("[data-node-state]");
  if (failure === null) {
    if (!connection.hidden && serverState.textContent === "Not connected") {
      serverState.textContent = serverState.dataset.lastAnswer ?? "";
    }
    connection.hidden = true;
    return;
  }
  if (serverState.textContent !== "Not connected") {
    serverState.dataset.lastAnswer = serverState.textContent;
    serverState.textContent = "Not connected";
  }
  connection.hidden = false;
  // Polling repeats the same failure every few seconds; rewriting identical
  // text would make a screen reader announce it again each time.
  if (connectionText.textContent !== failure.summary) connectionText.textContent = failure.summary;
}

async function apiResponse(path, options = {}, readJson = (response) => response.json()) {
  const headers = new Headers(options.headers || {});
  headers.set("Accept", "application/json");
  headers.set("X-Covalent-Console", "1");
  if (path === "/api/v1/sync/status" && (options.method ?? "GET").toUpperCase() === "GET") {
    headers.set("X-Covalent-Endpoint-Roster", "1");
  }
  if (token) headers.set("Authorization", `Bearer ${token}`);
  if (options.body) headers.set("Content-Type", "application/json");
  // The deadline covers the whole exchange, body included: a server that sends
  // its headers and then stalls is as silent as one that never answers.
  const deadline = requestDeadline(options);
  options = { ...options, signal: deadline.signal };
  try {
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
  } catch (error) {
    // Whatever surfaced after the deadline fired was caused by it, whether that
    // is fetch's own AbortError or a body reader objecting to its cancelled
    // stream. A status the server did send is still that status.
    if (!deadline.expired() || error instanceof NodeApiError || error instanceof ProtocolMismatchError) throw error;
    const timedOut = new Error(`no answer within ${deadline.milliseconds} ms`);
    timedOut.name = "TimeoutError";
    throw timedOut;
  } finally {
    deadline.clear();
  }
}

// How long the console waits for an answer before it stops and reports that
// the server did not answer in time. Reads match the Mac client's 20-second
// request timeout. A change must outlast the slowest answer the server can
// legitimately give, or the console reports "no answer" in place of the real
// result: saving a device address can take up to 45 seconds (the server
// contacts the device, through a QUIC handshake and idle timeout), and "Try
// folder sync again" about 30 while the transfer engine starts. 60 seconds
// covers both with room to spare.
const REQUEST_TIMEOUT_MS = Object.freeze({ read: 20000, change: 60000 });

function requestDeadline(options) {
  const milliseconds = (options.method ?? "GET").toUpperCase() === "GET"
    ? REQUEST_TIMEOUT_MS.read
    : REQUEST_TIMEOUT_MS.change;
  const controller = new AbortController();
  let expired = false;
  const timer = setTimeout(() => {
    expired = true;
    controller.abort();
  }, milliseconds);
  return {
    milliseconds,
    signal: controller.signal,
    expired: () => expired,
    clear: () => clearTimeout(timer),
  };
}

async function api(path, options = {}, readJson) {
  const change = (options.method ?? "GET").toUpperCase() !== "GET";
  const answersBefore = serverAnswers;
  // A refusal is still an answer: the server is there.
  let answered = true;
  try {
    return (await apiResponse(path, options, readJson)).body;
  } catch (error) {
    const failure = errorCopy.describe(error);
    if (failure.unanswered) {
      answered = false;
      markRequest(error, change ? "change" : "read");
      // A slow change on a server that kept answering everything else in the
      // meantime is one slow change, not a lost server. It is reported as
      // such and leaves the connection notice and the header alone.
      if (!change || serverAnswers === answersBefore) renderConnection(failure);
    }
    throw error;
  } finally {
    if (answered) serverAnswered();
  }
}

// Counts answers of any kind, so a change that timed out can tell whether the
// server went quiet or only that one request was slow.
let serverAnswers = 0;

function serverAnswered() {
  serverAnswers += 1;
  renderConnection(null);
}

// Tells fail() whether the request that got no answer was a read or a change.
function markRequest(error, kind) {
  try { error.covalentRequest = kind; } catch { /* a frozen value reads as a change */ }
}

async function loadStatus() {
  try {
    const status = await api("/api/v1/status");
    if (status.protocolVersion !== PROTOCOL_VERSION) {
      throw new ProtocolMismatchError(status.protocolVersion);
    }
    renderServerVersion(status);
    $("[data-device-name]").textContent = status.deviceName;
    $("[data-device-name]").dataset.name = status.deviceName;
    $("[data-node-state]").textContent = `${status.deviceName} · ${status.state === "ready" ? "Connected" : "Not ready yet"}`;
    document.querySelectorAll("[data-discovery]").forEach((el) => { el.textContent = status.lanDiscovery ? "On" : "Off"; });
  } catch (error) {
    const failure = errorCopy.describe(error);
    $("[data-device-name]").textContent = "Unavailable";
    // The connection notice carries the full sentence when there was no answer.
    $("[data-node-state]").textContent = failure.unanswered ? "Not connected" : failure.summary;
    failToConnect(error);
  }
}

// The version line in the footer and, when the server has seen a newer
// release, a notice that says how to install it. The server never updates
// itself, so the notice only informs; dismissing it hides it until a newer
// version than the dismissed one is announced.
const UPDATE_DISMISSED_KEY = "covalent.update-notice.dismissed";
let updateDismissedHere = null;
let updateRechecks = 0;
let updateRecheck = null;

function dismissedUpdate() {
  try { return globalThis.localStorage.getItem(UPDATE_DISMISSED_KEY) ?? updateDismissedHere; }
  catch { return updateDismissedHere; }
}

function renderServerVersion(status) {
  const about = errorCopy.serverVersion(status);
  const line = $("[data-server-version]");
  line.replaceChildren();
  line.hidden = about.version === null;
  if (about.version !== null) {
    line.append(`Covalent ${about.version}${about.note === null ? "" : ` · ${about.note}`}`);
    if (about.available?.releaseUrl) {
      const link = document.createElement("a");
      link.href = about.available.releaseUrl;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      link.textContent = "Release notes";
      line.append(" · ", link);
    }
  }
  const notice = $("[data-update-notice]");
  const available = about.available;
  notice.hidden = available === null || dismissedUpdate() === available.version;
  if (available !== null) {
    notice.dataset.version = available.version;
    $("[data-update-heading]").textContent = `Covalent ${available.version} is available`;
    $("[data-update-running]").textContent = about.version === null ? "" : `This server runs Covalent ${about.version}.`;
    $("[data-update-notes]").hidden = available.releaseUrl === null;
    if (available.releaseUrl !== null) {
      const link = $("[data-update-link]");
      link.href = available.releaseUrl;
      link.textContent = `Read the release notes for ${available.version}`;
    }
  }
  // The server's first check finishes about a minute after it starts. Look
  // again a few times rather than leave a fresh page without the answer.
  if (about.pending && updateRecheck === null && updateRechecks < 5) {
    updateRechecks += 1;
    updateRecheck = setTimeout(recheckServerVersion, 90000);
  }
}

// Quiet on purpose: this read never raises the connection notice or a
// message. If it fails, the version line simply stays as it was.
async function recheckServerVersion() {
  if (updateRecheck !== null) clearTimeout(updateRecheck);
  updateRecheck = null;
  try {
    renderServerVersion((await apiResponse("/api/v1/status")).body);
  } catch {
    // The header's Refresh reads it again.
  }
}

function folderPollingEligible() {
  const foldersSelected = $("[data-tab=folders]").getAttribute("aria-selected") === "true";
  const pairSelected = $("[data-tab=pair]").getAttribute("aria-selected") === "true";
  return Boolean(
    accessReady
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
  // With no answer from the server the connection notice gives the reason, so
  // the list only says what that means for the links shown here.
  status.textContent = failure.unanswered
    ? "Links can't be refreshed until this server answers. Anything shown below may be out of date."
    : failure.summary;
  status.className = "folder-state";
  status.dataset.kind = "attention";
  status.hidden = false;
  $("[data-folders-refresh]").hidden = false;
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
    ?? "Paired device unavailable";
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
  cadenceLabel.textContent = "Timing ";
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
    succeeded: "Completed",
    failed: "Failed",
    timedOut: "Incomplete after 24 hours",
    interrupted: "Interrupted",
    cancelled: "Cancelled",
  })[result];
}

function runDestinationName(status, peerId) {
  if (peerId === folderDeviceId) return "This server";
  return status.peers.find((peer) => peer.peerId === peerId)?.displayName ?? "Another recipient";
}

function renderLinkRun(container, status, share) {
  const run = share.linkRun;
  if (run === null || !share.linkSettings.confirmed) return;
  const section = document.createElement("details");
  section.className = "link-run";
  const heading = document.createElement("summary");
  heading.textContent = run.phase === "running" ? "Transfer in progress" : "Transfer details";
  const summary = document.createElement("p");
  summary.setAttribute("role", "status");
  if (run.pendingRequest !== null) summary.textContent = "Run requested. Waiting for the source to come online and start it.";
  else if (run.phase === "preparing") summary.textContent = "Preparing this run and checking source files.";
  else if (run.phase === "running") summary.textContent = "This run is in progress.";
  else if (run.phase === "succeeded") summary.textContent = "The last run completed for every recipient.";
  else if (run.phase === "incomplete") summary.textContent = "The last run did not finish for every recipient. Check each one below, then choose Run Now when ready.";
  else if (run.phase === "interrupted") summary.textContent = interruptedRunCopy(share);
  else if (run.phase === "cancelled") summary.textContent = "The last run was cancelled before it finished.";
  else if (share.linkSettings.settings.cadence.mode === "manual") {
    summary.textContent = "Ready. Nothing is copied until someone on this link chooses Run Now.";
  } else if (share.linkSettings.settings.cadence.mode === "continuous") {
    summary.textContent = "Ready. Changes are copied as they happen.";
  } else summary.textContent = "Ready. The next run starts on schedule.";
  section.append(heading, summary);

  if (share.linkSettings.settings.cadence.mode === "scheduled") {
    const schedule = document.createElement("p");
    schedule.className = "muted";
    schedule.textContent = run.nextDueAtUnixMs === null
      ? "The source keeps this schedule. The next run time is not known yet."
      : `Next run: ${new Date(run.nextDueAtUnixMs).toLocaleString()}.`;
    section.append(schedule);
  }
  if (run.destinations.length > 0) {
    const destinations = document.createElement("ul");
    for (const destination of run.destinations) {
      const item = document.createElement("li");
      item.textContent = `${runDestinationName(status, destination.peerId)}: ${runResultCopy(destination.result)}`;
      if (destination.result === "failed") {
        const why = failedRecipientCopy(status, share, destination.peerId);
        if (why !== null) item.textContent += `. ${why}`;
      }
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
      "Run Now requested. Link status shows progress for every recipient.",
    );
  });
  button.dataset.linkRunButton = "";
  button.disabled = button.disabled || active || share.linkSettings.settings.paused
    || (saved !== null && saved.folderId !== share.folderId);
  container.append(button, section);
}

// This server's own failure for the link's current run, if the server
// explained it. An entry for an earlier run is about that run, not this one.
function runFailureFor(share, role) {
  const failure = runFailures.get(share.folderId);
  return failure !== undefined && failure.role === role && failure.generation === share.linkRun?.generation
    ? failure : null;
}

// Why one recipient's copy failed. Only this server's own failure can be
// explained here, because the cause is never sent between devices; for
// another recipient the page says where to look instead.
function failedRecipientCopy(status, share, peerId) {
  if (peerId === folderDeviceId) {
    const failure = runFailureFor(share, "destination");
    return failure === null ? null : folderSync.runFailureCopy(failure.code, share.linkSettings.settings.cadence.mode);
  }
  const name = status.peers.find((peer) => peer.peerId === peerId)?.displayName;
  return name === undefined ? "Open Covalent on that device to see why." : `Open Covalent on ${name} to see why.`;
}

function interruptedRunCopy(share) {
  const failure = runFailureFor(share, "source");
  return failure === null
    ? "The last run was interrupted before it finished. Choose Run Now when ready."
    : `The last run was interrupted before it finished. ${folderSync.runFailureCopy(failure.code, share.linkSettings.settings.cadence.mode)}`;
}

function refreshLinkRun(container, status, share) {
  const state = JSON.stringify([share.linkRun, share.linkSettings, folderController.pendingRun(), runFailures.get(share.folderId) ?? null]);
  if (container.dataset.runState === state) return;
  const previous = container.querySelector(".link-run");
  const button = container.querySelector("[data-link-run-button]");
  const focused = button !== null && document.activeElement === button;
  const open = previous?.open ?? false;
  previous?.remove();
  button?.remove();
  if (share.linkSettings !== null && firstLinkMember(status, share)) renderLinkRun(container, status, share);
  const updated = container.querySelector(".link-run");
  if (updated) updated.open = open;
  if (focused) container.querySelector("[data-link-run-button]")?.focus();
  container.dataset.runState = state;
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
    current.textContent = "Waiting for the source to confirm these settings. Nothing has been copied yet.";
    details.append(current);
  }

  if (state.pendingChange !== null) {
    const pending = document.createElement("p");
    pending.setAttribute("role", "status");
    pending.textContent = `Waiting for the source to confirm this change: ${folderSync.settingsExplanation(state.pendingChange.settings)}`;
    details.append(pending);
  }
  if (state.conflictedChange !== null) {
    const conflict = document.createElement("p");
    conflict.setAttribute("role", "alert");
    conflict.textContent = `A change based on older settings was not applied: ${folderSync.settingsExplanation(state.conflictedChange.settings)} Check the current settings below before saving again.`;
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
  submit.textContent = state.conflictedChange === null ? "Save link settings" : "Save reviewed settings";
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
    if (enablesDeletion && !globalThis.confirm(folderSync.deletionConfirmation(policy, share.label, "change"))) return;
    void runFolderMutation(
      () => folderController.updateLinkSettings(share.folderId, settings),
      "Settings change sent. It applies once the source confirms it.",
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
  deletionHelp.textContent = "With both options off, deleting a source file keeps its destination copies, and a file deleted at a destination is not copied there again.";
  const android = document.createElement("details");
  android.className = "android-conditions";
  const androidSummary = document.createElement("summary");
  androidSummary.textContent = "Android conditions";
  android.append(androidSummary, androidHelp, wifi, charging);
  form.append(group("When this link runs", timing.cadenceLabel, timing.intervalLabel),
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
  details.className = "add-recipient";
  const summary = document.createElement("summary");
  summary.textContent = "Add recipient";
  const explanation = document.createElement("p");
  explanation.className = "muted";
  explanation.textContent = "Choose a paired device. It joins this link with the same timing and settings.";
  details.append(summary, explanation);
  if (peers.length === 0) {
    const empty = document.createElement("p");
    empty.textContent = "Pair another device to add it to this link.";
    const pairButton = document.createElement("button");
    pairButton.type = "button";
    pairButton.textContent = "Pair a device";
    pairButton.addEventListener("click", () => { $("[data-tab=pair]").click(); $("#network-pair-address").focus(); });
    details.append(empty, pairButton);
    container.append(details);
    return;
  }
  const form = document.createElement("form");
  form.className = "inline-form";
  const peerLabel = document.createElement("label");
  peerLabel.textContent = "Recipient ";
  const select = document.createElement("select");
  select.name = "peerId";
  select.required = true;
  const placeholder = document.createElement("option");
  placeholder.value = "";
  placeholder.textContent = "Choose a paired device";
  select.append(placeholder);
  for (const peer of peers) {
    const option = document.createElement("option");
    option.value = peer.peerId;
    option.textContent = peer.displayName;
    select.append(option);
  }
  peerLabel.append(select);
  const submit = document.createElement("button");
  submit.dataset.folderMutation = "";
  submit.textContent = "Add recipient";
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const body = {
      peerId: select.value,
      folderId: share.folderId,
      label: share.label,
      linkPolicy: share.linkPolicy,
      cadence: share.linkSettings.settings.cadence,
      androidConditions: share.linkSettings.settings.androidConditions,
    };
    void runFolderMutation(
      () => folderController.sendOffer(body),
      "Invitation sent. Open Covalent on that device to accept it and choose its destination folder.",
    );
  });
  form.append(peerLabel, submit);
  details.append(form);
  container.append(details);
}

function renderSharedFolderActions(container, status, share) {
  const firstMember = share.linkSettings === null || firstLinkMember(status, share);
  if (firstMember && !share.expired && !(share.incoming && share.phase === "offered")) {
    const paused = share.phase === "paused";
    const pause = folderActionButton(share.linkPolicy === null
      ? (paused ? "Resume" : "Pause") : (paused ? "Resume link" : "Pause link"), () => {
      void runFolderMutation(
        () => folderController.pause(share.offerId, !paused),
        share.linkPolicy === null ? (paused ? "Folder sync resumed." : "Folder sync paused.")
          : "Pause change sent. It applies to every device once the source confirms it.",
      );
    });
    container.append(pause);
  }

  if (share.linkSettings !== null && firstMember) {
    renderLinkSettings(container, status, share);
    if (!share.incoming && share.linkSettings.confirmed && ["ready", "paused"].includes(share.phase)) {
      renderAddDestination(container, status, share);
    }
  }

}

function renderFolderActions(container, status, share, view) {
  if (share.phase === "removed") return;
  if (share.incoming && share.phase === "offered" && !share.expired) {
    const pathLabel = document.createElement("label");
    pathLabel.textContent = "Destination folder on this server";
    const path = document.createElement("input");
    path.value = "/sync";
    path.maxLength = 1024;
    path.required = true;
    pathLabel.append(path);
    const accept = folderActionButton("Accept", () => {
      void runFolderMutation(
        () => folderController.accept(share.offerId, path.value),
        "Invitation accepted. Covalent is checking the destination folder.",
      );
    }, "");
    container.append(pathLabel, accept);
  }

  if (share.expired && !share.incoming && ["offered", "paused"].includes(share.phase)) {
    container.append(folderActionButton("Send new invitation", () => {
      void runFolderMutation(
        () => folderController.renew(share.offerId),
        "New invitation sent. Accept it on the other device to start this link.",
      );
    }));
  }

  // Each wording says what stops, what is kept, and on which device, because
  // "remove" could otherwise be read as deleting the files themselves.
  const removal = share.incoming && share.phase === "offered" && !share.expired ? {
    open: "Decline",
    question: `Decline the invitation to ${share.label}? Nothing has been copied to this server yet, and nothing is deleted on any device. The source can send a new invitation later.`,
    keep: "Keep invitation",
    confirm: "Decline invitation",
    done: "Invitation declined. Nothing was copied or deleted.",
  } : share.incoming ? {
    open: "Stop receiving…",
    question: `Stop receiving ${share.label} on this server? Covalent stops copying files here now, and the source stops sending when it next connects. Files already in the destination folder stay there, and nothing is deleted on the source.`,
    keep: "Keep receiving",
    confirm: "Stop receiving",
    done: "This server no longer receives this link. Files already copied stay on both devices.",
  } : {
    open: "Remove recipient…",
    question: `Remove ${folderPeerName(status, share.peerId)} from ${share.label}? Covalent stops copying to that device: here now, and there when it next connects. Files already copied stay on that device, and nothing is deleted from the source folder.`,
    keep: "Keep recipient",
    confirm: "Remove recipient",
    done: "Recipient removed. Files already copied stay on both devices.",
  };
  const confirmation = document.createElement("div");
  confirmation.className = "folder-removal-confirmation";
  confirmation.hidden = true;
  confirmation.setAttribute("role", "group");
  confirmation.setAttribute("aria-label", `${removal.confirm}: ${share.label}`);
  const explanation = document.createElement("p");
  explanation.textContent = removal.question;
  const remove = folderActionButton(removal.open, () => {
    confirmation.hidden = false;
    cancel.focus();
  }, "quiet");
  const cancel = folderActionButton(removal.keep, () => {
    confirmation.hidden = true;
    remove.focus();
  }, "quiet");
  const confirmed = folderActionButton(removal.confirm, () => {
    void runFolderMutation(() => folderController.remove(share.offerId), removal.done);
  }, "quiet");
  confirmation.append(explanation, cancel, confirmed);
  container.append(remove, confirmation);

  if (view.kind === "expired") {
    const guidance = document.createElement("p");
    guidance.textContent = share.incoming
      ? `Ask ${folderPeerName(status, share.peerId)} to send a new invitation, then choose your destination folder again.`
      : "Send a new invitation so the other device can accept it and choose its destination folder again.";
    container.append(guidance);
  }
}

function renderFolderPeers(status) {
  const select = $("[data-folder-peer]");
  const previous = select.value;
  const labels = status.peers.map((peer) => [peer.peerId, peer.displayName]);
  const rendered = Array.from(select.options).slice(1).map((option) => [option.value, option.textContent]);
  const placeholderText = status.peers.length === 0
    ? "No paired devices yet"
    : "Choose a paired device";
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
  label.textContent = "New device address";
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
  explanation.textContent = "Enter a numeric IP address and port. Covalent contacts that address and saves it only if the same paired device answers.";
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
      : "Trying the same address again…";
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
        submit.textContent = "Try this address again";
        state.textContent = "This update is not confirmed yet. Try this address again, or cancel before entering a different one.";
      } else if (/saved address changed/.test(error?.covalentGuidance ?? "")) {
        state.textContent = "The saved address changed. Check the current address before saving again.";
      } else {
        state.textContent = "This update is not confirmed. Refresh, then check this device’s address before trying again.";
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
      ? " — Address not available"
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
  if (status.peers.length === 0) empty.textContent = "No paired devices yet. Pair a device above, then add it to a link.";
}

function renderFolderStatus(status) {
  const ownDeviceName = $("[data-device-name]").dataset.name;
  const summary = folderSync.statusSummary(status);
  const statusCopy = $("[data-folders-status]");
  statusCopy.textContent = summary.text;
  statusCopy.className = "folder-state";
  statusCopy.dataset.kind = summary.kind;
  statusCopy.hidden = summary.kind === "ready" && status.shares.length > 0;
  $("[data-folders-refresh]").hidden = true;
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
  const groups = folderSync.groupShares(visibleShares);
  const retained = new Map(Array.from(list.children).map((item) => [item.dataset.folderId, item]));
  for (const [id, item] of retained) if (!groups.some((group) => group[0].folderId === id)) item.remove();
  for (const [index, members] of groups.entries()) {
    const share = members.find((member) => member.phase !== "removed") ?? members[0];
    let item = retained.get(share.folderId);
    if (!item) {
      item = document.createElement("li");
      item.dataset.folderId = share.folderId;
      const heading = document.createElement("div");
      heading.className = "share-heading";
      const name = document.createElement("h3");
      const description = document.createElement("p");
      description.className = "muted";
      heading.append(name, description);
      const recipients = document.createElement("ul");
      recipients.className = "recipient-list";
      recipients.setAttribute("aria-label", "Recipients");
      const actions = document.createElement("div");
      actions.className = "folder-actions";
      item.append(heading, recipients, actions);
    }
    const [heading, recipients, actions] = item.children;
    heading.children[0].textContent = share.label;
    const cadence = share.linkSettings?.settings.cadence;
    const timing = !cadence ? "Legacy two-way sync" : cadence.mode === "manual" ? "Manual"
      : cadence.mode === "continuous" ? "Continuous" : cadence.intervalMinutes === 60 ? "Every hour"
        : cadence.intervalMinutes === 1440 ? "Every day" : `Every ${cadence.intervalMinutes} minutes`;
    const sourceName = share.endpointRoster?.source.displayName
      ?? (share.incoming ? folderPeerName(status, share.peerId) : ownDeviceName || "this server");
    heading.children[1].textContent = `From ${sourceName} · ${timing}${share.linkSettings?.settings.paused ? " · Paused" : ""}`;
    const recipientRows = folderSync.shareRecipients(status, members, folderDeviceId, ownDeviceName);
    const retainedRecipients = new Map(Array.from(recipients.children).map((row) => [row.dataset.recipientKey, row]));
    for (const [id, row] of retainedRecipients) if (!recipientRows.some((recipient) => recipient.key === id)) row.remove();
    for (const [recipientIndex, recipient] of recipientRows.entries()) {
      const member = recipient.member;
      const view = member === null ? { kind: "info", text: "Receives this link" } : folderSync.shareView(status, member);
      let row = retainedRecipients.get(recipient.key);
      if (!row) {
        row = document.createElement("li");
        row.dataset.recipientKey = recipient.key;
        const identity = document.createElement("div");
        identity.className = "recipient-identity";
        identity.append(document.createElement("strong"), document.createElement("span"));
        const controls = document.createElement("div");
        controls.className = "recipient-actions";
        row.append(identity, controls);
      }
      row.children[0].children[0].textContent = recipient.displayName;
      const state = row.children[0].children[1];
      state.className = "folder-state";
      state.dataset.kind = view.kind;
      state.textContent = view.text;
      const controls = row.children[1];
      const memberState = member === null ? "display-only" : JSON.stringify([member.offerId, member.incoming, member.phase, member.expired, view.kind]);
      if (controls.dataset.state !== memberState) {
        controls.replaceChildren();
        if (member !== null) renderFolderActions(controls, status, member, view);
        controls.dataset.state = memberState;
      }
      if (recipients.children[recipientIndex] !== row) recipients.insertBefore(row, recipients.children[recipientIndex] ?? null);
    }
    // Preserve typed fields and focus while routine health polling updates text.
    const actionState = JSON.stringify([
      folderDeviceId, share.incoming, share.phase, share.expired, share.linkSettings,
      members.map((member) => member.peerId), status.peers.map((peer) => [peer.peerId, peer.displayName]),
      savedSettings?.folderId === share.folderId ? savedSettings : null,
    ]);
    if (actions.dataset.state !== actionState) {
      actions.replaceChildren();
      renderSharedFolderActions(actions, status, share);
      actions.dataset.state = actionState;
      delete actions.dataset.runState;
    }
    refreshLinkRun(actions, status, share);
    if (list.children[index] !== item) list.insertBefore(item, list.children[index] ?? null);
  }
  const empty = $("[data-folders-empty]");
  empty.hidden = visibleShares.length > 0;
  if (visibleShares.length === 0) {
    empty.textContent = status.availability === "available"
      ? status.peers.length === 0
        ? "A link copies a folder to a paired device. Choose Pair device to connect a Mac, an Android device, or another server."
        : "No links yet. Choose New link to copy a folder to a paired device."
      : "Links cannot be loaded while folder sync is offline on this server.";
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
    : "the saved device";
  $("[data-folder-pending-summary]").textContent = `${pending.label} for ${peerName}${pending.selectedRoot ? `, using ${pending.selectedRoot}` : ", using the existing source folder"}.`;
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
  $("[data-link-run-pending-summary]").textContent = `${share?.label ?? "This link"}: send the same request again, or discard it after checking the link’s status.`;
}

async function loadFolders(reportError = false) {
  try {
    const result = await folderController.refresh();
    if (result.busy) {
      const status = $("[data-folders-status]");
      status.textContent = "Updating status…";
      status.className = "folder-state";
      status.dataset.kind = "checking";
      status.hidden = false;
    }
    if (result.applied) {
      renderPendingFolderOffer();
      renderPendingLinkSettings();
      renderPendingLinkRun();
      // Not awaited: a slow answer here must never hold up link status.
      void refreshRunFailures();
    }
  } catch (error) {
    renderFolderError(error);
    if (reportError) fail(error);
  }
}

function runFailuresWanted(status) {
  return status.shares.some((share) => share.linkRun !== null && (share.linkRun.phase === "interrupted"
    || share.linkRun.destinations.some((destination) => destination.result === "failed")));
}

// Asked only while a link shows a failed or interrupted run. Quiet on
// purpose: it never raises the connection notice or a message, and when it
// fails the page keeps showing "Failed" alone, as before.
async function refreshRunFailures() {
  const status = folderController.current();
  if (runFailuresRefreshing || !runFailuresAvailable || status === null || !runFailuresWanted(status)) return;
  runFailuresRefreshing = true;
  const device = folderDeviceId;
  let next;
  try {
    next = folderSync.runFailures((await apiResponse("/api/v1/sync/run-failures", {}, folderSync.readJson)).body);
  } catch (error) {
    // An older server has no such route. Stop asking it for this page.
    if (error instanceof NodeApiError && error.status === 404) runFailuresAvailable = false;
    return;
  } finally {
    runFailuresRefreshing = false;
  }
  if (folderDeviceId !== device || JSON.stringify([...next]) === JSON.stringify([...runFailures])) return;
  runFailures = next;
  const current = folderController.current();
  if (current !== null) renderFolderStatus(current);
}

async function initializeFolderSync() {
  const identity = await folderApi("/api/v1/transport/identity");
  folderDeviceId = identity?.deviceId ?? null;
  const hostname = globalThis.location.hostname;
  if (Number.isInteger(identity?.peerPort) && identity.peerPort > 0 && identity.peerPort <= 65535
    && !["localhost", "127.0.0.1", "[::1]"].includes(hostname)) {
    $("[data-own-address]").textContent = `${hostname}:${identity.peerPort}`;
    $("[data-own-address-panel]").hidden = false;
  }
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
  runFailures = new Map();
  runFailuresAvailable = true;
  folderController.setAccess({ deviceId: null, unlocked: false });
  folderController.setPollingEnabled(false);
  // A locked console can neither refresh nor act on links, so it shows none
  // rather than a list that looks current and is not.
  $("[data-folders-list]").replaceChildren();
  $("[data-folders-empty]").hidden = true;
  $("[data-folders-refresh]").hidden = true;
  const linksStatus = $("[data-folders-status]");
  linksStatus.textContent = "Unlock the console to see your links.";
  linksStatus.className = "muted";
  delete linksStatus.dataset.kind;
  linksStatus.hidden = false;
  $("[data-link-settings-pending]").hidden = true;
  $("[data-link-run-pending]").hidden = true;
  $("[data-paired-devices]").replaceChildren();
  const empty = $("[data-paired-devices-empty]");
  empty.hidden = false;
  empty.textContent = "Unlock the console to load paired devices.";
}

function requireUnlocked() {
  if (accessReady) return true;
  $("[data-access-panel]").open = true;
  $("#api-token").focus();
  say("Unlock the console with the server access token first.", true);
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
    const name = document.createElement("strong");
    name.textContent = candidate.name ?? candidate.endpoint.replace(/:\d+$/, "");
    const address = document.createElement("span");
    address.className = "muted";
    address.textContent = candidate.name ? `${candidate.where} · ${candidate.endpoint}` : candidate.where;
    label.className = "candidate-identity";
    label.append(name, address);
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
  networkPoll = setInterval(pollNetworkPairings, 3000);
}

// Background refreshes never overlap themselves. A server that takes longer
// than the polling interval to answer would otherwise collect a queue of
// requests, and for links each newer request would discard the answer to the
// one before it, so the page would never update at all.
let networkPairingsRefreshing = false;
let foldersRefreshing = false;

function pollNetworkPairings() {
  if (networkPairingsRefreshing) return;
  networkPairingsRefreshing = true;
  // Nothing to report here: an unanswered poll already raised the connection
  // notice, and the next poll tries again.
  refreshNetworkPairings().catch(() => {}).finally(() => { networkPairingsRefreshing = false; });
}

function pollFolders() {
  if (foldersRefreshing) return;
  foldersRefreshing = true;
  loadFolders(false).finally(() => { foldersRefreshing = false; });
}

function renderNetworkPairing(item) {
  networkPairing = item;
  const review = $("[data-review-pairing]");
  review.hidden = item === null || item.direction !== "incoming" || item.state !== "awaiting_local_confirmation";
  if (!review.hidden) review.textContent = `Pairing request from ${item.peerName} — review code`;
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
  code.setAttribute("aria-label", `Confirmation code ${details.spokenCode}`);
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
  if (!accessReady) return;
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
    say("Compare the confirmation code below on both devices before confirming.");
  } catch (error) { fail(error); }
}

function initializeConsoleAccess() {
  return errorCopy.initializeUnlockedConsole({
    authorize: async () => {
      await api("/api/v1/config/export", { method: "POST" });
      accessReady = true;
      // The server may report its version and update check only to an
      // unlocked console, so read them again now that this one is.
      void recheckServerVersion();
    },
    initializeLinks: initializeFolderSync,
    refreshPairings: refreshNetworkPairings,
    onLinksError(error) { clearFolderSyncAccess(); renderFolderError(error); },
  });
}

async function tryTrustedAccess() {
  try {
    await initializeConsoleAccess();
    $("[data-access-panel]").hidden = true;
    $("[data-access-state]").hidden = false;
    $("[data-access-state]").textContent = "Trusted access";
  } catch (error) {
    if (accessReady) {
      $("[data-access-state]").hidden = false;
      $("[data-access-state]").textContent = "Trusted access";
      failToConnect(error);
      return;
    }
    accessReady = false;
    clearFolderSyncAccess();
    $("[data-access-panel]").hidden = false;
    $("[data-access-panel]").open = true;
    $("[data-access-state]").hidden = false;
    $("[data-access-state]").textContent = "Console locked";
    if (!(error instanceof NodeApiError && (error.status === 401 || error.status === 403))) failToConnect(error);
  }
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
    say("That file is not a server access token. Choose the file named local-api-token that covalent claim created, not a setup code or a settings file.", true);
  } finally {
    input.value = "";
  }
});

$("[data-token-form]").addEventListener("submit", async (event) => {
  event.preventDefault();
  const submit = $("[data-token-submit]");
  // One attempt at a time: a slow server must not be sent the token twice.
  if (submit.disabled) return;
  token = formData(event.currentTarget).get("token").trim();
  submit.disabled = true;
  submit.textContent = "Unlocking…";
  try {
    await initializeConsoleAccess();
    $("[data-access-panel]").open = false;
    $("[data-access-state]").textContent = "Unlocked in this tab";
    $("[data-access-state]").hidden = false;
    $("[data-access-guidance]").textContent = "Unlocked for this tab";
    // Keep the credential only in the existing in-memory token variable.
    $("#api-token").value = "";
    $("[data-tab=folders]").focus();
    say("Console unlocked for this tab only.", false, null, true);
  }
  catch (error) {
    token = "";
    accessReady = false;
    $("[data-access-panel]").open = true;
    $("[data-access-state]").textContent = "Console locked";
    $("[data-access-guidance]").textContent = "Enter the server access token";
    clearFolderSyncAccess();
    failToConnect(error);
  } finally {
    submit.disabled = false;
    submit.textContent = "Unlock console";
  }
});

// The header's Refresh and the connection notice's "Try again" ask the same
// thing: load everything this page shows from the server again.
$("[data-refresh]").addEventListener("click", (event) => { void reloadConsole(event.currentTarget); });
$("[data-connection-retry]").addEventListener("click", (event) => { void reloadConsole(event.currentTarget); });

async function reloadConsole(control) {
  if (control.getAttribute("aria-busy") === "true") return;
  control.setAttribute("aria-busy", "true");
  const idleLabel = control.textContent;
  if (control.dataset.busyLabel) control.textContent = control.dataset.busyLabel;
  try {
    await loadStatus();
    // A console that is locked asks for trusted access again, so recovering
    // from a server that was unreachable when the page opened does not need a
    // page reload. A server that wants a token simply shows the token step.
    if (!accessReady && token === "") await tryTrustedAccess();
    if (!accessReady) return;
    if (folderDeviceId === null) await retryFolderSync();
    await Promise.all([loadFolders(false), refreshNetworkPairings()]);
  } catch (error) {
    failToConnect(error);
  } finally {
    control.removeAttribute("aria-busy");
    if (control.dataset.busyLabel) control.textContent = idleLabel;
    // "Try again" disappears with its notice once the server answers. Hand its
    // focus to the current section's tab instead of dropping it on the page.
    const focusLost = document.activeElement === control || document.activeElement === document.body;
    if (control.closest("[hidden]") !== null && focusLost) $('[data-tab][aria-selected="true"]').focus();
  }
}

// Access was granted but the links never loaded, because the server stopped
// answering in between. Loading them again is the retry.
async function retryFolderSync() {
  try {
    await initializeFolderSync();
  } catch (error) {
    clearFolderSyncAccess();
    renderFolderError(error);
  }
}

$("[data-folders-refresh]").addEventListener("click", async () => {
  if (!requireUnlocked()) return;
  if (folderDeviceId === null) {
    await retryFolderSync();
    return;
  }
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
    && !globalThis.confirm(folderSync.deletionConfirmation(body.linkPolicy, body.label, "create"))) return;
  void runFolderMutation(async () => {
    await folderController.sendOffer(body);
    form.reset();
    form.elements.selectedRoot.value = "/sync";
    syncOfferScheduleInput();
    $("[data-create-link]").open = false;
    $("[data-open-create]").focus();
  }, "Invitation sent. Open Covalent on that device to accept it and choose its destination folder.");
});
$("[data-folder-offer-retry]").addEventListener("click", () => {
  void runFolderMutation(() => folderController.retryPendingOffer(), "Invitation sent.");
});
$("[data-folder-offer-discard]").addEventListener("click", () => {
  if (!globalThis.confirm([
    "Discard the saved invitation?",
    "Removed: only the copy of this request that this browser kept so it could be sent again.",
    "Not removed: if the invitation already reached this server, it stays there and appears under Your links, where you can remove that recipient. No link, folder or file is deleted.",
  ].join("\n\n"))) return;
  try {
    folderController.discardPendingOffer();
    renderPendingFolderOffer();
    say("Saved invitation discarded. Nothing was removed from this server.");
  } catch (error) { fail(error); }
});
$('[data-link-settings-retry]').addEventListener("click", () => {
  void runFolderMutation(
    () => folderController.retryPendingLinkSettings(),
    "Settings change confirmed with this server.",
  );
});
$('[data-link-settings-discard]').addEventListener("click", () => {
  if (!globalThis.confirm([
    "Discard the saved settings change?",
    "Removed: only the copy of this change that this browser kept so it could be sent again.",
    "Not undone: if the change already reached this server, it stays in effect, so check the link’s current settings afterwards. No link, folder or file is deleted.",
  ].join("\n\n"))) return;
  try {
    folderController.discardPendingLinkSettings();
    renderPendingLinkSettings();
    say("Saved settings change discarded. The link’s settings on this server were left as they are.");
  } catch (error) { fail(error); }
});
$('[data-link-run-retry]').addEventListener("click", () => {
  void runFolderMutation(
    () => folderController.retryPendingRun(),
    "Run Now confirmed with this server.",
  );
});
$('[data-link-run-discard]').addEventListener("click", () => {
  if (!globalThis.confirm([
    "Discard the saved Run Now request?",
    "Removed: only the copy of this request that this browser kept so it could be sent again.",
    "Not cancelled: if the request already reached the source, that run still goes ahead. No link, folder or file is deleted.",
  ].join("\n\n"))) return;
  try {
    folderController.discardPendingRun();
    renderPendingLinkRun();
    say("Saved Run Now request discarded. No new request was sent, and no run was cancelled.");
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
  if (folderPollingEligible()) {
    pollFolders();
    if (networkPoll === null) pollNetworkPairings();
  }
}, 5000);

$("[data-network-discover]").addEventListener("click", async (event) => {
  if (!requireUnlocked()) return;
  const button = event.currentTarget;
  const state = $("[data-network-discovery-state]");
  button.disabled = true;
  state.textContent = "Finding devices…";
  try {
    const result = await pairing.network.discovery(api);
    renderNetworkCandidates(result.candidates);
    const notes = [];
    if (result.tailscale === "unavailable") notes.push("Tailscale discovery isn’t connected to this server. Enter a Tailscale name above.");
    if (result.tailscale === "stale") notes.push("The Tailscale device list is out of date. Enter a name above or try again shortly.");
    if (result.tailscale === "error") notes.push("Tailscale discovery could not load. You can still pair by name above.");
    if (result.lan === "disabled") notes.push("Nearby discovery is off.");
    if (result.lan === "error") notes.push("Nearby discovery could not load.");
    state.textContent = notes.join(" ") || `${result.candidates.length} ${result.candidates.length === 1 ? "device" : "devices"} found.`;
    await refreshNetworkPairings();
  } catch (error) { state.textContent = "Discovery could not finish. Enter a device address above."; fail(error); }
  finally { button.disabled = false; }
});

$("[data-copy-address]").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText($("[data-own-address]").textContent);
    say("Device address copied.", false, null, true);
  } catch { say("Select the device address and copy it from your browser."); }
});
$("[data-review-pairing]").addEventListener("click", () => {
  $("[data-tab=pair]").click();
  $("[data-network-card]").scrollIntoView({ block: "start" });
  $("[data-network-confirm]").focus();
});

$("[data-network-manual]").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.currentTarget;
  const button = form.querySelector("button");
  button.disabled = true;
  button.textContent = "Sending pairing request…";
  try { await startNetworkPairing(formData(form).get("candidateAddress")); }
  finally { button.disabled = false; button.textContent = "Send pairing request"; }
});

$("[data-network-confirm]").addEventListener("click", async () => {
  if (networkPairing === null || !requireUnlocked()) return;
  try {
    const confirmed = await pairing.network.confirm(api, networkPairing);
    renderNetworkPairing(confirmed);
    if (confirmed.state === "complete") await loadFolders(false);
    say(confirmed.state === "complete"
      ? "Device paired. You can now add it to a one-way link."
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
    say(settled ? "Pairing request dismissed." : "Pairing request cancelled. Nothing was trusted.");
  }
  catch (error) { fail(error); }
});

$("[data-settings-export]").addEventListener("click", async () => {
  try { const settings = await api("/api/v1/config/export", { method: "POST" }); const blob = new Blob([JSON.stringify(settings, null, 2)], { type: "application/json" }); const link = Object.assign(document.createElement("a"), { href: URL.createObjectURL(blob), download: "covalent-settings.json" }); link.click(); URL.revokeObjectURL(link.href); say("Safe settings downloaded."); }
  catch (error) { fail(error); }
});
$("[data-settings-import]").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.currentTarget;
  const submit = form.querySelector("button");
  // One import at a time: a slow server must not be sent the same settings twice.
  if (submit.disabled) return;
  const data = formData(form);
  submit.disabled = true;
  try { await api("/api/v1/config/import", { method: "POST", body: JSON.stringify({ confirmed: data.get("confirmed") === "on", settings: JSON.parse(data.get("settings")) }) }); await loadStatus(); say("Safe settings imported. Nearby discovery was refreshed from this server."); }
  catch (error) { fail(error); }
  finally { submit.disabled = false; }
});

$("[data-update-dismiss]").addEventListener("click", () => {
  const notice = $("[data-update-notice]");
  updateDismissedHere = notice.dataset.version ?? null;
  try { globalThis.localStorage.setItem(UPDATE_DISMISSED_KEY, updateDismissedHere ?? ""); }
  catch { /* Stays dismissed for this page only. */ }
  notice.hidden = true;
  // The button is gone; keep keyboard focus somewhere sensible.
  $('[data-tab][aria-selected="true"]').focus();
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

void tryTrustedAccess();
loadStatus();
}
