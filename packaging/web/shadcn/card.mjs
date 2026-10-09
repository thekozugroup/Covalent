// shadcn/ui Card, MIT © 2023 shadcn. See LICENSE and upstream-card.tsx.
// JSX is expressed with createElement; local classes use our system-font theme.
import { createElement as h } from "react";

export function Card({ className = "", ...props }) {
  return h("section", { "data-slot": "card", className: `shared-settings-card ${className}`, ...props });
}

export function CardHeader(props) {
  return h("div", { "data-slot": "card-header", ...props });
}

export function CardTitle(props) {
  return h("h4", { "data-slot": "card-title", ...props });
}

export function CardDescription(props) {
  return h("p", { "data-slot": "card-description", ...props });
}

export function CardContent(props) {
  return h("div", { "data-slot": "card-content", ...props });
}
