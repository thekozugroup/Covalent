import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { readFile, writeFile } from "node:fs/promises";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "./card.mjs";
import { Sidebar, SidebarHeader, SidebarContent, SidebarMenu, SidebarMenuButton } from "./sidebar.mjs";

const path = new URL("../index.html", import.meta.url);
const html = await readFile(path, "utf8");
const license = await readFile(new URL("./LICENSE", import.meta.url), "utf8");
const markup = renderToStaticMarkup(h(Card, { hidden: true },
  h(CardHeader, null,
    h(CardTitle, null, "Shared link settings"),
    h(CardDescription, null, "Edit here or on the other device. These choices apply to the whole link.")),
  h(CardContent)));
const block = `<!-- shadcn:link-settings:start -->\n    <template id="link-settings-card">${markup}</template>\n    <!-- shadcn:link-settings:end -->`;
const icon = (paths) => h("svg", { viewBox: "0 0 24 24", width: 20, height: 20, fill: "none", stroke: "currentColor", strokeWidth: 1.6, strokeLinecap: "round", strokeLinejoin: "round", "aria-hidden": true },
  ...paths.map((d) => h("path", { key: d, d })));
const sidebar = renderToStaticMarkup(h(Sidebar, { id: "console-sidebar", "aria-label": "Console navigation" },
  h(SidebarHeader, null,
    h("span", { className: "sidebar-brand" }, "Covalent"),
    h("button", { type: "button", "data-sidebar-toggle": "", "aria-label": "Collapse sidebar", "aria-expanded": true, "aria-controls": "console-sidebar", title: "Collapse sidebar" }, icon(["M4 4h16v16H4z", "M9 4v16"]))),
  h(SidebarContent, null,
    h(SidebarMenu, { role: "tablist", "aria-label": "Covalent actions", "aria-orientation": "vertical" },
      ...[
        ["folders", "Links", ["M10 13a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-2 2", "M14 11a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l2-2"]],
        ["pair", "Devices", ["M3 4h12v12H3z", "M6 20h6", "M9 16v4", "M17 9h5v11h-5z"]],
        ["settings", "Server", ["M4 3h16v7H4z", "M4 14h16v7H4z", "M8 6.5h.01", "M8 17.5h.01", "M12 6.5h5", "M12 17.5h5"]],
      ].map(([id, label, paths]) => h(SidebarMenuButton, { key: id, id: `${id}-tab`, role: "tab", isActive: id === "folders", "aria-selected": id === "folders", "aria-controls": `${id}-panel`, tabIndex: id === "folders" ? 0 : -1, "data-tab": id, title: label, "aria-label": label }, icon(paths), h("span", { className: "sidebar-label" }, label)))))));
let updated = html.replace(/<!-- shadcn:link-settings:start -->[\s\S]*?<!-- shadcn:link-settings:end -->/, block);
updated = updated.replace(/<!-- shadcn:sidebar:start -->[\s\S]*?<!-- shadcn:sidebar:end -->/, `<!-- shadcn:sidebar:start -->\n    <!-- shadcn/ui components adapted under the following license:\n${license}-->\n    ${sidebar}\n    <!-- shadcn:sidebar:end -->`);
if (!html.includes("<!-- shadcn:link-settings:start -->")) throw new Error("Missing component template marker");
if (process.argv.includes("--check")) {
  if (updated !== html) throw new Error("Run npm run build in packaging/web/shadcn");
} else await writeFile(path, updated);
