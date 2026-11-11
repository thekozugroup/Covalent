import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { runInNewContext } from "node:vm";

const require = createRequire(import.meta.url);
const copy = require("../app.js");
const folders = require("../folder-sync-flow.js");
const root = new URL("../", import.meta.url);

const self = "11111111-1111-4111-8111-111111111111";
const waypoint = "22222222-2222-4222-8222-222222222222";
const folderId = "44444444-4444-4444-8444-444444444444";
const release = (version) => `https://github.com/thekozugroup/Covalent/releases/tag/v${version}`;

function source(name) {
  return readFile(new URL(name, root), "utf8");
}

function definition(app, name) {
  const found = new RegExp(`(?:async )?function ${name}\\([^]*?\\n\\}`).exec(app)?.[0];
  assert.ok(found, `${name} must exist`);
  return found;
}

function status(update, version = "0.2.12") {
  return { protocolVersion: 1, deviceName: "Atlas", state: "ready", lanDiscovery: false, version, update };
}

function update(state, extra = {}) {
  return { state, latestVersion: null, releaseUrl: null, checkedAtUnixMs: null, failure: null, ...extra };
}

test("the version line reports the server's own release check, and never as an error", () => {
  const cases = [
    [status(update("upToDate", { latestVersion: "0.2.12", checkedAtUnixMs: 1 })), "Up to date", null],
    [status(update("disabled")), "Update checks are off", null],
    [status(update("unsupported")), null, null],
    [status(update("pending")), null, null],
    [status(update("updateAvailable", { latestVersion: "0.2.13", releaseUrl: release("0.2.13"), checkedAtUnixMs: 1 })),
      "Version 0.2.13 is available", { version: "0.2.13", releaseUrl: release("0.2.13") }],
  ];
  for (const [body, note, available] of cases) {
    const about = copy.serverVersion(body);
    assert.equal(about.version, "0.2.12");
    assert.equal(about.note, note, body.update.state);
    assert.deepEqual(about.available === null ? null : { ...about.available }, available, body.update.state);
  }
  assert.equal(copy.serverVersion(status(update("pending"))).pending, true);

  // A check that could not run is a quiet sentence keyed on its code; a code
  // this page does not know reads like an unexpected answer.
  for (const [code, words] of [
    ["network", /couldn't reach GitHub\. Check its internet connection\./],
    ["timeout", /took too long/],
    ["secureConnection", /secure connection/],
    ["rateLimited", /limiting requests/],
    ["unexpectedResponse", /wasn't one Covalent understands/],
    ["somethingNew", /wasn't one Covalent understands/],
  ]) {
    const note = copy.serverVersion(status(update("checkFailed", { checkedAtUnixMs: 1, failure: { code, message: "server text" } }))).note;
    assert.match(note, /^Couldn't check for updates/);
    assert.match(note, words, code);
    assert.ok(!note.includes("server text"), "the server's sentence is not shown");
    assert.doesNotMatch(note, /error|failed|warning/i, "a check that could not run is not presented as a fault");
  }
});

test("older servers and unexpected values degrade to showing less, never to an error", () => {
  // A server from before the check sends neither field.
  const old = copy.serverVersion({ protocolVersion: 1, deviceName: "Atlas", state: "ready", lanDiscovery: false });
  assert.deepEqual({ ...old }, { version: null, note: null, available: null, pending: false });
  for (const body of [null, undefined, "x", 7, []]) assert.equal(copy.serverVersion(body).version, null);

  assert.equal(copy.serverVersion(status(update("upToDate"), "<b>1</b>")).version, null, "a version must look like one");
  assert.equal(copy.serverVersion(status(update("upToDate"), "v0.2.12")).version, null);
  assert.equal(copy.serverVersion(status(update("upToDate"), "0.2.13-dev+abc")).version, "0.2.13-dev+abc");

  // An announcement needs a real version; its link must be exactly that
  // version's release page, or the notice is shown without a link.
  assert.equal(copy.serverVersion(status(update("updateAvailable", { latestVersion: "latest" }))).available, null);
  for (const releaseUrl of [
    "https://example.com/thekozugroup/Covalent/releases/tag/v0.2.13",
    "javascript:alert(1)",
    release("0.2.14"),
    `${release("0.2.13")}?x=1`,
    null,
  ]) {
    const about = copy.serverVersion(status(update("updateAvailable", { latestVersion: "0.2.13", releaseUrl })));
    assert.equal(about.available.version, "0.2.13");
    assert.equal(about.available.releaseUrl, null, String(releaseUrl));
  }
});

class Element {
  constructor() {
    this.hidden = false;
    this.dataset = {};
    this.children = [];
    this.textContent = "";
  }

  append(...items) { for (const item of items) this.children.push(item); }
  replaceChildren() { this.children = []; }
  text() { return this.children.map((item) => (typeof item === "string" ? item : item.textContent)).join(""); }
}

async function versionRenderer({ stored = null, storageThrows = false } = {}) {
  const app = await source("app.js");
  const nodes = new Map();
  const timers = [];
  const writes = [];
  const storage = {
    getItem(key) { if (storageThrows) throw new Error("blocked"); return key === "covalent.update-notice.dismissed" ? stored : null; },
    setItem(key, value) { if (storageThrows) throw new Error("blocked"); writes.push([key, value]); stored = value; },
  };
  const context = {
    $: (selector) => { if (!nodes.has(selector)) nodes.set(selector, new Element()); return nodes.get(selector); },
    document: { createElement: () => new Element() },
    errorCopy: copy,
    globalThis: { localStorage: storage },
    setTimeout: (callback, milliseconds) => { timers.push(milliseconds); return timers.length; },
    clearTimeout: () => {},
    apiResponse: async () => ({ body: status(update("upToDate", { latestVersion: "0.2.12" })) }),
  };
  const declarations = /const UPDATE_DISMISSED_KEY[^]*?let updateRecheck = null;/.exec(app)?.[0];
  assert.ok(declarations);
  runInNewContext(`${declarations.replaceAll("const ", "var ").replaceAll("let ", "var ")}
${definition(app, "dismissedUpdate")}
${definition(app, "renderServerVersion")}
${definition(app, "recheckServerVersion")}`, context);
  return { context, node: context.$, timers, writes };
}

test("the footer shows the running version, with the release notes when there is news", async () => {
  const { context, node } = await versionRenderer();
  context.renderServerVersion(status(update("upToDate", { latestVersion: "0.2.12" })));
  assert.equal(node("[data-server-version]").hidden, false);
  assert.equal(node("[data-server-version]").text(), "Covalent 0.2.12 · Up to date");
  assert.equal(node("[data-update-notice]").hidden, true);

  context.renderServerVersion(status(update("updateAvailable", { latestVersion: "0.2.13", releaseUrl: release("0.2.13") })));
  const line = node("[data-server-version]");
  assert.equal(line.text(), "Covalent 0.2.12 · Version 0.2.13 is available · Release notes");
  const link = line.children.at(-1);
  assert.equal(link.href, release("0.2.13"));
  assert.equal(link.rel, "noopener noreferrer");

  // An older server: no version line at all.
  context.renderServerVersion({ protocolVersion: 1, deviceName: "Atlas", state: "ready", lanDiscovery: false });
  assert.equal(node("[data-server-version]").hidden, true);
  assert.equal(node("[data-update-notice]").hidden, true);
});

test("version and update fields absent before unlock stay quiet, and appear once the unlocked read has them", async () => {
  const { context, node } = await versionRenderer();
  const locked = { protocolVersion: 1, deviceName: "Atlas", state: "ready", lanDiscovery: false };
  context.apiResponse = async () => ({ body: locked });
  await context.recheckServerVersion();
  assert.equal(node("[data-server-version]").hidden, true);
  assert.equal(node("[data-update-notice]").hidden, true);

  context.apiResponse = async () => ({ body: status(update("updateAvailable", { latestVersion: "0.2.13", releaseUrl: release("0.2.13") })) });
  await context.recheckServerVersion();
  assert.equal(node("[data-server-version]").text(), "Covalent 0.2.12 · Version 0.2.13 is available · Release notes");
  assert.equal(node("[data-update-notice]").hidden, false);

  const app = await source("app.js");
  const authorize = /authorize: async \(\) => \{([^]*?)\n    \},/.exec(app)?.[1];
  assert.ok(authorize);
  assert.match(authorize, /accessReady = true;\s*[^]*void recheckServerVersion\(\);/, "unlocking reads the version again");
});

test("an available update raises a calm notice that says how this install is updated, until dismissed", async () => {
  const [{ context, node }, html, app] = await Promise.all([versionRenderer(), source("index.html"), source("app.js")]);
  context.renderServerVersion(status(update("updateAvailable", { latestVersion: "0.2.13", releaseUrl: release("0.2.13") })));
  const notice = node("[data-update-notice]");
  assert.equal(notice.hidden, false);
  assert.equal(notice.dataset.version, "0.2.13");
  assert.equal(node("[data-update-heading]").textContent, "Covalent 0.2.13 is available");
  assert.equal(node("[data-update-running]").textContent, "This server runs Covalent 0.2.12.");
  assert.equal(node("[data-update-notes]").hidden, false);
  assert.equal(node("[data-update-link]").href, release("0.2.13"));
  assert.equal(node("[data-update-link]").textContent, "Read the release notes for 0.2.13");

  // No link when the announcement carries none the page can trust.
  context.renderServerVersion(status(update("updateAvailable", { latestVersion: "0.2.13", releaseUrl: "https://example.com/" })));
  assert.equal(node("[data-update-notes]").hidden, true);

  // The static half: an informational tone, how the container is updated, a
  // way to dismiss, and no wording that sounds like a fault.
  const markup = /<section class="update-notice"[^]*?<\/section>/.exec(html)?.[0];
  assert.ok(markup);
  assert.match(markup, /data-update-notice hidden aria-labelledby="update-heading"/);
  assert.match(markup, /A Covalent server doesn’t update itself\./);
  assert.match(markup, /on Unraid, open the Docker tab and choose Check for Updates/);
  assert.match(markup, /<code>docker compose pull &amp;&amp; docker compose up -d<\/code>/);
  assert.match(markup, /Covalent-Updater/);
  assert.match(markup, /<a data-update-link href="#" target="_blank" rel="noopener noreferrer">/);
  assert.match(markup, /<button type="button" class="quiet" data-update-dismiss>Dismiss<\/button>/);
  assert.doesNotMatch(markup, /role="alert"|warning|error|urgent/i);

  // Dismissing remembers the announced version, so a later release shows again.
  const dismiss = /\$\("\[data-update-dismiss\]"\)\.addEventListener\("click", \(\) => \{([^]*?)\n\}\);/.exec(app)?.[1];
  assert.ok(dismiss);
  assert.match(dismiss, /updateDismissedHere = notice\.dataset\.version/);
  assert.match(dismiss, /localStorage\.setItem\(UPDATE_DISMISSED_KEY/);
  assert.match(dismiss, /notice\.hidden = true;/);
});

test("a dismissed version stays hidden, a newer one does not, and blocked storage still works for the page", async () => {
  const dismissed = await versionRenderer({ stored: "0.2.13" });
  dismissed.context.renderServerVersion(status(update("updateAvailable", { latestVersion: "0.2.13", releaseUrl: release("0.2.13") })));
  assert.equal(dismissed.node("[data-update-notice]").hidden, true);
  assert.equal(dismissed.node("[data-server-version]").text(), "Covalent 0.2.12 · Version 0.2.13 is available · Release notes",
    "the footer still says so after the notice is dismissed");
  dismissed.context.renderServerVersion(status(update("updateAvailable", { latestVersion: "0.2.14", releaseUrl: release("0.2.14") })));
  assert.equal(dismissed.node("[data-update-notice]").hidden, false);

  const blocked = await versionRenderer({ storageThrows: true });
  blocked.context.updateDismissedHere = "0.2.13";
  blocked.context.renderServerVersion(status(update("updateAvailable", { latestVersion: "0.2.13", releaseUrl: release("0.2.13") })));
  assert.equal(blocked.node("[data-update-notice]").hidden, true);
});

test("while the server's first check is pending the page looks again, quietly and a bounded number of times", async () => {
  const { context, node, timers } = await versionRenderer();
  context.renderServerVersion(status(update("pending")));
  assert.deepEqual(timers, [90000]);
  context.renderServerVersion(status(update("pending")));
  assert.deepEqual(timers, [90000], "one recheck at a time");
  for (let round = 0; round < 6; round += 1) await context.recheckServerVersion();
  assert.equal(node("[data-server-version]").text(), "Covalent 0.2.12 · Up to date");

  context.apiResponse = async () => ({ body: status(update("pending")) });
  for (let round = 0; round < 10; round += 1) await context.recheckServerVersion();
  assert.ok(timers.length <= 5, `at most five rechecks, saw ${timers.length}`);

  // A recheck that gets no answer leaves the line as it was and says nothing.
  context.apiResponse = async () => { throw new TypeError("Failed to fetch"); };
  const before = node("[data-server-version]").text();
  await context.recheckServerVersion();
  assert.equal(node("[data-server-version]").text(), before);
  const app = await source("app.js");
  assert.match(definition(app, "recheckServerVersion"), /apiResponse\("\/api\/v1\/status"\)/,
    "the quiet recheck bypasses the connection notice that api() maintains");
});

test("failure causes are read leniently and keyed on their code", () => {
  const entry = (overrides = {}) => ({ folderId, generation: 3, role: "destination", code: "noSpace", message: "run the share again", endedAtUnixMs: 1, ...overrides });
  const read = folders.runFailures({ schemaVersion: 1, failures: [
    entry(),
    entry({ folderId: "not-a-uuid" }),
    entry({ folderId: "55555555-5555-4555-8555-555555555555", generation: 0 }),
    entry({ folderId: "66666666-6666-4666-8666-666666666666", role: "observer" }),
    entry({ folderId: "77777777-7777-4777-8777-777777777777", code: 7 }),
    entry({ folderId: "88888888-8888-4888-8888-888888888888", role: "source", code: "aCodeFromLater" }),
    null,
    "x",
  ] });
  assert.deepEqual([...read.keys()], [folderId, "88888888-8888-4888-8888-888888888888"]);
  assert.deepEqual({ ...read.get(folderId) }, { folderId, generation: 3, role: "destination", code: "noSpace" },
    "the server's own sentence is not kept");
  for (const value of [null, {}, { schemaVersion: 2, failures: [entry()] }, { schemaVersion: 1, failures: {} }, []]) {
    assert.equal(folders.runFailures(value).size, 0);
  }

  assert.equal(folders.runFailureCopy("noSpace", "manual"),
    "This server ran out of storage space for the copy. Free up space on it. Then choose Run Now.");
  assert.equal(folders.runFailureCopy("noSpace", "scheduled"),
    "This server ran out of storage space for the copy. Free up space on it. Then choose Run Now, or wait for the next scheduled run.");
  assert.equal(folders.runFailureCopy("noSpace", "continuous"), "This server ran out of storage space for the copy. Free up space on it.");
  assert.equal(folders.runFailureCopy("sourceChanged", "manual"),
    "Files at the source changed while they were being copied. Nothing needs fixing: the next run copies the new versions.");
  assert.equal(folders.runFailureCopy("aCodeFromLater", "manual"), folders.runFailureCopy("unknown", "manual"));
  assert.equal(folders.runFailureCopy("__proto__", "manual"), folders.runFailureCopy("unknown", "manual"));
  for (const code of ["noSpace", "permissionDenied", "readOnly", "integrityMismatch", "notFound", "network", "timeout",
    "sourceChanged", "folderUnavailable", "recoveryRequired", "workerUnavailable", "unknown"]) {
    const words = folders.runFailureCopy(code, "manual");
    assert.match(words, /\.$/);
    assert.doesNotMatch(words, /\bshares?\b|\bnode\b|\bpeer\b/i, `${code} uses the console's own words`);
  }
});

async function linkRunRenderer(runFailures, deviceId = self) {
  const app = await source("app.js");
  const element = () => ({ children: [], dataset: {}, textContent: "", append(...items) { this.children.push(...items); }, setAttribute() {} });
  const context = {
    document: { createElement: element },
    folderController: { pendingRun: () => null },
    folderActionButton: (label) => ({ label, disabled: false, dataset: {} }),
    renderFolderError: assert.fail,
    folderSync: folders,
    folderDeviceId: deviceId,
    runFailures,
  };
  runInNewContext(["runResultCopy", "runDestinationName", "runFailureFor", "failedRecipientCopy", "interruptedRunCopy", "renderLinkRun"]
    .map((name) => definition(app, name)).join("\n"), context);
  return (share, peers = [{ peerId: waypoint, displayName: "Waypoint" }]) => {
    const container = element();
    context.renderLinkRun(container, { peers }, share);
    const section = container.children.find((item) => !item.label);
    const list = section.children.find((item) => Array.isArray(item.children) && item.children.length > 0 && item.children[0].textContent);
    return { summary: section.children[1].textContent, rows: list?.children.map((row) => row.textContent) ?? [] };
  };
}

function failedShare(overrides = {}) {
  return {
    folderId,
    linkSettings: { confirmed: true, settings: { cadence: { mode: "manual" }, paused: false } },
    linkRun: { generation: 3, phase: "incomplete", pendingRequest: null, rejectedRequest: null, nextDueAtUnixMs: null, destinations: [
      { peerId: self, result: "failed" }, { peerId: waypoint, result: "failed" },
    ] },
    ...overrides,
  };
}

test("a failed copy on this server says why; another device's failure says where to look", async () => {
  const known = new Map([[folderId, { folderId, generation: 3, role: "destination", code: "readOnly" }]]);
  const render = await linkRunRenderer(known);
  assert.deepEqual(render(failedShare()).rows, [
    "This server: Failed. The destination folder on this server is read-only. Make it writable (on Docker, check that it isn't mounted read-only) or choose another folder. Then choose Run Now.",
    "Waypoint: Failed. Open Covalent on Waypoint to see why.",
  ]);
  assert.deepEqual(render(failedShare(), []).rows.at(-1), "Another recipient: Failed. Open Covalent on that device to see why.");

  // A cause recorded for an earlier run is not this run's cause.
  const stale = await linkRunRenderer(new Map([[folderId, { folderId, generation: 2, role: "destination", code: "readOnly" }]]));
  assert.equal(stale(failedShare()).rows[0], "This server: Failed");
  // Nothing known (an older server, or the route did not answer): as before.
  const none = await linkRunRenderer(new Map());
  assert.equal(none(failedShare()).rows[0], "This server: Failed");
  // Results that are not failures are untouched.
  const done = await linkRunRenderer(known);
  const succeeded = failedShare({ linkRun: { ...failedShare().linkRun, destinations: [{ peerId: self, result: "succeeded" }, { peerId: waypoint, result: "timedOut" }] } });
  assert.deepEqual(done(succeeded).rows, ["This server: Completed", "Waypoint: Incomplete after 24 hours"]);
});

test("an interrupted run names the source's own problem when this server is the source", async () => {
  const interrupted = failedShare({ linkRun: { ...failedShare().linkRun, phase: "interrupted", destinations: [] } });
  const render = await linkRunRenderer(new Map([[folderId, { folderId, generation: 3, role: "source", code: "folderUnavailable" }]]));
  assert.equal(render(interrupted).summary,
    "The last run was interrupted before it finished. This server couldn't open the link's folder. Reconnect its drive or mount, or choose the folder again. Then choose Run Now.");
  const plain = await linkRunRenderer(new Map([[folderId, { folderId, generation: 3, role: "destination", code: "noSpace" }]]));
  assert.equal(plain(interrupted).summary, "The last run was interrupted before it finished. Choose Run Now when ready.");
});

async function failuresRefresher(current, respond) {
  const app = await source("app.js");
  class NodeApiError extends Error {
    constructor(status) { super(`http ${status}`); this.status = status; }
  }
  const calls = { requests: [], renders: 0, loud: 0 };
  const context = {
    NodeApiError,
    folderSync: folders,
    folderDeviceId: self,
    runFailures: new Map(),
    runFailuresAvailable: true,
    runFailuresRefreshing: false,
    folderController: { current: () => current },
    apiResponse: async (...args) => { calls.requests.push(args); return respond(NodeApiError); },
    renderFolderStatus: () => { calls.renders += 1; },
    // The quiet read must never reach the loud paths.
    fail: () => { calls.loud += 1; },
    say: () => { calls.loud += 1; },
    renderConnection: () => { calls.loud += 1; },
  };
  runInNewContext(`${definition(app, "runFailuresWanted")}\n${definition(app, "refreshRunFailures")}`, context);
  return { context, calls };
}

test("failure causes are asked for only when needed, quietly, and an older server is left alone", async () => {
  const failing = { shares: [failedShare()] };
  const body = { schemaVersion: 1, failures: [{ folderId, generation: 3, role: "destination", code: "noSpace", message: "", endedAtUnixMs: 1 }] };

  const ok = await failuresRefresher(failing, () => ({ body }));
  await ok.context.refreshRunFailures();
  assert.equal(ok.calls.requests[0][0], "/api/v1/sync/run-failures");
  assert.deepEqual(Object.keys(ok.calls.requests[0][1]), [], "a plain GET");
  assert.equal(ok.calls.requests[0][2], folders.readJson, "the bounded reader is used");
  assert.equal(ok.context.runFailures.get(folderId).code, "noSpace");
  assert.equal(ok.calls.renders, 1);
  await ok.context.refreshRunFailures();
  assert.equal(ok.calls.renders, 1, "an unchanged answer does not redraw the page");

  const healthy = await failuresRefresher({ shares: [failedShare({ linkRun: { ...failedShare().linkRun, phase: "succeeded", destinations: [{ peerId: self, result: "succeeded" }] } })] }, assert.fail);
  await healthy.context.refreshRunFailures();
  assert.equal(healthy.calls.requests.length, 0, "nothing failed, nothing to ask");

  const old = await failuresRefresher(failing, (NodeApiError) => { throw new NodeApiError(404); });
  await old.context.refreshRunFailures();
  await old.context.refreshRunFailures();
  assert.equal(old.calls.requests.length, 1, "a server without the route is not asked again");
  assert.equal(old.context.runFailuresAvailable, false);

  for (const failure of [() => { throw new TypeError("Failed to fetch"); }, () => { const error = new Error("late"); error.name = "TimeoutError"; throw error; },
    (NodeApiError) => { throw new NodeApiError(401); }, () => ({ body: "not json" })]) {
    const quiet = await failuresRefresher(failing, failure);
    await quiet.context.refreshRunFailures();
    assert.equal(quiet.calls.loud, 0, "no banner and no message for this read");
    assert.equal(quiet.context.runFailuresAvailable, true, "only a 404 stops the asking");
    assert.equal(quiet.context.runFailures.size, 0);
  }

  // One read at a time: polling while an answer is still outstanding asks nothing.
  let release;
  let held = true;
  const slow = await failuresRefresher(failing, () => (held
    ? new Promise((resolve) => { release = () => resolve({ body }); })
    : { body }));
  const first = slow.context.refreshRunFailures();
  await slow.context.refreshRunFailures();
  await slow.context.refreshRunFailures();
  assert.equal(slow.calls.requests.length, 1);
  held = false;
  release();
  await first;
  assert.equal(slow.context.runFailuresRefreshing, false);
  await slow.context.refreshRunFailures();
  assert.equal(slow.calls.requests.length, 2);

  // The answer arrived after the console moved to another server.
  const moved = await failuresRefresher(failing, () => ({ body }));
  moved.context.apiResponse = async () => { moved.context.folderDeviceId = null; return { body }; };
  await moved.context.refreshRunFailures();
  assert.equal(moved.context.runFailures.size, 0);
  assert.equal(moved.calls.renders, 0);

  const app = await source("app.js");
  // Link status never waits for this optional read.
  assert.match(definition(app, "loadFolders"), /if \(result\.applied\) \{[^}]*void refreshRunFailures\(\);/);
  assert.doesNotMatch(definition(app, "loadFolders"), /await refreshRunFailures/);
  assert.match(definition(app, "clearFolderSyncAccess"), /runFailures = new Map\(\);\s*runFailuresAvailable = true;/);
  assert.match(definition(app, "refreshLinkRun"), /runFailures\.get\(share\.folderId\) \?\? null\]\);/,
    "a cause that arrives later redraws the run details");
});
