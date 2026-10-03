# Console components

The shared settings panel uses shadcn/ui's MIT-licensed Card source, adapted to
semantic headings and Covalent's Apple-style system-font theme. The original
registry source is retained in `upstream-card.tsx`; `card.mjs` is its JSX-free
React adaptation. Source: https://ui.shadcn.com/r/styles/new-york-v4/card.json

We render the Card at build time into a checked-in HTML template. The existing
console inserts its live controls into CardContent. No React runtime, CDN,
Tailwind runtime, or browser dependency is shipped. This deliberately adopts
the Card component without converting the existing console into a React app.

After editing the component:

```sh
cd packaging/web/shadcn
npm ci --ignore-scripts
npm run build
npm run check
```

Rust and Docker embed the generated `index.html` directly, as before.

The collapsible sidebar uses the same approach. `sidebar.mjs` adapts the static
Sidebar, SidebarHeader, SidebarContent, SidebarMenu, and SidebarMenuButton
components from https://ui.shadcn.com/r/styles/new-york-v4/sidebar.json.
Covalent's existing tab controller owns navigation; a native DOM handler owns
collapse state. Interactive Radix sheets/tooltips and React hydration are not
included. Collapsed items keep accessible names and native title tooltips.


The navigation and refresh icons are genuine `lucide-react` 1.47.0 components,
rendered into inline SVG at build time. The pinned dependency and lockfile make
regeneration reproducible; the generated page includes the upstream ISC/MIT
notices retained in `LUCIDE-LICENSE`. Icons are decorative inside named buttons.
Source: https://lucide.dev/guide/react

The console uses the Tailwind CSS v3.4.17 palette directly as CSS tokens:
zinc neutrals, blue actions, emerald success and red errors. Light and dark
variants use different shades to retain contrast. This adopts the requested
colors without adding a framework or runtime stylesheet. Canonical values:
https://github.com/tailwindlabs/tailwindcss/blob/v3.4.17/src/public/colors.js
