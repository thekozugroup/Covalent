# ADR 0004: Restore beneath an authorized root

Status: accepted.

Every restore is bound to one immutable platform-authorized target root. Manifest entries use a validated relative-path type. Resolution rejects absolute paths, parent traversal, dot components, prefixes, NULs, and symlink components; execution rechecks directory identity before atomic commit.

Apple access uses document/folder selection, security-scoped grants, and coordination. Android uses an explicitly granted SAF tree. Docker/Unraid restore requires an explicit writable mount distinct from read-only backup sources.
