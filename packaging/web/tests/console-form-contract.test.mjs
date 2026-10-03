import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { runInNewContext } from "node:vm";

const require = createRequire(import.meta.url);
const consoleRuntime = require("../app.js");
const tabs = require("../tab-flow.js");
const root = new URL("../", import.meta.url);

async function source(name) {
  return readFile(new URL(name, root), "utf8");
}

class FakeElement {
  constructor(dataset, selected = false) {
    this.dataset = dataset;
    this.hidden = false;
    this.attributes = new Map([
      ["aria-selected", String(selected)],
      ["tabindex", selected ? "0" : "-1"],
    ]);
    this.listeners = new Map();
  }

  addEventListener(type, listener) { this.listeners.set(type, listener); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  setAttribute(name, value) { this.attributes.set(name, value); }
  focus() {}
  fire(type) { this.listeners.get(type)?.({ preventDefault() {} }); }
}

test("unlock initializes Links without archive APIs and primary tabs work", async () => {
  const calls = [];
  const api = async (path) => { calls.push(path); return {}; };
  await consoleRuntime.initializeUnlockedConsole({
    authorize: () => api("/api/v1/config/export"),
    initializeLinks: async () => {
      await api("/api/v1/transport/identity");
      await api("/api/v1/sync/status");
    },
    refreshPairings: () => api("/api/v1/pair/network/pending"),
    onLinksError: assert.fail,
  });
  assert.deepEqual(calls, [
    "/api/v1/config/export",
    "/api/v1/transport/identity",
    "/api/v1/sync/status",
    "/api/v1/pair/network/pending",
  ]);
  assert.equal(calls.some((path) => /backup|restore|recovery|provider|archive/.test(path)), false);

  const names = ["pair", "folders", "settings"];
  const tabElements = names.map((name) => new FakeElement({ tab: name }, name === "folders"));
  const panels = names.map((name) => Object.assign(new FakeElement({ panel: name }), { hidden: name !== "folders" }));
  tabs.install({
    querySelectorAll(selector) {
      if (selector === "[data-tab]") return tabElements;
      if (selector === "[data-panel]") return panels;
      return [];
    },
  });
  tabElements[0].fire("click");
  assert.deepEqual(tabElements.map((tab) => tab.getAttribute("aria-selected")), ["true", "false", "false"]);
  assert.deepEqual(panels.map((panel) => panel.hidden), [false, true, true]);
});

test("console access uses trusted claim output and never a server-state token path", async () => {
  const html = await source("index.html");
  assert.match(html, /data-access-state hidden>Console locked/);
  assert.match(html, /data-access-panel hidden>/);
  assert.match(html, /owner-only output directory created by <code>covalent claim<\/code> on a trusted computer/);
  assert.match(html, /This console accepts a token only; it does not accept a setup code\./);
  assert.doesNotMatch(html, /\/data\/local-api-token/);
  assert.doesNotMatch(html, /token from <code>\/data\//);
});

test("trusted access initializes Links and pairings before showing its state", async () => {
  const app = await source("app.js");
  const body = app.slice(app.indexOf("function initializeConsoleAccess()"), app.indexOf('$("[data-token-file-choose]")'));
  const calls = [];
  const elements = new Map([
    ["[data-access-panel]", { hidden: true, open: false }],
    ["[data-access-state]", { hidden: true, textContent: "Console locked" }],
  ]);
  const context = {
    accessReady: false,
    errorCopy: consoleRuntime,
    api: async (path, options) => { calls.push([path, options.method]); },
    initializeFolderSync: () => { assert.equal(context.accessReady, true); calls.push(["Links"]); },
    refreshNetworkPairings: () => { assert.equal(context.accessReady, true); calls.push(["pairings"]); },
    clearFolderSyncAccess: assert.fail,
    renderFolderError: assert.fail,
    $: (selector) => elements.get(selector),
    fail: assert.fail,
  };
  runInNewContext(body + "\nthis.tryTrustedAccess = tryTrustedAccess;", context);
  await context.tryTrustedAccess();
  assert.deepEqual(calls, [["/api/v1/config/export", "POST"], ["Links"], ["pairings"]]);
  assert.equal(elements.get("[data-access-panel]").hidden, true);
  assert.equal(elements.get("[data-access-state]").textContent, "Trusted access");
});

test("failed trusted access reveals token fallback and preserves network failures", async () => {
  const app = await source("app.js");
  const body = app.slice(app.indexOf("function initializeConsoleAccess()"), app.indexOf('$("[data-token-file-choose]")'));
  const elements = new Map([
    ["[data-access-panel]", { hidden: true, open: false }],
    ["[data-access-state]", { hidden: true, textContent: "Console locked" }],
  ]);
  const failures = [];
  const context = {
    accessReady: false,
    errorCopy: consoleRuntime,
    api: async () => { throw new TypeError("network unavailable"); },
    initializeFolderSync: assert.fail,
    refreshNetworkPairings: assert.fail,
    clearFolderSyncAccess() {},
    renderFolderError: assert.fail,
    $: (selector) => elements.get(selector),
    fail: (error) => failures.push(error),
    NodeApiError: class extends Error { constructor(status) { super(); this.status = status; } },
  };
  runInNewContext(body + "\nthis.tryTrustedAccess = tryTrustedAccess;", context);
  await context.tryTrustedAccess();
  assert.equal(context.accessReady, false);
  assert.equal(elements.get("[data-access-panel]").hidden, false);
  assert.equal(elements.get("[data-access-panel]").open, true);
  assert.equal(failures.length, 1);

  elements.get("[data-access-panel]").hidden = true;
  context.api = async () => { throw new context.NodeApiError(401); };
  await context.tryTrustedAccess();
  assert.equal(elements.get("[data-access-panel]").hidden, false);
  assert.equal(failures.length, 1, "expected authorization denial should only show token fallback");
});

test("API requests identify the console without a token and guards use access state", async () => {
  const app = await source("app.js");
  const responseSource = app.slice(app.indexOf("async function apiResponse("), app.indexOf("\nasync function api("));
  const requests = [];
  const apiResponse = new Function("Headers", "fetch", "token", "PROTOCOL_VERSION", "NodeApiError", "ProtocolMismatchError",
    responseSource + "\nreturn apiResponse;")(Headers, async (path, options) => {
      requests.push({ path, options });
      return { ok: true, status: 204, headers: new Headers() };
    }, "", 1, Error, Error);
  await apiResponse("/api/v1/config/export", { method: "POST" });
  assert.equal(requests[0].options.headers.get("X-Covalent-Console"), "1");
  assert.equal(requests[0].options.headers.has("Authorization"), false);
  assert.equal(requests[0].options.headers.has("X-Covalent-Endpoint-Roster"), false);
  for (const options of [{}, { method: "GET" }, { method: "get" }]) {
    await apiResponse("/api/v1/sync/status", options);
    assert.equal(requests.at(-1).path, "/api/v1/sync/status");
    assert.equal(requests.at(-1).options.headers.get("X-Covalent-Endpoint-Roster"), "1");
  }
  await apiResponse("/api/v1/sync/status", { method: "POST" });
  assert.equal(requests.at(-1).options.headers.has("X-Covalent-Endpoint-Roster"), false);
  await apiResponse("/api/v1/status");
  assert.equal(requests.at(-1).options.headers.has("X-Covalent-Endpoint-Roster"), false);
  assert.match(app, /function folderPollingEligible\(\)[\s\S]*?accessReady\s*&&/);
  assert.match(app, /async function refreshNetworkPairings\(\) \{\s*if \(!accessReady\) return;/);
  assert.match(app, /function requireUnlocked\(\) \{\s*if \(accessReady\) return true;/);
  assert.match(app, /\[data-refresh\][\s\S]*?if \(!accessReady\) return;/);
});

test("transient confirmation clears without discarding later instructions or errors", async () => {
  const app = await source("app.js");
  const saySource = app.slice(app.indexOf("function say("), app.indexOf("\n// The only path from a thrown value"));
  const timers = new Map();
  const message = { textContent: "", classList: { toggle() {} } };
  const context = { message, messageDetail: {}, messageDetails: {}, messageTimer: null,
    setTimeout(callback) { timers.set(1, callback); return 1; },
    clearTimeout(id) { timers.delete(id); } };
  runInNewContext(saySource, context);
  context.say("Unlocked", false, null, true);
  timers.get(1)();
  assert.equal(message.textContent, "");
  context.say("Unlocked", false, null, true);
  context.say("Compare the pairing code");
  assert.equal(timers.size, 0);
  assert.equal(message.textContent, "Compare the pairing code");
  context.say("Failed", true, "Retry after checking the address", true);
  assert.equal(timers.size, 0);
  assert.equal(context.messageDetails.hidden, false);
  assert.equal(context.messageDetail.textContent, "Retry after checking the address");
});

test("token-file selection validates locally without unlocking or exposing rejected content", async () => {
  const app = await source("app.js");
  const handler = /\$\("\[data-token-file\]"\)\.addEventListener\("change", async \(event\) => \{([\s\S]*?)\n\}\);/.exec(app);
  assert.ok(handler, "token-file selection handler must be present");
  const field = { value: "", focus() {} };
  const messages = [];
  const select = new Function("$", "say", `return async (event) => {${handler[1]}};`)(
    (selector) => { assert.equal(selector, "#api-token"); return field; },
    (message, failed = false) => messages.push({ message, failed }),
  );
  let reads = 0;
  const choose = async (value, size = value.length) => {
    const input = { value: "selected-file", files: [{ size, async text() { reads += 1; return value; } }] };
    await select({ currentTarget: input });
    assert.equal(input.value, "", "clear the file input so the same file can be chosen again");
  };
  const valid = "a".repeat(64);
  await choose(`${valid}\n`);
  assert.equal(field.value, valid);
  assert.equal(messages.at(-1).failed, false);
  assert.match(messages.at(-1).message, /Choose Unlock console/);
  for (const invalid of ["123456", "a".repeat(513), `${valid}\nsecret`, `${valid}"`, `${valid}\\`]) {
    await choose(invalid);
    assert.equal(messages.at(-1).failed, true);
    assert.equal(messages.at(-1).message.includes(invalid), false);
  }
  const readsBeforeOversize = reads;
  await choose("private-content", 4097);
  assert.equal(reads, readsBeforeOversize, "oversized files must be rejected before reading");
  assert.equal(messages.at(-1).failed, true);
});

test("primary console contains one-way Links and keeps legacy data available through the CLI", async () => {
  const html = await source("index.html");
  assert.match(html, /Create a one-way link/);
  assert.match(html, /covalent backups/);
  assert.match(html, /does not delete files, keys, archives, or settings/);
  assert.doesNotMatch(html, /data-backup-form|data-restore-preview|data-recovery-export|data-provider-selection/);
  assert.doesNotMatch(html, /(?:backup|restore|recovery)-(?:selection|terminal|verification|plan|preview|flow)\.js/);
});

test("link cadence controls use shared settings and Run Now contracts without exposing counters", async () => {
  const [html, app, flow] = await Promise.all([
    source("index.html"), source("app.js"), source("folder-sync-flow.js"),
  ]);
  assert.match(html, /name="cadenceMode" value="manual" required/);
  assert.match(html, /name="cadenceMode" value="scheduled" required/);
  assert.match(html, /name="cadenceMode" value="continuous" required/);
  assert.match(html, /name="intervalMinutes" type="number" min="15" max="525600"/);
  assert.match(html, /Wi-Fi only on Android devices/);
  assert.match(html, /Charging only on Android devices/);
  assert.match(app, /folderController\.updateLinkSettings\(share\.folderId, settings\)/);
  assert.match(app, /folderController\.runNow\(share\.folderId\)/);
  assert.match(flow, /"\/api\/v1\/sync\/run"/);
  assert.match(flow, /expectedGeneration: share\.linkRun\.generation/);
  assert.match(flow, /settingsRevision: share\.linkSettings\.revision/);
  assert.doesNotMatch(html, /generation|settings revision/i);
  assert.doesNotMatch(app, /textContent\s*=.*(?:generation|revision)/i);
  assert.doesNotMatch(app, /\/api\/v1\/sync\/conditions/);
});

test("Manual and Scheduled render Run Now while Continuous only renders its status", async () => {
  const app = await source("app.js");
  const body = /function renderLinkRun\(container, status, share\) \{([\s\S]*?)\n\}\n/.exec(app)?.[1];
  assert.ok(body, "link run renderer must exist");
  const element = () => ({ children: [], append(...items) { this.children.push(...items); }, setAttribute() {} });
  const render = new Function("document", "folderController", "folderActionButton", "renderFolderError",
    `return (container, status, share) => {${body}};`)(
    { createElement: element }, { pendingRun: () => null },
    (label) => ({ label, disabled: false, dataset: {} }), assert.fail,
  );
  for (const mode of ["manual", "scheduled", "continuous"]) {
    const container = element();
    render(container, {}, {
      folderId: "folder",
      linkSettings: { confirmed: true, settings: { cadence: { mode }, paused: false } },
      linkRun: { phase: null, pendingRequest: null, rejectedRequest: null, nextDueAtUnixMs: null, destinations: [] },
    });
    const buttons = container.children.filter((item) => item.label);
    assert.equal(container.children.filter((item) => !item.label).length, 1, "transfer details remain available");
    assert.deepEqual(buttons.map((item) => item.label), mode === "continuous" ? [] : ["Run Now"]);
    assert.equal(buttons.some((item) => item.disabled), false);
  }
});

test("sender and receiver Settings buttons open editable shared controls and remain open after refresh", async () => {
  const app = await source("app.js");
  const body = /function renderLinkSettings\(container, status, share\) \{([\s\S]*?)\n\}\n/.exec(app)?.[1];
  assert.ok(body);
  const element = () => ({
    children: [], dataset: {}, attributes: {}, listeners: {},
    append(...items) { this.children.push(...items); },
    setAttribute(name, value) { this.attributes[name] = value; },
    addEventListener(name, handler) { this.listeners[name] = handler; },
  });
  for (const incoming of [false, true]) {
    const expanded = new Set();
    const submitted = [];
    const card = () => {
      const shell = element();
      const content = element();
      shell.append(content);
      shell.querySelector = () => content;
      return shell;
    };
    const render = new Function("document", "$", "expandedLinkSettings", "folderController", "cadenceControls", "cadenceValue", "runFolderMutation", "folderSync",
      `return (container, status, share) => {${body}};`)(
      { createElement: element }, () => ({ content: { firstElementChild: { cloneNode: card } } }), expanded,
      { isMutationLocked: () => false, updateLinkSettings: (id, settings) => submitted.push({ id, settings }) },
      () => ({ cadenceLabel: element(), intervalLabel: element(), cadence: { value: "scheduled" }, interval: { value: "60" } }),
      (mode, interval) => ({ mode, intervalMinutes: Number(interval) }), (action) => action(), {},
    );
    const share = { incoming, offerId: "offer", folderId: "shared-link", label: "Music", linkSettings: {
      confirmed: true, pendingChange: null, conflictedChange: null,
      settings: { cadence: { mode: "scheduled", intervalMinutes: 60 }, deletionPolicy: { propagateSourceDeletions: false, restoreLocalDeletions: false }, androidConditions: { wifiOnly: false, chargingOnly: false }, paused: false },
    } };
    const first = element();
    render(first, {}, share);
    const [button, panel] = first.children;
    assert.equal(button.attributes["aria-label"], "Settings for Music");
    assert.equal(panel.hidden, true);
    button.listeners.click();
    assert.equal(panel.hidden, false);
    assert.equal(button.attributes["aria-expanded"], "true");
    panel.children[0].children[0].listeners.submit({ preventDefault() {} });
    assert.deepEqual(submitted, [{ id: "shared-link", settings: share.linkSettings.settings }]);
    const refreshed = element();
    render(refreshed, {}, share);
    assert.equal(refreshed.children[1].hidden, false);
  }
});

test("sidebar navigation handles vertical arrow keys", () => {
  assert.equal(tabs.targetIndex(0, 3, "ArrowDown"), 1);
  assert.equal(tabs.targetIndex(0, 3, "ArrowUp"), 2);
});

test("service refresh updates node status without replacing shadcn sidebar contents", async () => {
  const [app, html, css] = await Promise.all([source("app.js"), source("index.html"), source("app.css")]);
  assert.match(html, /<aside[^>]*data-state="expanded"/);
  const sidebarHeader = /<div data-slot="sidebar-header">([\s\S]*?)<\/div>/.exec(html)?.[1];
  assert.match(sidebarHeader, /<span class="sidebar-brand"[^>]*aria-label="Covalent"[^>]*>/);
  assert.match(sidebarHeader, /<svg viewBox="0 0 88 64"/);
  assert.match(sidebarHeader, /<button[^>]*data-sidebar-toggle[^>]*>[\s\S]*lucide-panel-left/);
  assert.doesNotMatch(sidebarHeader, />Covalent</);
  assert.match(css, /\[data-state="collapsed"\] \.sidebar-label \{ display:none; \}/);
  assert.doesNotMatch(css, /\[data-state="collapsed"\] \.sidebar-brand[^}]*display:none/);
  const header = /<header>([\s\S]*?)<\/header>/.exec(html)?.[1];
  assert.match(header, /<h1>Covalent<\/h1>/);
  assert.doesNotMatch(header, /brand-mark|data-device-name/);
  assert.match(html, /Server: <strong data-device-name>Loading…<\/strong>/);
  assert.match(html, /<p[^>]*data-node-state[^>]*aria-live="polite"/);
  const body = /async function loadStatus\(\) \{([\s\S]*?)\n\}\n/.exec(app)?.[1];
  assert.ok(body);
  for (const offline of [false, true]) {
    const name = { textContent: "", dataset: {} };
    const status = { textContent: "" };
    const sidebar = { set textContent(value) { assert.fail(`Service refresh destroyed navigation: ${value}`); } };
    const load = new Function("$", "api", "document", "PROTOCOL_VERSION", "ProtocolMismatchError", "errorCopy", "fail",
      `return async () => {${body}};`)(
      (selector) => ({ "[data-device-name]": name, "[data-node-state]": status, "[data-state]": sidebar })[selector],
      async () => {
        if (offline) throw new Error("offline");
        return { protocolVersion: 1, deviceName: "Waypoint", state: "ready", lanDiscovery: false };
      },
      { querySelectorAll: () => [] }, 1, Error,
      { describe: () => ({ summary: "Server unavailable" }) }, () => {},
    );
    await load();
    assert.equal(name.textContent, offline ? "Node unavailable" : "Waypoint");
    assert.equal(status.textContent, offline ? "Server unavailable" : "Waypoint · Connected");
  }
});


test("Add recipient sends the existing share identity without asking for a source path", async () => {
  const app = await source("app.js");
  const body = /function renderAddDestination\(container, status, share\) \{([\s\S]*?)\n\}\n/.exec(app)?.[1];
  assert.ok(body);
  const element = () => ({ children: [], dataset: {}, listeners: {}, value: "", append(...items) { this.children.push(...items); }, addEventListener(name, callback) { this.listeners[name] = callback; } });
  const sent = [];
  const render = new Function("document", "folderController", "runFolderMutation", `return (container, status, share) => {${body}};`)(
    { createElement: element }, { sendOffer: (body) => sent.push(body) }, (action) => action(),
  );
  const container = element();
  const share = { folderId: "music", peerId: "waypoint", label: "Music", phase: "ready", linkPolicy: { propagateSourceDeletions: false, restoreLocalDeletions: false }, linkSettings: { settings: { cadence: { mode: "scheduled", intervalMinutes: 60 }, androidConditions: { wifiOnly: false, chargingOnly: false } } } };
  render(container, { shares: [share], peers: [{ peerId: "waypoint", displayName: "Waypoint" }, { peerId: "atlas", displayName: "Atlas" }] }, share);
  const details = container.children[0];
  assert.equal(details.children[0].textContent, "Add recipient");
  const form = details.children[2];
  assert.equal(form.children.length, 2, "only recipient selection and submit are needed");
  const select = form.children[0].children[0];
  assert.deepEqual(select.children.slice(1).map((option) => option.value), ["atlas"]);
  select.value = "atlas";
  form.listeners.submit({ preventDefault() {} });
  assert.deepEqual(sent, [{ peerId: "atlas", folderId: "music", label: "Music", linkPolicy: share.linkPolicy, cadence: share.linkSettings.settings.cadence, androidConditions: share.linkSettings.settings.androidConditions }]);
});

test("polling errors and busy feedback reveal a previously hidden healthy status", async () => {
  const app = await source("app.js");
  const status = { hidden: true, dataset: {} };
  const context = {
    $: () => status,
    errorCopy: { describe: () => ({ summary: "Server unavailable. Try again." }) },
    folderController: { refresh: async () => ({ busy: true, applied: false }) },
    fail: assert.fail,
  };
  for (const name of ["renderFolderError", "loadFolders"]) {
    const definition = new RegExp(`(?:async )?function ${name}\\([^]*?\\n\\}`).exec(app)?.[0];
    assert.ok(definition);
    runInNewContext(definition, context);
  }
  await context.loadFolders();
  assert.equal(status.hidden, false);
  assert.equal(status.dataset.kind, "checking");
  status.hidden = true;
  context.folderController.refresh = async () => { throw new Error("offline"); };
  await context.loadFolders();
  assert.equal(status.hidden, false);
  assert.equal(status.dataset.kind, "attention");
  assert.equal(status.textContent, "Server unavailable. Try again.");
});

test("transfer polling updates run details while retaining settings drafts, recipient selection, and focus", async () => {
  const app = await source("app.js");
  const element = () => ({
    children: [], dataset: {}, className: "", parent: null,
    append(...items) { for (const item of items) { item.parent = this; this.children.push(item); } },
    setAttribute() {},
    replaceChildren() { this.children = []; },
    remove() { this.parent.children.splice(this.parent.children.indexOf(this), 1); },
    insertBefore(item, reference) {
      if (item.parent) item.remove();
      item.parent = this;
      this.children.splice(reference === null ? this.children.length : this.children.indexOf(reference), 0, item);
    },
    querySelector(selector) {
      return this.children.find((item) => selector === ".link-run"
        ? item.className === "link-run" : Object.hasOwn(item.dataset, "linkRunButton")) ?? null;
    },
  });
  const list = element();
  const nodes = new Map([["[data-folders-list]", list]]);
  const document = { createElement: element, activeElement: null };
  let controlRenders = 0;
  let runRenders = 0;
  const context = {
    document, folderDeviceId: "atmos",
    $: (selector) => { if (!nodes.has(selector)) nodes.set(selector, element()); return nodes.get(selector); },
    folderSync: { statusSummary: () => ({ kind: "ready", text: "Ready" }), groupShares: (shares) => [shares], shareRecipients: require("../folder-sync-flow.js").shareRecipients, shareView: () => ({ kind: "ready", text: "Ready" }) },
    folderController: { pendingLinkSettings: () => null, pendingRun: () => null },
    renderFolderPeers() {}, renderPairedDevices() {}, renderFolderError: assert.fail,
    folderPeerName: () => "Waypoint", firstLinkMember: () => true, renderFolderActions() {},
    renderSharedFolderActions(container) {
      controlRenders += 1;
      const settings = element(); settings.value = "60";
      const recipient = element(); recipient.value = "";
      container.append(settings, recipient);
    },
    renderLinkRun(container, status, share) {
      runRenders += 1;
      const details = element(); details.className = "link-run"; details.textContent = share.linkRun.phase;
      const button = element(); button.dataset.linkRunButton = "";
      container.append(button, details);
    },
  };
  for (const name of ["refreshLinkRun", "renderFolderStatus"]) {
    const definition = new RegExp(`function ${name}\\([^]*?\\n\\}`).exec(app)?.[0];
    assert.ok(definition);
    runInNewContext(definition, context);
  }
  const share = { folderId: "music", offerId: "waypoint-offer", peerId: "waypoint", label: "Music", incoming: false, phase: "ready", expired: false,
    linkSettings: { confirmed: true, settings: { cadence: { mode: "scheduled", intervalMinutes: 60 }, paused: false } }, linkRun: { phase: null, generation: 0 } };
  const status = { shares: [share], peers: [{ peerId: "waypoint", displayName: "Waypoint" }], issue: null };
  context.renderFolderStatus(status);
  assert.equal(list.children[0].children[0].children[1].textContent, "From this server · Every hour");
  nodes.get("[data-device-name]").dataset.name = "Atmos";
  context.renderFolderStatus(status);
  assert.equal(list.children[0].children[0].children[1].textContent, "From Atmos · Every hour");
  const controls = list.children[0].children[2];
  const [settings, recipient] = controls.children;
  settings.value = "120";
  recipient.value = "atlas";
  document.activeElement = settings;
  controls.querySelector(".link-run").open = true;
  context.renderFolderStatus({ ...status, shares: [{ ...share, linkRun: { phase: "running", generation: 1 } }] });
  assert.equal(controlRenders, 1, "run state must not rebuild editable controls");
  assert.equal(runRenders, 2, "transfer display still updates");
  assert.equal(controls.children[0], settings);
  assert.equal(settings.value, "120");
  assert.equal(recipient.value, "atlas");
  assert.equal(document.activeElement, settings);
  assert.equal(controls.querySelector(".link-run").textContent, "running");
  assert.equal(controls.querySelector(".link-run").open, true);
  const incoming = { ...status, shares: [{ ...share, incoming: true }] };
  for (const ownName of ["Atlas", "Waypoint", undefined]) {
    nodes.get("[data-device-name]").dataset.name = ownName;
    context.renderFolderStatus(incoming);
    const recipientName = list.children[0].children[1].children[0].children[0].children[0].textContent;
    assert.equal(recipientName, ownName ?? "This server");
  }
  const roster = { revision: 1, label: "Music", source: { deviceId: "atmos", displayName: "Atmos" }, destinations: [
    { deviceId: "waypoint", displayName: "Waypoint" }, { deviceId: "atlas", displayName: "Atlas" },
  ] };
  const removed = [];
  context.folderActionButton = (label, action) => Object.assign(element(), { textContent: label, action });
  context.runFolderMutation = (action) => action();
  context.folderController.remove = (id) => removed.push(id);
  runInNewContext(/function renderFolderActions\([^]*?\n\}/.exec(app)[0], context);
  for (const ownId of ["atmos", "waypoint", "atlas"]) {
    context.folderDeviceId = ownId;
    const shares = ownId === "atmos"
      ? roster.destinations.map((destination) => ({ ...share, endpointRoster: roster, label: "My music", peerId: destination.deviceId, offerId: destination.deviceId + "-offer" }))
      : [{ ...share, endpointRoster: roster, label: "My music", incoming: true, peerId: "atmos", offerId: ownId + "-offer" }];
    context.renderFolderStatus({ ...status, shares });
    const card = list.children[0];
    assert.equal(card.children[0].children[0].textContent, "My music", "keep the locally resolved share alias");
    assert.equal(card.children[0].children[1].textContent, "From Atmos · Every hour");
    const rows = card.children[1].children;
    assert.deepEqual(rows.map((row) => row.children[0].children[0].textContent), ["Waypoint", "Atlas"]);
    for (const [index, row] of rows.entries()) {
      const local = ownId === "atmos" || ownId === roster.destinations[index].deviceId;
      const controls = row.children[1].children;
      if (!local) {
        assert.equal(row.children[0].children[1].textContent, "Receives this share");
        assert.equal(row.children[0].children[1].dataset.kind, "info");
        assert.equal(controls.length, 0, "a receiver cannot control another recipient");
      } else {
        assert.equal(controls[0].textContent, ownId === "atmos" ? "Remove recipient…" : "Stop receiving…");
        controls[1].children.at(-1).action();
        assert.equal(removed.at(-1), roster.destinations[index].deviceId + "-offer");
      }
    }
    const originalRows = [...rows];
    context.renderFolderStatus({ ...status, shares: shares.map((item) => ({ ...item, endpointRoster: { ...roster, revision: 2 } })) });
    assert.deepEqual(card.children[1].children, originalRows, "roster polling preserves recipient controls");
  }
});
