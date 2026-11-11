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
  const names = ["pair", "folders", "settings"];
  const tabElements = names.map((name) => new FakeElement(
    { tab: name },
    { "aria-selected": name === "folders" ? "true" : "false", tabindex: name === "folders" ? "0" : "-1" },
  ));
  const panelNames = ["folders", "pair", "settings"];
  const panels = panelNames.map((name) => {
    const panel = new FakeElement({ panel: name });
    panel.hidden = name !== "folders";
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
  assert.match(html, /id="folders-tab" role="tab" aria-selected="true" aria-controls="folders-panel" tabindex="0"/);
  for (const name of ["pair", "settings"]) {
    assert.match(html,
      new RegExp(`id="${name}-tab" role="tab" aria-selected="false" aria-controls="${name}-panel" tabindex="-1"`));
    assert.match(html,
      new RegExp(`id="${name}-panel" role="tabpanel" aria-labelledby="${name}-tab" tabindex="0" data-panel="${name}" hidden`));
  }
  assert.match(html, /id="folders-panel" role="tabpanel" aria-labelledby="folders-tab" tabindex="0" data-panel="folders">/);
  assert.match(html, /<script src="\/assets\/folder-sync-flow\.js" defer><\/script>/);
  assert.match(html, /<script src="\/assets\/tab-flow\.js" defer><\/script>/);
});

test("ArrowLeft and ArrowRight wrap focus, selection, tabindex, and visible panel", () => {
  const fixture = tabFixture();
  tabs.install(fixture.documentRoot);
  assert.deepEqual(selectedState(fixture.tabElements), [
    ["false", "-1"], ["true", "0"], ["false", "-1"],
  ]);
  assert.deepEqual(fixture.panels.map((panel) => panel.hidden), [false, true, true]);

  const right = fixture.tabElements[1].fire("keydown", "ArrowRight");
  assert.equal(right.defaultPrevented, true);
  assert.equal(fixture.tabElements[2].focusCount, 1);
  assert.deepEqual(selectedState(fixture.tabElements), [
    ["false", "-1"], ["false", "-1"], ["true", "0"],
  ]);
  assert.deepEqual(fixture.panels.map((panel) => panel.hidden), [true, true, false]);

  const left = fixture.tabElements[0].fire("keydown", "ArrowLeft");
  assert.equal(left.defaultPrevented, true);
  assert.equal(fixture.tabElements[2].focusCount, 2);
  assert.deepEqual(fixture.panels.map((panel) => panel.hidden), [true, true, false]);
});

test("Home and End move to the first and last tabs with automatic activation", () => {
  const fixture = tabFixture();
  tabs.install(fixture.documentRoot);
  const end = fixture.tabElements[1].fire("keydown", "End");
  assert.equal(end.defaultPrevented, true);
  assert.equal(fixture.tabElements[2].focusCount, 1);
  assert.deepEqual(fixture.panels.map((panel) => panel.hidden), [true, true, false]);

  const home = fixture.tabElements[2].fire("keydown", "Home");
  assert.equal(home.defaultPrevented, true);
  assert.equal(fixture.tabElements[0].focusCount, 1);
  assert.deepEqual(fixture.panels.map((panel) => panel.hidden), [true, false, true]);
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

function themeBlocks(css) {
  const light = /^:root\s*\{([^}]*)\}/.exec(css)?.[1];
  const dark = /@media \(prefers-color-scheme:dark\)\s*\{\s*:root\s*\{([^}]*)\}/.exec(css)?.[1];
  assert.ok(light && dark, "both theme token blocks must exist");
  return [["light", light], ["dark", dark]];
}

function colourTokens(block) {
  return new Map([...block.matchAll(/--([a-z-]+):\s*(#[^;]*);/g)].map((match) => [match[1], match[2].trim()]));
}

test("dark redefines every colour the light theme defines, as plain hex", async () => {
  const css = await readFile(new URL("app.css", root), "utf8");
  const [[, light], [, dark]] = themeBlocks(css);
  const lightColours = colourTokens(light);
  const darkColours = colourTokens(dark);
  assert.ok(lightColours.size >= 12, "the token block lost its colours");
  assert.deepEqual([...darkColours.keys()].sort(), [...lightColours.keys()].sort(),
    "a colour defined for one theme only would show the other theme's value");
  for (const [theme, colours] of [["light", lightColours], ["dark", darkColours]]) {
    for (const [name, value] of colours) assert.match(value, /^#[0-9a-f]{6}$/i, `${theme} --${name}`);
  }
  // The page's own background and ink are set on :root beside the tokens.
  for (const block of [light, dark]) {
    assert.match(block, /background:#[0-9a-f]{6};/i);
    assert.match(block, /[; ]color:#[0-9a-f]{6};/i);
  }
  // No colour is written outside the two token blocks except the code block,
  // which is dark in both themes.
  const rules = css.replace(/^:root\s*\{[^}]*\}/, "").replace(/@media \(prefers-color-scheme:dark\)\s*\{\s*:root\s*\{[^}]*\}\s*\}/, "");
  const strays = [...rules.matchAll(/#[0-9a-f]{3,8}\b/gi)].map((match) => match[0].toUpperCase());
  assert.deepEqual(strays.sort(), ["#151619", "#F5F6F8"]);
  assert.match(css, /pre\s*\{[^}]*background:#151619; color:#F5F6F8;/);
});

test("text, status and control colours keep their contrast on every surface they sit on", async () => {
  const css = await readFile(new URL("app.css", root), "utf8");
  // [foreground, background, minimum]. 4.5:1 for text, 3:1 for the outline of a
  // control and for the focus ring.
  const pairs = [
    ["ink", "canvas", 4.5], ["ink", "surface", 4.5], ["ink", "sidebar", 4.5], ["ink", "sidebar-selected", 4.5],
    ["ink", "control-tone", 4.5], ["ink", "info-surface", 4.5], ["ink", "warning-surface", 4.5],
    ["muted", "canvas", 4.5], ["muted", "surface", 4.5], ["muted", "sidebar", 4.5], ["muted", "control-tone", 4.5],
    ["accent-text", "canvas", 4.5], ["accent-text", "surface", 4.5], ["accent-text", "sidebar-selected", 4.5],
    ["accent-text", "info-surface", 4.5], ["muted", "info-surface", 4.5], ["accent", "info-surface", 3],
    ["danger", "canvas", 4.5], ["danger", "surface", 4.5], ["danger", "danger-surface", 4.5],
    ["success", "canvas", 4.5], ["success", "surface", 4.5], ["success", "success-surface", 4.5],
    ["warning", "warning-surface", 4.5],
    ["control-border", "surface", 3], ["control-border", "canvas", 3],
    ["accent", "canvas", 3], ["accent", "surface", 3], ["accent", "sidebar", 3], ["accent", "sidebar-selected", 3],
    ["accent", "control-tone", 3], ["accent", "warning-surface", 3],
  ];
  for (const [theme, block] of themeBlocks(css)) {
    for (const [foreground, background, minimum] of pairs) {
      const ratio = contrast(token(block, foreground), token(block, background));
      assert.ok(ratio >= minimum, `${theme} ${foreground} on ${background} was ${ratio.toFixed(2)}:1, below ${minimum}:1`);
    }
  }
  // The uses those pairs stand for.
  assert.match(css, /button\.quiet \{[^}]*color:var\(--accent-text\)/);
  assert.match(css, /button\.quiet:hover \{ background:var\(--sidebar-selected\); \}/);
  assert.match(css, /button\.secondary \{ border-color:var\(--control-border\);/);
  assert.match(css, /\.toast\.error \{ color:var\(--danger\); background:var\(--danger-surface\); \}/);
  assert.match(css, /\.connection-banner strong \{[^}]*color:var\(--warning\)/);
  assert.match(css, /\.update-notice \{[^}]*background:var\(--info-surface\); color:var\(--ink\);/);
  assert.match(css, /(?:^|\n)a \{ color:var\(--accent-text\);/);
});

test("keyboard focus is visible everywhere, including the tab panels", async () => {
  const [css, html] = await Promise.all([
    readFile(new URL("app.css", root), "utf8"),
    readFile(new URL("index.html", root), "utf8"),
  ]);
  assert.doesNotMatch(css, /outline\s*:\s*(?:none|0)\b/, "no rule may remove the focus indicator");
  const ring = /([^{}]*:focus-visible[^{}]*)\{\s*outline:3px solid var\(--accent\); outline-offset:3px;\s*\}/.exec(css);
  assert.ok(ring, "the focus ring is a 3px accent outline with a 3px offset");
  const covered = ring[1].split(",").map((selector) => selector.trim());
  for (const selector of ["button", "input", "textarea", "select", "summary", "a", '[role="tabpanel"]']) {
    assert.ok(covered.includes(`${selector}:focus-visible`), `${selector} has no focus ring`);
  }
  // Everything the page makes focusable is one of those. The three panels are
  // focusable so their content can be reached; nothing else carries a tabindex.
  const focusable = [...html.matchAll(/<([a-z0-9]+)\b[^>]*\stabindex="(-?\d+)"/g)].map((match) => match[1]);
  assert.deepEqual([...new Set(focusable)].sort(), ["button", "section"]);
  assert.equal([...html.matchAll(/<section\b[^>]*role="tabpanel"[^>]*tabindex="0"/g)].length, 3);
});
