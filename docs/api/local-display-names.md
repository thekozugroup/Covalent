# Local device and folder names

The authenticated local API can change how a paired device or shared folder is
named on one node, without changing its identity, signed records, membership,
paths, deletion settings, or transfer history.

Send `POST /api/v1/sync/display-names` with one of these JSON bodies:

```json
{"type":"peer","peerId":"PEER_UUID_FROM_SYNC_STATUS","name":"Atlas"}
```

```json
{"type":"folder","folderId":"FOLDER_UUID_FROM_SYNC_STATUS","name":"Music"}
```

Use actual identifiers from `GET /api/v1/sync/status`. The node accepts a device
override only for a currently trusted, pinned peer and a folder override only
for a known local share. Success returns `204`; subsequent sync status and the
WebUI use the new names. A folder with several recipients has one local name.
Names must be nonblank, contain no control characters, and fit within 80 UTF-8
bytes for devices or 256 for folders. Send `"name":null` to clear an override.

These preferences persist in the node's private data directory across restarts
and normal updates. They are local display names: they do not automatically
propagate. Set the desired folder name on each node displaying that folder and
the desired device name on each node displaying that peer. To change a node's
own name, use the existing confirmed configuration import; these peer overrides
do not replace that setting.

Historical pairing and folder offers retain the names covered by their
signatures. Adding a recipient from a locally renamed source reuses its
retained source path and signed folder label; the recipient may then set its
own display preference. No re-pairing, link removal, or certificate change is
needed. Clearing a preference restores the historical name.
