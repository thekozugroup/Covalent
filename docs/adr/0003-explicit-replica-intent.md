# ADR 0003: Explicit replica intent

Status: accepted.

Replica placement is a set of provider device IDs selected by the user for each backup. The scheduler may use all connected selected providers for speed and verification, but it cannot substitute or add an unselected provider.

Offline or undersized selections produce a visible degraded state. Availability is factual, not inferred from a requested replica count.
