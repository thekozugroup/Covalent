# ADR 0002: Authenticated QUIC and independent discovery

Status: accepted.

Peer data uses encrypted QUIC with Covalent identity authentication and protocol negotiation. LAN mDNS, explicit addresses, remembered signed gossip, and Tailnet-aware hints locate candidates; none grants trust.

Tailscale supplies stable routing and optional MagicDNS, but Covalent owns pairing, authentication, authorization, and service discovery. LAN discovery is user-disableable without disabling manual or Tailnet connections.
