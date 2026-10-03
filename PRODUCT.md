# Product

<!-- impeccable:product-schema 1 -->

## Platform

adaptive

## Users

People who want the same folder available on their own Mac, Android devices, and servers. The server console supports managing existing shares without command-line configuration.

## Product Purpose

Covalent copies files from one source to one or more recipients. Success means a person can select an existing share, add a paired recipient, and see whether its copy is ready.

## Operating Context

The current deployment uses Atmos as the source of Music, with Waypoint and Atlas as recipients. Servers can connect over Tailscale. The browser console manages folders mounted into the server, not folders on the browser’s computer.

## Capabilities and Constraints

- The file-transfer engine uses rclone. Shares can run manually, on a schedule, or continuously.
- Timing and deletion settings belong to the share and apply to all recipients.
- Each member sees the share's source and all recipients. The source manages recipients; a receiver can stop receiving its own copy.
- Propagating source deletions and restoring recipient deletions are separate opt-in choices.
- Pairing requires comparison and confirmation of the same code on both devices. Discovery does not establish trust.
- The WebUI remains static HTML, CSS, and JavaScript. Shadcn components and Lucide icons are rendered at build time. No browser framework migration is required.

## Brand Commitments

Preserve the Covalent name and overlapping-circle logo. Keep the logo at the top left of the sidebar and Covalent as the main page heading. The user requested the supplied screenshot’s quiet sidebar, generous spacing, restrained colors, and serif headings. Navigation must collapse to icons.

## Product Principles

- Start with the existing share and its recipients.
- Ask for information only when it is needed for the operation.
- Keep pairing verification and deletion choices explicit.
- Show actionable errors and retain exact requests when retrying could otherwise duplicate work.
