import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { Link, MonitorSmartphone, PanelLeft, RefreshCw, Server } from "lucide-react";
import { readFile, writeFile } from "node:fs/promises";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "./card.mjs";
import { Sidebar, SidebarHeader, SidebarContent, SidebarMenu, SidebarMenuButton } from "./sidebar.mjs";

const path = new URL("../index.html", import.meta.url);
const html = await readFile(path, "utf8");
const license = await readFile(new URL("./LICENSE", import.meta.url), "utf8");
const iconLicense = await readFile(new URL("./LUCIDE-LICENSE", import.meta.url), "utf8");
const markup = renderToStaticMarkup(h(Card, { hidden: true },
  h(CardHeader, null,
    h(CardTitle, null, "Shared link settings"),
    h(CardDescription, null, "Edit here or on the other device. These choices apply to the whole link.")),
  h(CardContent)));
const block = `<!-- shadcn:link-settings:start -->\n    <template id="link-settings-card">${markup}</template>\n    <!-- shadcn:link-settings:end -->`;
const icon = (component, size = 20) => h(component, { size, strokeWidth: 1.75, "aria-hidden": true, focusable: false });
const mark = h("svg", { viewBox: "0 0 88 64", width: 26, height: 20, "aria-hidden": true, focusable: false },
  h("path", { d: "M44 11.215a24 24 0 0 1 0 41.57 24 24 0 0 1 0-41.57Z", fill: "currentColor", opacity: ".22" }),
  h("g", { fill: "none", stroke: "currentColor", strokeWidth: 3.5 },
    h("circle", { cx: 32, cy: 32, r: 24 }),
    h("circle", { cx: 56, cy: 32, r: 24 })));
const sidebar = renderToStaticMarkup(h(Sidebar, { id: "console-sidebar", "aria-label": "Console navigation" },
  h(SidebarHeader, null,
    h("span", { className: "sidebar-brand", "aria-label": "Covalent", role: "img" }, mark),
    h("button", { type: "button", "data-sidebar-toggle": "", "aria-label": "Collapse sidebar", "aria-expanded": true, "aria-controls": "console-sidebar", title: "Collapse sidebar" }, icon(PanelLeft))),
  h(SidebarContent, null,
    h(SidebarMenu, { role: "tablist", "aria-label": "Covalent actions", "aria-orientation": "vertical" },
      ...[
        ["folders", "Links", Link],
        ["pair", "Devices", MonitorSmartphone],
        ["settings", "Server", Server],
      ].map(([id, label, component]) => h(SidebarMenuButton, { key: id, id: `${id}-tab`, role: "tab", isActive: id === "folders", "aria-selected": id === "folders", "aria-controls": `${id}-panel`, tabIndex: id === "folders" ? 0 : -1, "data-tab": id, title: label, "aria-label": label }, icon(component), h("span", { className: "sidebar-label" }, label)))))));
let updated = html.replace(/<!-- shadcn:link-settings:start -->[\s\S]*?<!-- shadcn:link-settings:end -->/, block);
updated = updated.replace(/<!-- shadcn:sidebar:start -->[\s\S]*?<!-- shadcn:sidebar:end -->/, `<!-- shadcn:sidebar:start -->\n    <!-- shadcn/ui components adapted under the following license:\n${license}
Lucide icons:
${iconLicense.replace(/^---$/gm, "")}-->\n    ${sidebar}\n    <!-- shadcn:sidebar:end -->`);
updated = updated.replace(/(<button[^>]*data-refresh[^>]*>)[\s\S]*?(<\/button>)/, `$1${renderToStaticMarkup(icon(RefreshCw, 18))}$2`);
if (!html.includes("<!-- shadcn:link-settings:start -->")) throw new Error("Missing component template marker");
if (process.argv.includes("--check")) {
  if (updated !== html) throw new Error("Run npm run build in packaging/web/shadcn");
} else await writeFile(path, updated);
