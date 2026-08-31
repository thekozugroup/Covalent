(function exportTabFlow(root, factory) {
  const flow = factory();
  if (typeof module === "object" && module.exports) module.exports = flow;
  root.CovalentTabFlow = flow;
})(typeof globalThis === "object" ? globalThis : this, function createTabFlow() {
  "use strict";

  function targetIndex(current, count, key) {
    if (!Number.isInteger(current) || !Number.isInteger(count) || count < 1
      || current < 0 || current >= count) return null;
    if (key === "ArrowRight") return (current + 1) % count;
    if (key === "ArrowLeft") return (current - 1 + count) % count;
    if (key === "Home") return 0;
    if (key === "End") return count - 1;
    return null;
  }

  function activate(tabs, panels, target, moveFocus = false) {
    if (!Array.isArray(tabs) || !Array.isArray(panels) || !tabs.includes(target)) {
      throw new TypeError("tab activation requires one tab from this tablist");
    }
    for (const tab of tabs) {
      const selected = tab === target;
      tab.setAttribute("aria-selected", String(selected));
      tab.setAttribute("tabindex", selected ? "0" : "-1");
    }
    for (const panel of panels) panel.hidden = panel.dataset.panel !== target.dataset.tab;
    if (moveFocus) target.focus();
    return target;
  }

  function install(documentRoot) {
    const tabs = [...documentRoot.querySelectorAll("[data-tab]")];
    const panels = [...documentRoot.querySelectorAll("[data-panel]")];
    if (tabs.length === 0) return Object.freeze({ tabs, panels });
    const initial = tabs.find((tab) => tab.getAttribute("aria-selected") === "true") ?? tabs[0];
    activate(tabs, panels, initial);
    for (const tab of tabs) {
      tab.addEventListener("click", () => activate(tabs, panels, tab));
      tab.addEventListener("keydown", (event) => {
        const next = targetIndex(tabs.indexOf(tab), tabs.length, event.key);
        if (next === null) return;
        event.preventDefault();
        activate(tabs, panels, tabs[next], true);
      });
    }
    return Object.freeze({ tabs, panels });
  }

  return Object.freeze({ activate, install, targetIndex });
});
