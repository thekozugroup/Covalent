import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const restore = require("../restore-plan-flow.js");

const reference = {
  planId: "c".repeat(64),
  planDigest: "b".repeat(64),
  backupId: "11111111-1111-1111-1111-111111111111",
  snapshotId: "snapshot-test",
  authorizedRoot: "/restore",
  manifestDigest: "a".repeat(64),
  conflictPolicy: "fail",
  jobId: "restore-test",
  signerDeviceId: "22222222-2222-2222-2222-222222222222",
  signature: "signed",
  totalEntries: 2,
};

test("restore preview pages are bounded and bound to the durable signed reference", async () => {
  const calls = [];
  const api = async (path) => {
    calls.push(path);
    return {
      ...reference,
      entryOffset: 1,
      entries: [{ destinationPath: "folder/file.txt", kind: "file", action: "create_file" }],
      nextCursor: null,
    };
  };
  const page = await restore.page(api, reference, "1", 100);
  assert.equal(page.entryOffset, 1);
  assert.deepEqual(calls, ["/api/v1/restores/plans/" + reference.planId + "?limit=100&cursor=1"]);
  assert.throws(
    () => restore.requirePage(reference, { ...page, planDigest: "0".repeat(64) }, 100),
    /does not match/,
  );
});

test("restore execution sends only the durable plan ID and discards by job ID", async () => {
  const calls = [];
  const api = async (path, options) => {
    calls.push({ path, body: JSON.parse(options.body) });
    return path.endsWith("execute") ? { filesRestored: 2 } : null;
  };
  assert.deepEqual(await restore.execute(api, reference), { filesRestored: 2 });
  await restore.discard(api, reference);
  assert.deepEqual(calls, [
    { path: "/api/v1/restores/execute", body: { planId: reference.planId } },
    { path: "/api/v1/jobs/discard", body: { jobId: reference.jobId } },
  ]);
});
