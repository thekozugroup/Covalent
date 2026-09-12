import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const selection = require("../backup-selection-flow.js");
const backupId = "11111111-1111-4111-8111-111111111111";

test("remembered completed backups produce human labels without exposing IDs", () => {
  const choices = selection.choices([{
    backupId,
    name: "Family photos",
    latestSnapshotId: "snapshot-newest",
    snapshotCount: 3,
  }]);
  assert.deepEqual(choices, [{
    key: `${backupId}:snapshot-newest`,
    backupId,
    snapshotId: "snapshot-newest",
    label: "Family photos",
    detail: "3 retained snapshots; latest completed backup.",
    source: "remembered",
  }]);
  assert.doesNotMatch(choices[0].label, /11111111|snapshot-newest/);
});

test("a durable completed receipt remains selectable until the authoritative list catches up", () => {
  const choices = selection.choices([], {
    phase: "receipt",
    attempt: { displayName: "Phone documents" },
    result: { backupId, snapshotId: "snapshot-recent" },
  });
  assert.equal(choices[0].label, "Recent backup: Phone documents");
  assert.equal(choices[0].source, "receipt");
});

test("descriptors without a completed string snapshot never become restore choices", () => {
  const choices = selection.choices([
    { backupId, name: "Imported descriptor", latestSnapshotId: null, snapshotCount: 0 },
    { backupId: "22222222-2222-4222-8222-222222222222", name: "Missing snapshot", snapshotCount: 0 },
  ]);
  assert.deepEqual(choices, []);
  assert.throws(() => selection.resolve([], "", { backupId, snapshotId: null }), /Choose a completed backup/);
});

test("selection binds the exact remembered backup and stale selections never fall back to typed values", () => {
  const available = selection.choices([{ backupId, name: "Photos", latestSnapshotId: "snapshot-1", snapshotCount: 1 }]);
  assert.deepEqual(selection.resolve(available, available[0].key, { backupId: "22222222-2222-4222-8222-222222222222", snapshotId: "wrong" }), {
    backupId,
    snapshotId: "snapshot-1",
  });
  assert.throws(
    () => selection.resolve(available, `${backupId}:snapshot-stale`, { backupId, snapshotId: "snapshot-1" }),
    /no longer available/,
  );
});

test("manual recovery remains explicit and generated defaults are safe", () => {
  assert.deepEqual(selection.resolve([], "", { backupId, snapshotId: "recovery_snapshot" }), { backupId, snapshotId: "recovery_snapshot" });
  assert.equal(selection.defaultBackupName("/mnt/user/Photos/"), "Photos backup");
  assert.equal(selection.defaultBackupName("/"), "Selected files backup");
  assert.equal(
    selection.nextSnapshotId(() => "12345678-1234-4234-8234-123456789abc"),
    "snapshot-12345678123442348234123456789abc",
  );
  assert.throws(() => selection.nextSnapshotId(() => "not-a-uuid"), /safe snapshot identifier/);
});
