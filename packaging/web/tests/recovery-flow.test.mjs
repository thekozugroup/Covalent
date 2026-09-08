import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";
const flow = createRequire(import.meta.url)("../recovery-flow.js");
const provider = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const backupId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const encoded = () => ({ protocolVersion: 1, recoveryKit: Buffer.from("encrypted kit fixture").toString("base64url"), recoveryKey: Buffer.alloc(32, 7).toString("base64url") });
const report = (overrides = {}) => ({ protocolVersion: 1, phase: "not_configured", recoveredBackups: [], configuredProviderIds: [], queriedProviderIds: [], failures: [], newerSnapshotMayExist: false, ...overrides });
const restored = { backupId, snapshotId: "snapshot-1", sourceProviderIds: [provider] };

test("export preserves canonical raw kit bytes and separate 43-byte code", () => {
  const value = encoded();
  const pair = flow.decodeExport(value);
  assert.equal(Buffer.from(pair.kit).toString(), "encrypted kit fixture");
  assert.equal(Buffer.from(pair.code).toString(), value.recoveryKey);
  flow.disposePair(pair);
  assert.ok(pair.kit.every((byte) => byte === 0));
  assert.ok(pair.code.every((byte) => byte === 0));
});

test("export rejects wrong versions, unknown fields, padding and noncanonical key bits", () => {
  const value = encoded();
  for (const invalid of [null, [], { ...value, protocolVersion: 2 }, { ...value, extra: true },
    { ...value, recoveryKit: "" }, { ...value, recoveryKit: "AA==" },
    { ...value, recoveryKit: "A" }, { ...value, recoveryKey: "A".repeat(42) + "B" },
    { ...value, recoveryKey: "password" }, { ...value, recoveryKit: "A".repeat(Math.ceil(flow.MAX_KIT_BYTES * 4 / 3) + 1) }]) {
    assert.throws(() => flow.decodeExport(invalid), /recovery response/);
  }
});

test("closing during export ignores delayed response and permits a separate new attempt", async () => {
  const session = flow.session();
  let finish;
  let signal;
  const pending = session.generate((_path, options) => { signal = options.signal; return new Promise((resolve) => { finish = resolve; }); });
  await assert.rejects(session.generate(() => assert.fail()), /current recovery files/);
  session.dispose();
  assert.equal(signal.aborted, true);
  finish(encoded());
  assert.equal(await pending, false);
  assert.throws(() => session.download("kit", () => assert.fail()), /Create recovery files/);
  assert.equal(await session.generate(async () => encoded()), true);
  session.dispose();
});

test("download retries reuse the same pair and wipe callback buffers even on failure", async () => {
  const session = flow.session();
  let calls = 0;
  await session.generate(async (path, options) => {
    calls++;
    assert.equal(path, "/api/v1/recovery/kit");
    assert.deepEqual(JSON.parse(options.body), { confirmed: true });
    return encoded();
  });
  let handedOff;
  assert.throws(() => session.download("code", (bytes) => { handedOff = bytes; throw new Error("failed download"); }), /failed download/);
  assert.ok(handedOff.every((byte) => byte === 0));
  let code;
  session.download("code", (bytes) => { code = Buffer.from(bytes).toString(); });
  assert.equal(code, encoded().recoveryKey);
  assert.equal(calls, 1);
  session.dispose();
  assert.throws(() => session.download("code", () => assert.fail()), /Create recovery files/);
});

test("bounded response reader refuses oversized responses and redacts JSON excerpts", async () => {
  assert.deepEqual(await flow.readJson(new Response(JSON.stringify(encoded()))), encoded());
  await assert.rejects(flow.readJson(new Response("never read", { headers: { "content-length": String(flow.MAX_RESPONSE_BYTES + 1) } })), /recovery response/);
  const secret = "do-not-repeat-this-key";
  await assert.rejects(flow.readJson(new Response(`{"recoveryKey":"${secret}`)), (error) => !String(error).includes(secret));
  await assert.rejects(flow.readJson(new Response("x".repeat(8193), { status: 500 })), /recovery response/);
});

test("every recovery phase has a conservative summary and retry behavior", () => {
  assert.equal(flow.status(report()).canRetry, false);
  for (const phase of ["pending", "blocked"]) {
    const value = flow.status(report({ phase, configuredProviderIds: [provider], newerSnapshotMayExist: true }));
    assert.equal(value.canRetry, true);
    assert.match(value.warning, /newer backup may still exist/);
  }
  assert.equal(flow.status(report({ phase: "no_catalogs", configuredProviderIds: [provider], queriedProviderIds: [provider] })).canRetry, true);
  assert.equal(flow.status(report({ phase: "imported", configuredProviderIds: [provider], queriedProviderIds: [provider], recoveredBackups: [restored] })).canRetry, false);
  const partial = flow.status(report({ phase: "partial", configuredProviderIds: [provider], queriedProviderIds: [provider], recoveredBackups: [restored], newerSnapshotMayExist: true }));
  assert.equal(partial.canRetry, true);
  assert.match(partial.detail, /1 backup recovered/);
});

test("contradictory, duplicate, unknown and excessive recovery status fails closed", () => {
  for (const value of [report({ phase: "unknown" }), report({ phase: "imported" }), report({ extra: "field" }),
    report({ configuredProviderIds: [provider, provider] }), report({ queriedProviderIds: [provider] }),
    report({ phase: "pending" }), report({ failures: Array(257).fill({}) }),
    report({ phase: "partial", configuredProviderIds: [provider], queriedProviderIds: [provider], recoveredBackups: [restored, restored], newerSnapshotMayExist: true }),
    report({ phase: "imported", configuredProviderIds: [provider], queriedProviderIds: [provider], recoveredBackups: [restored], newerSnapshotMayExist: true })]) {
    assert.throws(() => flow.status(value), /recovery response/);
  }
});


test("a verified backup remains visible when its provider fails a later catalog page", () => {
  const partial = flow.status(report({ phase: "partial", configuredProviderIds: [provider],
    queriedProviderIds: [], recoveredBackups: [restored], newerSnapshotMayExist: true,
    failures: [{ providerId: provider, snapshotId: null, reason: "recovery_catalog_provider_unavailable" }] }));
  assert.equal(partial.phase, "partial");
  assert.match(partial.detail, /1 backup recovered; 0 of 1/);
  assert.equal(partial.canRetry, true);
});
