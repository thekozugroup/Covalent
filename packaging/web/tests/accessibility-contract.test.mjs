import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import test from "node:test";

const require = createRequire(import.meta.url);
const tabs = require("../tab-flow.js");
const root = new URL("../", import.meta.url);

class FakeElement {
  constructor(dataset, attributes = {}) {
    this.dataset = dataset;
    this.attributes = new Map(Object.entries(attributes));
    this.hidden = false;
    this.focusCount = 0;
    this.listeners = new Map();
  }

  addEventListener(type, listener) { this.listeners.set(type, listener); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  setAttribute(name, value) { this.attributes.set(name, value); }
  focus() { this.focusCount += 1; }
  fire(type, key = undefined) {
    const event = {
      key,
      defaultPrevented: false,
      preventDefault() { this.defaultPrevented = true; },
    };
    this.listeners.get(type)?.(event);
    return event;
  }
}

function tabFixture() {
  const names = ["pair", "backup", "restore", "settings"];
  const tabElements = names.map((name, index) => new FakeElement(
    { tab: name },
    { "aria-selected": index === 0 ? "true" : "false", tabindex: index === 0 ? "0" : "-1" },
  ));
  const panels = names.map((name, index) => {
    const panel = new FakeElement({ panel: name });
    panel.hidden = index !== 0;
    return panel;
  });
  const documentRoot = {
    querySelectorAll(selector) {
      if (selector === "[data-tab]") return tabElements;
      if (selector === "[data-panel]") return panels;
      return [];
    },
  };
  return { documentRoot, panels, tabElements };
}

function selectedState(tabElements) {
  return tabElements.map((tab) => [tab.getAttribute("aria-selected"), tab.getAttribute("tabindex")]);
}

test("tab markup starts with one tab stop and one visible labelled panel", async () => {
  const html = await readFile(new URL("index.html", root), "utf8");
  assert.match(html, /id="pair-tab" role="tab" aria-selected="true" aria-controls="pair-panel" tabindex="0"/);
  for (const name of ["backup", "restore", "settings"]) {
    assert.match(
      html,
      new RegExp(`id="${name}-tab" role="tab" aria-selected="false" aria-controls="${name}-panel" tabindex="-1"`),
    );
    assert.match(
      html,
      new RegExp(`id="${name}-panel" role="tabpanel" aria-labelledby="${name}-tab" tabindex="0" data-panel="${name}" hidden`),
    );
  }
  assert.match(html, /id="pair-panel" role="tabpanel" aria-labelledby="pair-tab" tabindex="0" data-panel="pair">/);
  assert.match(html, /<script src="\/assets\/tab-flow\.js" defer><\/script>/);
});

test("ArrowLeft and ArrowRight wrap focus, selection, tabindex, and visible panel", () => {
  const fixture = tabFixture();
  tabs.install(fixture.documentRoot);
  assert.deepEqual(selectedState(fixture.tabElements), [
    ["true", "0"], ["false", "-1"], ["false", "-1"], ["false", "-1"],
  ]);
  assert.deepEqual(fixture.panels.map((panel) => panel.hidden), [false, true, true, true]);

  const right = fixture.tabElements[0].fire("keydown", "ArrowRight");
  assert.equal(right.defaultPrevented, true);
  assert.equal(fixture.tabElements[1].focusCount, 1);
  assert.deepEqual(selectedState(fixture.tabElements), [
    ["false", "-1"], ["true", "0"], ["false", "-1"], ["false", "-1"],
  ]);
  assert.deepEqual(fixture.panels.map((panel) => panel.hidden), [true, false, true, true]);

  const left = fixture.tabElements[0].fire("keydown", "ArrowLeft");
  assert.equal(left.defaultPrevented, true);
  assert.equal(fixture.tabElements[3].focusCount, 1);
  assert.deepEqual(fixture.panels.map((panel) => panel.hidden), [true, true, true, false]);
});

test("Home and End move to the first and last tabs with automatic activation", () => {
  const fixture = tabFixture();
  tabs.install(fixture.documentRoot);
  const end = fixture.tabElements[1].fire("keydown", "End");
  assert.equal(end.defaultPrevented, true);
  assert.equal(fixture.tabElements[3].focusCount, 1);
  assert.deepEqual(fixture.panels.map((panel) => panel.hidden), [true, true, true, false]);

  const home = fixture.tabElements[3].fire("keydown", "Home");
  assert.equal(home.defaultPrevented, true);
  assert.equal(fixture.tabElements[0].focusCount, 1);
  assert.deepEqual(fixture.panels.map((panel) => panel.hidden), [false, true, true, true]);
});

function token(block, name) {
  const match = new RegExp(`--${name}:([^;]+);`).exec(block);
  assert.ok(match, `missing --${name}`);
  return match[1].trim();
}

function luminance(hex) {
  assert.match(hex, /^#[0-9a-f]{6}$/i);
  const channels = [1, 3, 5].map((offset) => Number.parseInt(hex.slice(offset, offset + 2), 16) / 255);
  const linear = channels.map((channel) => (channel <= 0.04045
    ? channel / 12.92
    : ((channel + 0.055) / 1.055) ** 2.4));
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

function contrast(left, right) {
  const values = [luminance(left), luminance(right)].sort((a, b) => b - a);
  return (values[0] + 0.05) / (values[1] + 0.05);
}

test("primary labels retain WCAG AA contrast in light and dark default and hover states", async () => {
  const css = await readFile(new URL("app.css", root), "utf8");
  const light = /^:root\s*\{([^}]*)\}/.exec(css)?.[1];
  const dark = /@media \(prefers-color-scheme:dark\)\s*\{\s*:root\s*\{([^}]*)\}/.exec(css)?.[1];
  assert.ok(light && dark, "both theme token blocks must exist");
  for (const [theme, block] of [["light", light], ["dark", dark]]) {
    const foreground = token(block, "accent-contrast");
    for (const backgroundName of ["accent", "accent-strong"]) {
      const ratio = contrast(token(block, backgroundName), foreground);
      assert.ok(ratio >= 4.5, `${theme} ${backgroundName} contrast was ${ratio.toFixed(2)}:1`);
    }
  }
  assert.match(css, /button\s*\{[^}]*color:var\(--accent-contrast\)/);
  assert.doesNotMatch(css, /button\s*\{[^}]*color:white/);
  assert.match(css, /input,textarea,select\s*\{[^}]*min-height:44px/);
});
