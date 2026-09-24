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
    (label) => ({ label, disabled: false }), assert.fail,
  );
  for (const mode of ["manual", "scheduled", "continuous"]) {
    const container = element();
    render(container, {}, {
      folderId: "folder",
      linkSettings: { confirmed: true, settings: { cadence: { mode }, paused: false } },
      linkRun: { phase: null, pendingRequest: null, rejectedRequest: null, nextDueAtUnixMs: null, destinations: [] },
    });
    assert.equal(container.children.length, 1);
    const buttons = container.children[0].children.filter((item) => item.label);
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
  assert.match(css, /\[data-state="collapsed"\] \.sidebar-brand,\[data-state="collapsed"\] \.sidebar-label \{ display:none; \}/);
  const header = /<header>([\s\S]*?)<\/header>/.exec(html)?.[1];
  assert.match(header, /<h1>Covalent<\/h1>/);
  assert.doesNotMatch(header, /brand-mark|data-device-name/);
  assert.match(html, /Server: <strong data-device-name>Loading…<\/strong>/);
  assert.match(html, /<p[^>]*data-node-state[^>]*aria-live="polite"/);
  const body = /async function loadStatus\(\) \{([\s\S]*?)\n\}\n/.exec(app)?.[1];
  assert.ok(body);
  for (const offline of [false, true]) {
    const name = { textContent: "" };
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
    assert.equal(status.textContent, offline ? "Server unavailable" : "Service: ready");
  }
});
