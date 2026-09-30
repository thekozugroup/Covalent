---
name: Covalent server console
description: A quiet share manager with serif headings and restrained controls.
colors:
  primary: "#2563eb"
  primary-hover: "#1d4ed8"
  canvas: "#fafafa"
  surface: "#ffffff"
  sidebar: "#f4f4f5"
  selected: "#e4e4e7"
  text: "#27272a"
  muted: "#52525b"
  border: "#e4e4e7"
  success: "#047857"
  danger: "#b91c1c"
typography:
  display:
    fontFamily: "Georgia, Times New Roman, serif"
    fontSize: "2.5rem"
    fontWeight: 400
    lineHeight: 1.15
    letterSpacing: "-0.025em"
  headline:
    fontFamily: "Georgia, Times New Roman, serif"
    fontSize: "1.9rem"
    fontWeight: 400
    letterSpacing: "-0.025em"
  body:
    fontFamily: "-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "15px"
    lineHeight: 1.6
rounded:
  control: "9px"
  card: "14px"
spacing:
  control: "10px"
  section: "24px"
  card: "28px"
  workspace: "48px"
components:
  button-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.surface}"
    rounded: "{rounded.control}"
    padding: "10px 17px"
    height: "44px"
  button-primary-hover:
    backgroundColor: "{colors.primary-hover}"
  share-card:
    backgroundColor: "{colors.surface}"
    rounded: "{rounded.card}"
    padding: "28px"
---

# Covalent server console

## Overview

Use the user’s supplied reference: pale sidebar, off-white canvas, serif headings, and generous space. The interface centers on the folder and its recipients. This record applies to the WebUI; native clients retain their platform conventions.

## Colors

Use the existing Tailwind zinc palette for surfaces and typography. Blue identifies primary actions and focus. Emerald and red convey actual status. Dark mode follows the system preference using the existing zinc, blue, emerald, and red counterparts in `packaging/web/app.css`.

## Typography

Georgia carries page and share headings, following the user’s explicit reference. Platform sans-serif carries navigation, forms, status, and actions. Body text is 15px; the main heading is 2.5rem, section headings 1.9rem, share headings 1.7rem. Mobile reduces the first two to 2rem and 1.65rem. Code and comparison codes use the platform monospace family.

## Layout

The sidebar is 224px wide and collapses to 72px. It keeps the logo visible in both states. Main content has a 1120px maximum width and 48px horizontal gutters. At 700px and below, the sidebar starts collapsed and content uses 18px gutters. Share cards group recipient rows under a single folder name, followed by the shared actions. Add recipient stays visible in the collapsed disclosure state.

## Elevation & Depth

Use borders and tonal surfaces. Cards have no shadows. The expanded mobile navigation alone uses a soft lateral shadow to distinguish its overlay.

## Shapes

Cards use a 14px radius. Buttons use 9px, inputs 8px. Controls have a 44px minimum height. Focus uses a 3px blue outline with a 3px offset.

## Components

Shadcn Sidebar and Card are generated static HTML. Lucide icons use a consistent 1.75 stroke. Native disclosures reveal recipient setup, transfer details, and advanced settings. Routine status polling retains typed input and focus. Settings affect all recipients; removing a recipient keeps existing files.

## Do's and Don'ts

- Keep one card per share and list its recipients together.
- Put a clear Add recipient action beside routine share actions.
- Keep the Covalent logo visible when navigation collapses.
- Preserve code comparison, explicit deletion choices, and useful error recovery.
- Do not reintroduce metric cards or repeat routine status explanations.
- Do not add decorative motion or a runtime framework.
