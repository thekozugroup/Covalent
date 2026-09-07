import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const verification = require("../backup-verification-flow.js");
const backup = { backupId: "backup-one", latestSnapshotId: "snapshot-two", selectedProviderIds: ["atlas", "mac"] };
const intact = { verified: 2, missing: [], corrupt: [], intact: true, providerAvailability: { atlas: "complete", mac: "complete" } };

test("Verify checks the exact latest snapshot and explicitly selected copies without repairing", async () => {
  const calls = [];
  const result = await verification.verify(async (path, options) => {
    calls.push({ path, method: options.method, body: JSON.parse(options.body) });
    return intact;
  }, backup);
  assert.deepEqual(calls, [{
    path: "/api/v1/backups/verify", method: "POST",
    body: { backupId: "backup-one", snapshotId: "snapshot-two", verifyProviders: true, repair: false },
  }]);
  assert.equal(result.intact, true);
  assert.match(result.summary, /every selected backup device/);
});

test("local-only verification explains the need for another device", async () => {
  const result = await verification.verify(async (_path, options) => {
    assert.equal(JSON.parse(options.body).verifyProviders, false);
    return { ...intact, providerAvailability: {} };
  }, { ...backup, selectedProviderIds: [] });
  assert.equal(result.intact, true);
  assert.match(result.summary, /protect against losing this device/);
});

test("offline, damaged, revoked, incomplete, and omitted copies never report full protection", () => {
  for (const state of ["offline", "corrupt", "revoked", "degraded", undefined]) {
    const providerAvailability = { atlas: "complete" };
    if (state !== undefined) providerAvailability.mac = state;
    const result = verification.summarize({ ...intact, providerAvailability }, backup.selectedProviderIds);
    assert.equal(result.intact, false, String(state));
    assert.match(result.summary, /Reconnect those devices/);
  }
});

test("local corruption and missing content are reported even when extra copies are complete", () => {
  for (const damage of [{ missing: ["chunk-one"] }, { corrupt: ["chunk-two"] }]) {
    const result = verification.summarize({ ...intact, ...damage, intact: false }, backup.selectedProviderIds);
    assert.equal(result.intact, false);
    assert.match(result.summary, /Keep the original files/);
  }
});

test("malformed and contradictory reports cannot become a success message", () => {
  for (const report of [null, {}, { ...intact, intact: "true" }, { ...intact, verified: -1 },
    { ...intact, missing: ["lost"] }, { ...intact, providerAvailability: null },
    { ...intact, providerAvailability: { atlas: "unknown" } }, { ...intact, corrupt: [42] }]) {
    assert.throws(() => verification.summarize(report, backup.selectedProviderIds), /Invalid backup verification/);
  }
});

test("uncommitted snapshots never send a request and API failures remain retryable", async () => {
  let calls = 0;
  await assert.rejects(verification.verify(async () => { calls += 1; }, { ...backup, latestSnapshotId: null }));
  assert.equal(calls, 0);
  const error = new TypeError("offline");
  await assert.rejects(verification.verify(async () => { throw error; }, backup), (received) => received === error);
});

test("a concurrent selection update cannot weaken an in-flight verification", async () => {
  const selected = { ...backup, selectedProviderIds: ["atlas", "mac"] };
  const result = await verification.verify(async () => {
    selected.selectedProviderIds.pop();
    return { ...intact, providerAvailability: { atlas: "complete" } };
  }, selected);
  assert.equal(result.intact, false);
});
