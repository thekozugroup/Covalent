import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import test from "node:test";

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
  assert.match(html, /owner-only output directory created by <code>covalent claim<\/code> on a trusted computer/);
  assert.match(html, /This console accepts a token only; it does not accept a setup code\./);
  assert.doesNotMatch(html, /\/data\/local-api-token/);
  assert.doesNotMatch(html, /token from <code>\/data\//);
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
