// shadcn/ui Sidebar source adaptation, MIT © 2023 shadcn. See LICENSE.
// Only the static Sidebar/Header/Content/Menu/Button components are needed.
// Collapse state belongs to the existing console, with no React hydration.
import { createElement as h } from "react";

export function Sidebar(props) {
  return h("aside", { "data-slot": "sidebar", "data-state": "expanded", "data-collapsible": "icon", ...props });
}
export function SidebarHeader(props) {
  return h("div", { "data-slot": "sidebar-header", ...props });
}
export function SidebarContent(props) {
  return h("div", { "data-slot": "sidebar-content", ...props });
}
export function SidebarMenu(props) {
  return h("div", { "data-slot": "sidebar-menu", ...props });
}
export function SidebarMenuButton({ isActive = false, ...props }) {
  return h("button", { "data-slot": "sidebar-menu-button", "data-active": isActive, type: "button", ...props });
}
