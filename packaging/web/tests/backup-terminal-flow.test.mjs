import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const terminal = require("../backup-terminal-flow.js");

const PROVIDER_ID = "11111111-1111-4111-8111-111111111111";
const BACKUP_ID = "22222222-2222-4222-8222-222222222222";
const CONTEXT = Object.freeze({ origin: "https://atlas.example:8443", protocolVersion: 1 });
const OTHER_CONTEXT = Object.freeze({ origin: "https://replacement.example:8443", protocolVersion: 1 });
const NOW = 1_800_000_000_000;

function memoryStorage(values = new Map(), behavior = {}) {
  return {
    getItem(key) {
      if (behavior.failGet) throw new Error("get blocked");
      return values.get(key) ?? null;
    },
    setItem(key, value) {
      if (behavior.failSet) throw new Error("set blocked");
      if (!behavior.dropSet) values.set(key, value);
    },
    removeItem(key) {
      if (behavior.failRemove) throw new Error("remove blocked");
      values.delete(key);
    },
    has: (key) => values.has(key),
    values,
  };
}

class MemoryLockManager {
  held = false;

  async request(name, options, callback) {
    assert.equal(name, terminal.LOCK_NAME);
    assert.deepEqual(options, { mode: "exclusive", ifAvailable: true });
    if (this.held) return callback(null);
    this.held = true;
    try { return await callback({ name, mode: "exclusive" }); }
    finally { this.held = false; }
  }
}

function attempt(jobId = "backup-receipt-1") {
  return {
    sourceRoot: "/source",
    displayName: "Photos",
    snapshotId: "snapshot_1",
    jobId,
    selectedProviderIds: [PROVIDER_ID],
  };
}

function result(overrides = {}) {
  return {
    backupId: BACKUP_ID,
    snapshotId: "snapshot_1",
    entries: 3,
    bytesRead: 2_048,
    chunksStored: 2,
    chunksDeduplicated: 1,
    selectedProviders: 1,
    degradedFailures: 0,
    ...overrides,
  };
}

function terminalResponse(overrides = {}) {
  return { result: result(), acknowledgementRequired: true, ...overrides };
}

test("an interrupted response survives a tab/browser restart and retries the exact job", async () => {
  const durableOrigin = new Map();
  const firstWindow = memoryStorage(durableOrigin);
  const original = attempt();
  const sent = [];
  const request = async (value) => {
    sent.push(value);
    if (sent.length === 1) throw new TypeError("connection dropped after acceptance");
    return terminalResponse();
  };

  await assert.rejects(
    terminal.submit(firstWindow, CONTEXT, original, request, () => NOW),
    TypeError,
  );
  assert.equal((await terminal.load(firstWindow, CONTEXT, NOW)).phase, "request");

  // A new facade over the same durable origin models a closed and reopened
  // browser, not merely a reload of the original tab object.
  const reopenedWindow = memoryStorage(durableOrigin);
  const receipt = await terminal.submit(reopenedWindow, CONTEXT, original, request, () => NOW + 1);
  assert.equal(receipt.phase, "receipt");
  assert.equal(receipt.attempt.jobId, "backup-receipt-1");
  assert.equal(sent.length, 2);
  assert.equal(sent[0].jobId, sent[1].jobId, "retry must use the durable terminal job ID");
});

test("a decoded receipt survives browser restart until an exact 204 acknowledgement", async () => {
  const durableOrigin = new Map();
  const firstWindow = memoryStorage(durableOrigin);
  await terminal.submit(firstWindow, CONTEXT, attempt(), async () => terminalResponse(), () => NOW);

  const reopenedWindow = memoryStorage(durableOrigin);
  assert.equal((await terminal.load(reopenedWindow, CONTEXT, NOW + 1)).phase, "receipt");
  await terminal.acknowledge(
    reopenedWindow,
    CONTEXT,
    async () => ({ status: 204, body: null }),
    NOW + 2,
  );
  assert.equal(await terminal.load(reopenedWindow, CONTEXT, NOW + 3), null);
});

test("the client never acknowledges before decoding and accepting the terminal result", async () => {
  const storage = memoryStorage();
  const acknowledgements = [];
  await assert.rejects(
    terminal.submit(storage, CONTEXT, attempt(), async () => terminalResponse({
      result: result({ snapshotId: "wrong-snapshot" }),
    }), () => NOW),
    /completion Covalent cannot verify/,
  );
  assert.equal((await terminal.load(storage, CONTEXT, NOW)).phase, "request");
  await terminal.acknowledge(storage, CONTEXT, async (path) => acknowledgements.push(path), NOW);
  assert.deepEqual(acknowledgements, []);
});

test("a normal backup fails closed unless receipt acknowledgement is explicitly required", async () => {
  for (const acknowledgementRequired of [undefined, false, "true", null]) {
    const storage = memoryStorage();
    await assert.rejects(
      terminal.submit(storage, CONTEXT, attempt(), async () => ({
        result: result(),
        acknowledgementRequired,
      }), () => NOW),
      /did not require the expected receipt confirmation/,
    );
    assert.equal((await terminal.load(storage, CONTEXT, NOW)).phase, "request");
  }
});

test("acknowledgement failures retain the decoded receipt and retry only that job", async () => {
  const storage = memoryStorage();
  const receipt = await terminal.submit(
    storage,
    CONTEXT,
    attempt(),
    async () => terminalResponse(),
    () => NOW,
  );
  assert.equal(receipt.phase, "receipt");
  assert.equal((await terminal.load(storage, CONTEXT, NOW)).result.backupId, BACKUP_ID);

  const calls = [];
  await assert.rejects(terminal.acknowledge(storage, CONTEXT, async (path, options) => {
    calls.push({ path, body: JSON.parse(options.body) });
    throw new TypeError("ack transport interrupted");
  }, NOW), TypeError);
  assert.equal((await terminal.load(storage, CONTEXT, NOW)).phase, "receipt");

  await terminal.acknowledge(storage, CONTEXT, async (path, options) => {
    calls.push({ path, body: JSON.parse(options.body) });
    return { status: 204, body: null };
  }, NOW);
  assert.equal(await terminal.load(storage, CONTEXT, NOW), null);
  assert.deepEqual(calls, [
    { path: "/api/v1/jobs/acknowledge", body: { jobId: "backup-receipt-1" } },
    { path: "/api/v1/jobs/acknowledge", body: { jobId: "backup-receipt-1" } },
  ]);
});

test("only an exact 204 acknowledgement clears durable receipt state", async () => {
  const storage = memoryStorage();
  await terminal.submit(storage, CONTEXT, attempt(), async () => terminalResponse(), () => NOW);
  for (const response of [null, {}, { status: 200 }, { status: "204" }]) {
    await assert.rejects(
      terminal.acknowledge(storage, CONTEXT, async () => response, NOW),
      /did not confirm receipt with the expected response/,
    );
    assert.equal((await terminal.load(storage, CONTEXT, NOW)).phase, "receipt");
  }
  await terminal.acknowledge(storage, CONTEXT, async () => ({ status: 204 }), NOW);
  assert.equal(await terminal.load(storage, CONTEXT, NOW), null);
});

test("one acknowledged receipt at a time handles more than eight terminal jobs without accumulation", async () => {
  const storage = memoryStorage();
  const locks = new MemoryLockManager();
  const calls = [];
  for (let index = 0; index < 9; index += 1) {
    const current = attempt(`backup-receipt-${index}`);
    await terminal.withExclusiveLock(async () => {
      const receipt = await terminal.submit(storage, CONTEXT, current, async (sent) => {
        calls.push({ kind: "backup", jobId: sent.jobId });
        return terminalResponse();
      }, () => NOW + index);
      assert.equal(receipt.attempt.jobId, current.jobId);
      await terminal.acknowledge(storage, CONTEXT, async (path, options) => {
        calls.push({ kind: "ack", path, jobId: JSON.parse(options.body).jobId });
        return { status: 204 };
      }, NOW + index);
    }, locks);
    assert.equal(await terminal.load(storage, CONTEXT, NOW + index), null);
    assert.equal(storage.values.size, 0);
  }
  assert.equal(calls.filter((call) => call.kind === "backup").length, 9);
  assert.equal(calls.filter((call) => call.kind === "ack").length, 9);
  assert.deepEqual(
    calls.filter((call) => call.kind === "ack").map((call) => call.jobId),
    Array.from({ length: 9 }, (_, index) => `backup-receipt-${index}`),
  );
});

test("a pending durable receipt blocks any different backup", async () => {
  const storage = memoryStorage();
  await terminal.submit(storage, CONTEXT, attempt("backup-first"), async () => terminalResponse(), () => NOW);
  await assert.rejects(
    terminal.submit(storage, CONTEXT, attempt("backup-second"), async () => {
      throw new Error("must not send");
    }, () => NOW),
    /Finish confirming the previous backup result/,
  );
});

test("server-context mismatch fails closed without changing the saved receipt", async () => {
  const storage = memoryStorage();
  await assert.rejects(
    terminal.submit(storage, CONTEXT, attempt(), async () => {
      throw new TypeError("offline");
    }, () => NOW),
    TypeError,
  );
  const before = storage.getItem(terminal.STORAGE_KEY);
  await assert.rejects(
    terminal.load(storage, OTHER_CONTEXT, NOW),
    /belongs to a different backup server/,
  );
  assert.equal(storage.getItem(terminal.STORAGE_KEY), before);
});

test("corrupt, unknown, and oversized records fail closed and remain available for reconciliation", async (t) => {
  await t.test("invalid JSON", async () => {
    const storage = memoryStorage();
    storage.setItem(terminal.STORAGE_KEY, "{");
    await assert.rejects(terminal.load(storage, CONTEXT, NOW), /corrupted/);
    assert.equal(storage.getItem(terminal.STORAGE_KEY), "{");
  });

  await t.test("unknown field", async () => {
    const storage = memoryStorage();
    await assert.rejects(
      terminal.submit(storage, CONTEXT, attempt(), async () => { throw new TypeError("offline"); }, () => NOW),
      TypeError,
    );
    const record = JSON.parse(storage.getItem(terminal.STORAGE_KEY));
    record.unexpected = true;
    storage.setItem(terminal.STORAGE_KEY, JSON.stringify(record));
    await assert.rejects(terminal.load(storage, CONTEXT, NOW), /not safe to resume/);
    assert.equal(storage.has(terminal.STORAGE_KEY), true);
  });

  await t.test("oversized", async () => {
    const storage = memoryStorage();
    const oversized = "x".repeat(terminal.MAX_RECORD_BYTES + 1);
    storage.setItem(terminal.STORAGE_KEY, oversized);
    await assert.rejects(terminal.load(storage, CONTEXT, NOW), /not safe to resume/);
    assert.equal(storage.getItem(terminal.STORAGE_KEY), oversized);
  });
});

test("field tampering fails integrity verification and sends no request", async () => {
  const storage = memoryStorage();
  await assert.rejects(
    terminal.submit(storage, CONTEXT, attempt(), async () => { throw new TypeError("offline"); }, () => NOW),
    TypeError,
  );
  const record = JSON.parse(storage.getItem(terminal.STORAGE_KEY));
  record.attempt.jobId = "tampered-job";
  storage.setItem(terminal.STORAGE_KEY, JSON.stringify(record));
  await assert.rejects(terminal.load(storage, CONTEXT, NOW), /changed or corrupted/);
  assert.equal(storage.has(terminal.STORAGE_KEY), true);
});

test("a receipt older than seven days still resumes and acknowledges while future dates fail closed", async () => {
  const storage = memoryStorage();
  const sent = [];
  await assert.rejects(
    terminal.submit(storage, CONTEXT, attempt(), async (value) => {
      sent.push(value.jobId);
      throw new TypeError("response lost");
    }, () => NOW),
    TypeError,
  );
  const agedNow = NOW + terminal.RECEIPT_REVIEW_AFTER_MS + 1;
  const aged = await terminal.load(storage, CONTEXT, agedNow);
  assert.equal(aged.phase, "request");
  const receipt = await terminal.submit(storage, CONTEXT, aged.attempt, async (value) => {
    sent.push(value.jobId);
    return terminalResponse();
  }, () => agedNow);
  assert.equal(receipt.phase, "receipt");
  await terminal.acknowledge(storage, CONTEXT, async () => ({ status: 204 }), agedNow);
  assert.equal(await terminal.load(storage, CONTEXT, agedNow), null);
  assert.deepEqual(sent, ["backup-receipt-1", "backup-receipt-1"]);

  const futureStorage = memoryStorage();
  await assert.rejects(
    terminal.submit(futureStorage, CONTEXT, attempt(), async () => { throw new TypeError("offline"); }, () => NOW),
    TypeError,
  );
  const before = futureStorage.getItem(terminal.STORAGE_KEY);
  await assert.rejects(
    terminal.load(futureStorage, CONTEXT, NOW - 5 * 60 * 1_000 - 1),
    /dated in the future/,
  );
  assert.equal(futureStorage.getItem(terminal.STORAGE_KEY), before);
});

test("exclusive same-origin locking admits one concurrent tab and recovers the exact receipt after a crash", async () => {
  const durableOrigin = new Map();
  const firstTab = memoryStorage(durableOrigin);
  const secondTab = memoryStorage(durableOrigin);
  const locks = new MemoryLockManager();
  const sent = [];
  let releaseRequest;
  let markRequestStarted;
  const requestStarted = new Promise((resolve) => { markRequestStarted = resolve; });
  const requestGate = new Promise((resolve) => { releaseRequest = resolve; });

  const crashedTab = terminal.withExclusiveLock(
    () => terminal.submit(firstTab, CONTEXT, attempt("shared-job"), async (value) => {
      sent.push(value.jobId);
      markRequestStarted();
      await requestGate;
      throw new TypeError("tab closed after server acceptance");
    }, () => NOW),
    locks,
  );
  await requestStarted;

  let competingNetworkCalls = 0;
  await assert.rejects(
    terminal.withExclusiveLock(
      () => terminal.submit(secondTab, CONTEXT, attempt("different-job"), async () => {
        competingNetworkCalls += 1;
        return terminalResponse();
      }, () => NOW),
      locks,
    ),
    /Another tab is already handling/,
  );
  assert.equal(competingNetworkCalls, 0);
  assert.deepEqual(sent, ["shared-job"]);

  releaseRequest();
  await assert.rejects(crashedTab, TypeError);
  assert.equal(locks.held, false, "a rejected callback must release the Web Lock");
  const pending = await terminal.load(secondTab, CONTEXT, NOW + 1);
  assert.equal(pending.attempt.jobId, "shared-job");

  await terminal.withExclusiveLock(async () => {
    const receipt = await terminal.submit(secondTab, CONTEXT, pending.attempt, async (value) => {
      sent.push(value.jobId);
      return terminalResponse();
    }, () => NOW + 1);
    await terminal.acknowledge(secondTab, CONTEXT, async () => ({ status: 204 }), NOW + 1);
    assert.equal(receipt.attempt.jobId, "shared-job");
  }, locks);
  assert.deepEqual(sent, ["shared-job", "shared-job"]);
  assert.equal(await terminal.load(secondTab, CONTEXT, NOW + 2), null);
});

test("missing Web Locks support fails closed before the callback or network", async () => {
  let called = false;
  await assert.rejects(
    terminal.withExclusiveLock(async () => { called = true; }, null),
    /cannot coordinate durable backups across tabs/,
  );
  assert.equal(called, false);
});

test("receipt persistence is synchronous and verified before the backup request", async () => {
  const storage = memoryStorage(new Map(), { dropSet: true });
  let called = false;
  await assert.rejects(
    terminal.submit(storage, CONTEXT, attempt(), async () => { called = true; }, () => NOW),
    /could not retain the backup receipt safely/,
  );
  assert.equal(called, false);
});

test("strict receipt schema rejects secrets and persists no token", async () => {
  assert.throws(
    () => terminal.requireAttempt({ ...attempt(), token: "must-never-persist" }),
    /cannot be retained safely/,
  );
  assert.throws(
    () => terminal.requireContext({ ...CONTEXT, token: "must-never-persist" }),
    /server context cannot be retained safely/,
  );

  const storage = memoryStorage();
  await assert.rejects(
    terminal.submit(storage, CONTEXT, attempt(), async () => { throw new TypeError("offline"); }, () => NOW),
    TypeError,
  );
  const encoded = storage.getItem(terminal.STORAGE_KEY);
  assert.doesNotMatch(encoded, /token|authorization|bearer/i);
  assert.equal(storage.values.size, 1, "only one bounded durable operation may exist");
});
