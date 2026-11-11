import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import test from "node:test";

const require = createRequire(import.meta.url);
const copy = require("../app.js");
const folders = require("../folder-sync-flow.js");
const root = new URL("../", import.meta.url);

function source(name) {
  return readFile(new URL(name, root), "utf8");
}

function between(text, start, end) {
  const from = text.indexOf(start);
  const to = text.indexOf(end, from);
  assert.ok(from >= 0 && to > from, `could not find ${JSON.stringify(start)} … ${JSON.stringify(end)}`);
  return text.slice(from, to);
}

function named(name) {
  return Object.assign(new Error("diagnostic text nobody should read"), { name });
}

function refusal(status, code) {
  return Object.assign(new Error("x"), { covalentFailureKind: "api", status, code, serverMessage: "" });
}

// Timers the test fires by hand, so a 30-second deadline costs no time.
function manualTimers() {
  const pending = new Map();
  let next = 1;
  return {
    pending,
    setTimeout(callback, milliseconds) {
      const id = next;
      next += 1;
      pending.set(id, { callback, milliseconds });
      return id;
    },
    clearTimeout(id) { pending.delete(id); },
    fire() {
      const [[id, timer]] = pending;
      pending.delete(id);
      timer.callback();
    },
  };
}

class ApiError extends Error {
  constructor(status, body) {
    super(`http ${status}`);
    this.covalentFailureKind = "api";
    this.status = status;
    this.code = body.code ?? `http_${status}`;
    this.serverMessage = "";
  }
}

class ProtocolError extends Error {}

async function consoleRequest(fetch, timers) {
  const app = await source("app.js");
  const responseSource = between(app, "async function apiResponse(", "\nasync function api(");
  return new Function(
    "Headers", "fetch", "token", "PROTOCOL_VERSION", "NodeApiError", "ProtocolMismatchError", "setTimeout", "clearTimeout",
    `${responseSource}\nreturn apiResponse;`,
  )(Headers, fetch, "", 1, ApiError, ProtocolError, timers.setTimeout, timers.clearTimeout);
}

// A request that only ever ends because it was abandoned.
function untilAborted(signal, failure = named("AbortError")) {
  return new Promise((_, reject) => signal.addEventListener("abort", () => reject(failure)));
}

async function settle() {
  for (let turn = 0; turn < 5; turn += 1) await Promise.resolve();
}

test("only a server that gave no answer is described as unanswered", () => {
  const silent = [
    [new TypeError("Failed to fetch"), { online: true }],
    [new TypeError("Failed to fetch"), { online: false }],
    [named("AbortError"), {}],
    [named("TimeoutError"), {}],
    [named("SecurityError"), {}],
  ];
  for (const [error, context] of silent) {
    assert.equal(copy.describe(error, context).unanswered, true, error.name);
  }
  assert.equal(
    copy.describe(named("TimeoutError")).summary,
    "This server didn't answer in time. Make sure it's turned on and awake, then try again.",
  );

  const answered = [
    refusal(401, "authentication_required"),
    refusal(503, "node_busy"),
    refusal(599, "a_code_from_a_newer_server"),
    Object.assign(new Error("x"), { covalentFailureKind: "protocol", observedVersion: 9 }),
    folders.guidance("Another change is still in progress."),
    new SyntaxError("Unexpected token"),
    new RangeError("boom"),
  ];
  for (const error of answered) {
    assert.equal(copy.describe(error, { online: true }).unanswered, false, error.name);
  }
  assert.ok(!copy.SUMMARIES.includes(copy.UNANSWERED_ACTION), "the action note is not a headline of its own");
  assert.ok(!copy.SUMMARIES.includes(copy.UNANSWERED_READ));
  assert.ok(copy.UNANSWERED_ACTION.includes("didn't answer, so that change may not have gone through. Check before repeating it."));
  assert.ok(copy.UNANSWERED_READ.includes("didn't answer, so that information couldn't be loaded. Choose Try again"));
  assert.doesNotMatch(copy.UNANSWERED_READ, /gone through|repeat/, "a read changed nothing, so it says nothing about repeating");
});

test("every console request is abandoned at a deadline and reported as a timeout", async () => {
  const timers = manualTimers();
  const requests = [];
  const apiResponse = await consoleRequest((path, options) => {
    requests.push({ path, options });
    return untilAborted(options.signal);
  }, timers);

  for (const [options, milliseconds] of [
    [undefined, 20000],
    [{ method: "get" }, 20000],
    // A change outlasts the slowest legitimate answer: saving a device
    // address can take 45 seconds while the server contacts the device.
    [{ method: "POST", body: "{}" }, 60000],
    [{ method: "DELETE" }, 60000],
  ]) {
    const request = apiResponse("/api/v1/status", options);
    assert.equal(timers.pending.size, 1, "one deadline per request");
    assert.equal([...timers.pending.values()][0].milliseconds, milliseconds);
    assert.ok(requests.at(-1).options.signal instanceof AbortSignal);
    assert.equal(requests.at(-1).options.signal.aborted, false);
    assert.equal(requests.at(-1).options.method, options?.method, "the caller's method is forwarded untouched");
    timers.fire();
    await assert.rejects(request, (error) => {
      assert.equal(error.name, "TimeoutError");
      const failure = copy.describe(error);
      assert.equal(failure.summary, "This server didn't answer in time. Make sure it's turned on and awake, then try again.");
      assert.equal(failure.recovery, "retry");
      assert.equal(failure.unanswered, true);
      return true;
    });
    assert.equal(requests.at(-1).options.signal.aborted, true, "the abandoned request is cancelled, not left running");
    assert.equal(timers.pending.size, 0);
  }
});

test("an answered request cancels its deadline and a failure is passed through unchanged", async () => {
  const timers = manualTimers();
  let respond;
  const apiResponse = await consoleRequest(() => respond(), timers);

  respond = async () => ({ ok: true, status: 204, headers: new Headers() });
  assert.deepEqual((await apiResponse("/api/v1/config/export", { method: "POST" })).body, null);
  assert.equal(timers.pending.size, 0, "no timer outlives its request");

  respond = async () => { throw new TypeError("Failed to fetch"); };
  await assert.rejects(apiResponse("/api/v1/status"), TypeError);
  assert.equal(timers.pending.size, 0);

  respond = async () => ({ ok: false, status: 503, headers: new Headers(), json: async () => ({ code: "node_busy" }) });
  await assert.rejects(apiResponse("/api/v1/status"), (error) => error instanceof ApiError && error.code === "node_busy");
  assert.equal(timers.pending.size, 0);
});

test("the deadline covers a body that stalls, without rewriting a status the server did send", async () => {
  const timers = manualTimers();
  let respond;
  const apiResponse = await consoleRequest((path, options) => respond(options.signal), timers);

  // Headers arrive, then the body never does. The reader's own complaint about
  // its cancelled stream must not be what the person is told.
  respond = async (signal) => ({
    ok: true,
    status: 200,
    headers: new Headers(),
    json: () => untilAborted(signal, new TypeError("stream was cancelled")),
  });
  const stalled = apiResponse("/api/v1/status");
  await settle();
  assert.equal(timers.pending.size, 1, "the deadline is still running while the body is read");
  timers.fire();
  await assert.rejects(stalled, (error) => error.name === "TimeoutError");

  // The folder reader turns every read failure into its own "cannot safely
  // read" guidance. A timeout must still be reported as a timeout.
  respond = async (signal) => ({
    ok: true,
    status: 200,
    headers: new Headers(),
    body: { getReader: () => ({ read: () => untilAborted(signal), cancel: async () => {}, releaseLock() {} }) },
  });
  const boundedStall = apiResponse("/api/v1/sync/status", {}, folders.readJson);
  await settle();
  timers.fire();
  await assert.rejects(boundedStall, (error) => error.name === "TimeoutError" && error.covalentGuidance === undefined);

  // A refusal whose explanation stalls is still that refusal.
  respond = async (signal) => ({ ok: false, status: 503, headers: new Headers(), json: () => untilAborted(signal) });
  const refused = apiResponse("/api/v1/status");
  await settle();
  timers.fire();
  await assert.rejects(refused, (error) => error instanceof ApiError && error.status === 503);
});

test("the connection notice follows the server: raised when unanswered, cleared by any answer", async () => {
  const app = await source("app.js");
  const notice = between(app, "\nfunction renderConnection(", "\nasync function apiResponse(");
  const request = between(app, "\nasync function api(", "\nasync function loadStatus(");
  const connection = { hidden: true };
  const writes = [];
  const connectionText = {
    value: "",
    get textContent() { return this.value; },
    set textContent(text) { writes.push(text); this.value = text; },
  };
  const serverState = { textContent: "Atlas · Connected", dataset: {} };
  let respond;
  const { api } = new Function("connection", "connectionText", "apiResponse", "errorCopy", "$",
    `${notice}\n${request}\nreturn { api };`)(connection, connectionText, async () => respond(), copy, (selector) => {
    assert.equal(selector, "[data-node-state]");
    return serverState;
  });

  respond = () => { throw new TypeError("Failed to fetch"); };
  await assert.rejects(api("/api/v1/status"), TypeError, "the caller still receives the failure");
  assert.equal(connection.hidden, false);
  assert.ok(connectionText.textContent.includes("couldn't reach this server"));
  assert.equal(serverState.textContent, "Not connected", "the header must not say Connected above the notice");

  // Polling repeats the same failure every few seconds.
  await assert.rejects(api("/api/v1/sync/status"), TypeError);
  assert.equal(writes.length, 1, "identical text is not rewritten, so it is not announced again");

  respond = () => { throw named("TimeoutError"); };
  await assert.rejects(api("/api/v1/status"));
  assert.equal(connectionText.textContent, "This server didn't answer in time. Make sure it's turned on and awake, then try again.");

  // A refusal is an answer: the server is there.
  respond = () => { throw refusal(401, "authentication_required"); };
  await assert.rejects(api("/api/v1/config/export", { method: "POST" }));
  assert.equal(connection.hidden, true);
  assert.equal(serverState.textContent, "Atlas · Connected", "the header returns to what the server last said");

  respond = () => { throw new TypeError("Failed to fetch"); };
  await assert.rejects(api("/api/v1/status"));
  assert.equal(connection.hidden, false);
  assert.equal(serverState.textContent, "Not connected");
  respond = () => ({ body: { deviceName: "Waypoint" } });
  assert.deepEqual(await api("/api/v1/status"), { deviceName: "Waypoint" });
  assert.equal(connection.hidden, true);
  assert.equal(serverState.textContent, "Atlas · Connected");

  // Answers that arrive while nothing is wrong leave the header alone.
  serverState.textContent = "Waypoint · Connected";
  await api("/api/v1/status");
  assert.equal(serverState.textContent, "Waypoint · Connected");

  // Every request that got no answer says whether it was a read or a change.
  respond = () => { throw new TypeError("Failed to fetch"); };
  await assert.rejects(api("/api/v1/status"), (error) => error.covalentRequest === "read");
  await assert.rejects(api("/api/v1/sync/retry", { method: "POST" }), (error) => error.covalentRequest === "change");
  respond = () => ({ body: {} });
  await api("/api/v1/status");
});

test("a slow change on a server that keeps answering does not report the server as lost", async () => {
  const app = await source("app.js");
  const notice = between(app, "\nfunction renderConnection(", "\nasync function apiResponse(");
  const request = between(app, "\nasync function api(", "\nasync function loadStatus(");
  const connection = { hidden: true };
  const connectionText = { textContent: "" };
  const serverState = { textContent: "Atlas · Connected", dataset: {} };
  const pending = new Map();
  const { api } = new Function("connection", "connectionText", "apiResponse", "errorCopy", "$",
    `${notice}\n${request}\nreturn { api };`)(connection, connectionText, (path) => new Promise((resolve, reject) => {
    pending.set(path, { resolve, reject });
  }), copy, () => serverState);
  const timedOut = () => named("TimeoutError");

  // A device-address update is still running while polling keeps getting answers.
  const change = api("/api/v1/sync/peers/refresh-address", { method: "POST", body: "{}" });
  const poll = api("/api/v1/sync/status");
  pending.get("/api/v1/sync/status").resolve({ body: { ok: true } });
  await poll;
  pending.get("/api/v1/sync/peers/refresh-address").reject(timedOut());
  await assert.rejects(change, (error) => error.covalentRequest === "change");
  assert.equal(connection.hidden, true, "the server answered meanwhile, so it is not lost");
  assert.equal(serverState.textContent, "Atlas · Connected", "the header keeps saying Connected");

  // The same change on a server that answered nothing in the meantime is the
  // server going quiet, and the notice says so.
  const quiet = api("/api/v1/sync/peers/refresh-address", { method: "POST", body: "{}" });
  pending.get("/api/v1/sync/peers/refresh-address").reject(timedOut());
  await assert.rejects(quiet);
  assert.equal(connection.hidden, false);
  assert.equal(serverState.textContent, "Not connected");

  // A read that gets no answer always raises the notice.
  const answer = api("/api/v1/status");
  pending.get("/api/v1/status").resolve({ body: {} });
  await answer;
  assert.equal(connection.hidden, true);
  const read = api("/api/v1/discovery");
  const other = api("/api/v1/sync/status");
  pending.get("/api/v1/sync/status").resolve({ body: {} });
  await other;
  pending.get("/api/v1/discovery").reject(new TypeError("Failed to fetch"));
  await assert.rejects(read);
  assert.equal(connection.hidden, false);

  // The server's real refusal of a slow change still arrives within the deadline.
  assert.match(app, /const REQUEST_TIMEOUT_MS = Object\.freeze\(\{ read: 20000, change: 60000 \}\);/);
});

test("an unanswered change or read leaves a lasting note in its own words; connecting adds none", async () => {
  const app = await source("app.js");
  const renderers = between(app, "\nfunction fail(error) {", "\n// One place owns");
  const said = [];
  const message = { dataset: {} };
  const { fail, failToConnect } = new Function("errorCopy", "console", "say", "message", "renderConnection",
    `${renderers}\nreturn { fail, failToConnect };`)(
    copy, { debug() {} }, (...values) => said.push(values), message,
    () => assert.fail("api() alone updates the connection notice; fail() must not override it"),
  );

  fail(Object.assign(new TypeError("Failed to fetch"), { covalentRequest: "change" }));
  assert.equal(said.length, 1);
  assert.equal(said[0][0], copy.UNANSWERED_ACTION, "a change may not have gone through");
  assert.equal(said[0][1], true);
  assert.equal(message.dataset.recovery, "reconnect");

  fail(Object.assign(named("TimeoutError"), { covalentRequest: "read" }));
  assert.equal(said[1][0], copy.UNANSWERED_READ, "a read could not be loaded");

  fail(new TypeError("Failed to fetch"));
  assert.equal(said[2][0], copy.UNANSWERED_ACTION, "an unmarked request is treated as a change");

  failToConnect(named("TimeoutError"));
  assert.equal(said.length, 3, "connecting adds no second message beside the notice");

  // Anything the server did answer is reported the ordinary way, by both.
  said.splice(0);
  failToConnect(refusal(403, "not_authorized"));
  fail(refusal(503, "node_busy"));
  assert.deepEqual(said.map((values) => values[0]), [
    "This server refused this request. Unlock the console again with a current access token.",
    "This server is busy with something else. Try again in a moment.",
  ]);
});

test("the page offers a way to try again wherever the server can fail to answer", async () => {
  const [html, app] = await Promise.all([source("index.html"), source("app.js")]);
  assert.match(html, /<section class="connection-banner" data-connection hidden>\s*<div role="alert"><strong>Can’t reach this server<\/strong><p data-connection-text><\/p><\/div>/);
  assert.match(html, /<button type="button" class="secondary" data-connection-retry[^>]*>Try again<\/button>/);
  assert.match(html, /<p data-folders-status>Loading your links…<\/p>\s*<button type="button" class="secondary" data-folders-refresh hidden>Refresh links<\/button>/);
  assert.match(html, /<p class="muted" data-folders-empty hidden>/, "the empty message does not show beside the loading message");

  // Refresh and "Try again" are one routine, and it asks for access again when
  // the page never got as far as being unlocked.
  assert.match(app, /\$\("\[data-refresh\]"\)\.addEventListener\("click", \(event\) => \{ void reloadConsole\(event\.currentTarget\); \}\);/);
  assert.match(app, /\$\("\[data-connection-retry\]"\)\.addEventListener\("click", \(event\) => \{ void reloadConsole\(event\.currentTarget\); \}\);/);
  const reload = between(app, "\nasync function reloadConsole(", "\n}\n");
  assert.match(reload, /await loadStatus\(\);[\s\S]*if \(!accessReady && token === ""\) await tryTrustedAccess\(\);\s*if \(!accessReady\) return;/);
  assert.match(reload, /if \(folderDeviceId === null\) await retryFolderSync\(\);/);

  // A list that failed to load says so and shows its own retry; a list that
  // loaded hides it again; a locked console shows no list at all.
  assert.match(between(app, "\nfunction renderFolderError(", "\n}\n"), /\$\("\[data-folders-refresh\]"\)\.hidden = false;/);
  assert.match(between(app, "\nfunction renderFolderStatus(", "\n}\n"), /\$\("\[data-folders-refresh\]"\)\.hidden = true;/);
  const locked = between(app, "\nfunction clearFolderSyncAccess(", "\n}\n");
  assert.match(locked, /\$\("\[data-folders-list\]"\)\.replaceChildren\(\);/);
  assert.match(locked, /"Unlock the console to see your links\."/);
});

test("background refreshes never overlap themselves and slow actions cannot be sent twice", async () => {
  const app = await source("app.js");
  const poll = between(app, "\nlet networkPairingsRefreshing = false;", "\nfunction renderNetworkPairing(");
  const calls = [];
  const settleLater = [];
  const pending = () => new Promise((resolve) => { settleLater.push(resolve); });
  const { pollFolders, pollNetworkPairings } = new Function("loadFolders", "refreshNetworkPairings",
    `${poll}\nreturn { pollFolders, pollNetworkPairings };`)(
    () => { calls.push("links"); return pending(); },
    () => { calls.push("pairings"); return pending(); },
  );
  pollFolders();
  pollFolders();
  pollNetworkPairings();
  pollNetworkPairings();
  assert.deepEqual(calls, ["links", "pairings"], "a refresh still waiting for its answer is not started again");
  settleLater.splice(0).forEach((resolve) => resolve());
  await settle();
  pollFolders();
  pollNetworkPairings();
  assert.deepEqual(calls, ["links", "pairings", "links", "pairings"]);

  assert.match(app, /setInterval\(\(\) => \{\s*if \(folderPollingEligible\(\)\) \{\s*pollFolders\(\);\s*if \(networkPoll === null\) pollNetworkPairings\(\);/);
  assert.match(app, /networkPoll = setInterval\(pollNetworkPairings, 3000\);/);
  const unlock = between(app, '$("[data-token-form]").addEventListener("submit"', "\n});");
  assert.match(unlock, /if \(submit\.disabled\) return;[\s\S]*submit\.disabled = true;[\s\S]*finally \{\s*submit\.disabled = false;/);
  const settingsImport = between(app, '$("[data-settings-import]").addEventListener("submit"', "\n});");
  assert.match(settingsImport, /if \(submit\.disabled\) return;[\s\S]*submit\.disabled = true;[\s\S]*finally \{ submit\.disabled = false; \}/);
});

test("deletion confirmations say what is deleted, where, and what is never touched", async () => {
  const deleting = folders.deletionConfirmation({ propagateSourceDeletions: true, restoreLocalDeletions: false }, " Music ", "change");
  assert.match(deleting, /^Change the deletion options for “Music”\?\n\n/);
  assert.match(deleting, /When a file is deleted from the source folder, Covalent also deletes its copy from every recipient’s destination folder\. Those copies are removed from disk\./);
  assert.match(deleting, /When a file is deleted from a destination folder, it stays deleted there\./);
  assert.match(deleting, /Files in the source folder are never deleted by these options\. They apply to every recipient of this link\.$/);

  const restoring = folders.deletionConfirmation({ propagateSourceDeletions: false, restoreLocalDeletions: true }, "Photos", "create");
  assert.match(restoring, /^Create “Photos” with these deletion options\?/);
  assert.match(restoring, /its destination copies are kept\./);
  assert.match(restoring, /Covalent copies it there again from the source\./);
  assert.doesNotMatch(restoring, /removed from disk/);

  assert.match(folders.deletionConfirmation({ propagateSourceDeletions: true, restoreLocalDeletions: true }, "", "change"),
    /^Change the deletion options for this link\?/);
  assert.throws(() => folders.deletionConfirmation({ propagateSourceDeletions: true }, "Music", "change"), /folder sync guidance/);

  const app = await source("app.js");
  assert.equal(app.split("globalThis.confirm(").length - 1, 5, "the five native confirmations are the only ones");
  assert.match(app, /globalThis\.confirm\(folderSync\.deletionConfirmation\(policy, share\.label, "change"\)\)/);
  assert.match(app, /globalThis\.confirm\(folderSync\.deletionConfirmation\(body\.linkPolicy, body\.label, "create"\)\)/);
  // Discarding a saved request removes this browser's copy and nothing else.
  for (const [question, kept] of [
    ["Discard the saved invitation?", "it stays there and appears under Your links"],
    ["Discard the saved settings change?", "it stays in effect"],
    ["Discard the saved Run Now request?", "that run still goes ahead"],
  ]) {
    const start = app.indexOf(question);
    assert.ok(start > 0, question);
    const body = app.slice(start, app.indexOf("return;", start));
    assert.match(body, /Removed: only the copy of this (?:request|change) that this browser kept so it could be sent again\./);
    assert.ok(body.includes(kept), `${question} must say what is not undone`);
    assert.match(body, /No link, folder or file is deleted\./);
  }
});

test("removing a recipient, stopping, and declining each say which files stay where", async () => {
  const app = await source("app.js");
  const body = /function renderFolderActions\(container, status, share, view\) \{([\s\S]*?)\n\}\n/.exec(app)?.[1];
  assert.ok(body, "the recipient action renderer must exist");
  const element = () => ({ children: [], hidden: false, append(...items) { this.children.push(...items); }, setAttribute(name, value) { this[name] = value; }, focus() {} });
  const done = [];
  const render = new Function("document", "folderActionButton", "runFolderMutation", "folderController", "folderPeerName",
    `return (container, status, share, view) => {${body}};`)(
    { createElement: element },
    (label, action) => ({ textContent: label, action, focus() {} }),
    (action, success) => { action(); done.push(success); },
    { remove: (id) => done.push(id), accept() {}, renew() {} },
    () => "Waypoint",
  );
  const base = { offerId: "offer", peerId: "waypoint", label: "Music", phase: "ready", expired: false, incoming: false };
  const cases = [
    [base, "Remove recipient…", /^Remove Waypoint from Music\?.*Files already copied stay on that device, and nothing is deleted from the source folder\.$/, "Keep recipient", "Remove recipient"],
    [{ ...base, incoming: true }, "Stop receiving…", /^Stop receiving Music on this server\?.*Files already in the destination folder stay there, and nothing is deleted on the source\.$/, "Keep receiving", "Stop receiving"],
    [{ ...base, incoming: true, phase: "offered" }, "Decline", /^Decline the invitation to Music\? Nothing has been copied to this server yet, and nothing is deleted on any device\./, "Keep invitation", "Decline invitation"],
  ];
  for (const [share, open, question, keep, confirm] of cases) {
    const container = element();
    render(container, {}, share, { kind: "ready" });
    const confirmation = container.children.at(-1);
    const opener = container.children.at(-2);
    assert.equal(opener.textContent, open);
    assert.equal(confirmation.hidden, true, "nothing is removed by the first press");
    opener.action();
    assert.equal(confirmation.hidden, false);
    const [explanation, cancel, confirmed] = confirmation.children;
    assert.match(explanation.textContent, question);
    assert.equal(cancel.textContent, keep);
    assert.equal(confirmed.textContent, confirm);
    assert.equal(confirmation["aria-label"], `${confirm}: Music`);
    confirmed.action();
    assert.equal(done.at(-2), "offer");
    assert.match(done.at(-1), /Nothing was copied or deleted\.|Files already copied stay on both devices\./);
  }
});

test("the console speaks one vocabulary: link, device, this server, access token, pairing request", async () => {
  const html = await source("index.html");
  const visible = html.replace(/<!--[\s\S]*?-->/g, " ").replace(/<[^>]+>/g, " ");
  assert.doesNotMatch(visible, /\bshares?\b/i, "the synced object is a link");
  assert.doesNotMatch(visible, /\boffers?\b|backup server|peer address|API token|local access token/i);
  assert.doesNotMatch(visible.replaceAll("covalent-node", ""), /\bnodes?\b|\bpeers?\b/i, "a machine running Covalent is a device; this one is this server");
  for (const attribute of html.matchAll(/(?:aria-label|title|placeholder)="([^"]*)"/g)) {
    assert.doesNotMatch(attribute[1], /\bshares?\b|\bpeer\b|\bnode\b|API token/i, attribute[0]);
  }
  assert.match(visible, /Your links/);
  assert.match(visible, /Create a one-way link/);
  assert.match(visible, /Send pairing request/);
  assert.match(visible, /This server’s device address/);

  for (const name of ["app.js", "folder-sync-flow.js", "pairing-flow.js"]) {
    const script = (await source(name)).split("\n").filter((line) => !/^\s*\/\//.test(line)).join("\n");
    // Sentences only: a quoted string with a space in it, not an identifier.
    let sentences = 0;
    for (const [literal] of script.matchAll(/"(?:[^"\\\n]|\\.)*"|`(?:[^`\\\n]|\\.)*`/g)) {
      if (!literal.includes(" ")) continue;
      sentences += 1;
      assert.doesNotMatch(literal, /backup server|\b(?:the|this|a) node\b|\bpeer address\b|folder offer|saved offer|API token|local access token/i, `${name}: ${literal.slice(0, 90)}`);
      assert.doesNotMatch(literal, /\b(?:this|your|a|the|first|New|No) shares?\b|\bshares yet\b/i, `${name}: ${literal.slice(0, 90)}`);
    }
    assert.ok(sentences > 20, `${name}: the sentence extractor found too little to be checking anything`);
  }
  assert.equal(copy.SUMMARIES.some((summary) => /backup server/i.test(summary)), false);
});
