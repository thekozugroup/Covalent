import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import test from "node:test";

const require = createRequire(import.meta.url);
const folders = require("../folder-sync-flow.js");

const deviceId = "11111111-1111-4111-8111-111111111111";
const otherDeviceId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const peerId = "22222222-2222-4222-8222-222222222222";
const offerId = "33333333-3333-4333-8333-333333333333";
const folderId = "44444444-4444-4444-8444-444444444444";
const linkPolicy = { propagateSourceDeletions: false, restoreLocalDeletions: false };
const changeId = "55555555-5555-4555-8555-555555555555";

function linkState(overrides = {}) {
  return {
    revision: 0,
    settings: { deletionPolicy: linkPolicy, paused: false },
    changeId: "00000000-0000-0000-0000-000000000000",
    changedBy: peerId,
    confirmed: false,
    pendingChange: null,
    conflictedChange: null,
    ...overrides,
  };
}

class MemoryStorage {
  constructor() { this.values = new Map(); }
  getItem(key) { return this.values.get(key) ?? null; }
  setItem(key, value) { this.values.set(key, value); }
  removeItem(key) { this.values.delete(key); }
}

function status(overrides = {}) {
  return {
    schemaVersion: 1,
    availability: "available",
    lifecycle: "running",
    issue: null,
    healthFreshness: "fresh",
    connectionFreshness: "fresh",
    peers: [{ peerId, displayName: "Kitchen server" }],
    shares: [],
    folders: [],
    ...overrides,
  };
}

function share(overrides = {}) {
  return {
    offerId,
    folderId,
    label: "Plans",
    peerId,
    incoming: true,
    phase: "offered",
    expiresAtUnixMs: 10,
    expired: false,
    peerConnection: "unknown",
    linkPolicy: null,
    linkSettings: null,
    ...overrides,
  };
}

function enable(controller) {
  controller.setAccess({ deviceId, unlocked: true });
  controller.setPollingEnabled(true);
}

test("server-declared expiry blocks acceptance before a mutation", async () => {
  const calls = [];
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path) => {
      calls.push(path);
      return status({ shares: [share({ expired: true })] });
    },
  });
  enable(controller);
  await controller.refresh();
  await assert.rejects(
    controller.accept(offerId, "/sync"),
    (error) => error.covalentGuidance === "That folder invitation has expired and cannot be accepted.",
  );
  assert.deepEqual(calls, ["/api/v1/sync/status"]);
});

test("a lost offer response reloads and retries the exact retained body", async () => {
  const storage = new MemoryStorage();
  const body = { peerId, folderId, label: "Plans", selectedRoot: "/sync", linkPolicy };
  const firstCalls = [];
  const first = folders.coordinator({
    storage,
    api: async (path, options) => {
      firstCalls.push({ path, body: JSON.parse(options.body) });
      throw new TypeError("connection closed after request");
    },
  });
  enable(first);
  await assert.rejects(first.sendOffer(body), TypeError);
  assert.deepEqual(first.loadPending(), body);

  const retryCalls = [];
  const reloaded = folders.coordinator({
    storage,
    api: async (path, options) => {
      retryCalls.push({ path, body: JSON.parse(options.body) });
      return { schemaVersion: 1, offerId, lifecycle: "running", issue: null };
    },
  });
  enable(reloaded);
  await reloaded.retryPendingOffer();
  assert.deepEqual(retryCalls, [{ path: "/api/v1/sync/folders", body }]);
  assert.deepEqual(firstCalls, [{ path: "/api/v1/sync/folders", body }]);
  assert.equal(reloaded.loadPending(), null);
});

test("renewal permits only expired outgoing invitations", async () => {
  for (const candidate of [
    share({ expired: true }),
    share({ incoming: false }),
    share({ incoming: false, expired: true, phase: "ready" }),
    share({ incoming: false, expired: true, phase: "removed" }),
  ]) {
    let mutations = 0;
    const controller = folders.coordinator({
      storage: new MemoryStorage(),
      api: async (path) => {
        if (path === "/api/v1/sync/status") return status({ shares: [candidate] });
        mutations += 1;
      },
    });
    enable(controller);
    await controller.refresh();
    await assert.rejects(controller.renew(offerId), /folder sync guidance/);
    assert.equal(mutations, 0);
  }
});

test("renewal metadata defaults safely and rejects ambiguous or excessive old identifiers", () => {
  assert.deepEqual(folders.requireStatus(status({ shares: [share()] })).shares[0].supersededOfferIds, []);
  const oldId = "55555555-5555-4555-8555-555555555555";
  for (const ids of [null, [offerId], [oldId, oldId], Array(129).fill(oldId), ["00000000-0000-0000-0000-000000000000"]]) {
    assert.throws(() => folders.requireStatus(status({ shares: [share({ supersededOfferIds: ids })] })));
  }
  assert.throws(() => folders.requireStatus(status({ shares: [
    share({ supersededOfferIds: [oldId] }), share({ offerId: oldId }),
  ] })));
  assert.deepEqual(folders.requireStatus(status({ shares: [share({ supersededOfferIds: [oldId] })] }))
    .shares[0].supersededOfferIds, [oldId]);
});

test("removal remains visible until the peer acknowledges and keeps old invitation IDs", () => {
  const oldId = "55555555-5555-4555-8555-555555555555";
  const snapshot = folders.requireStatus(status({ shares: [share({
    phase: "removed", remoteRemovalPending: true, supersededOfferIds: [oldId],
  })] }));
  const removed = snapshot.shares[0];
  assert.deepEqual(removed.supersededOfferIds, [oldId]);
  assert.equal(removed.remoteRemovalPending, true);
  assert.match(folders.shareView(snapshot, removed).text, /Stopped here.*confirm removal/);
  assert.equal(folders.shareView(snapshot, { ...removed, remoteRemovalPending: false }).text,
    "Sharing stopped. Files stay on both devices.");
  assert.equal(folders.requireStatus(status({ shares: [share()] })).shares[0].remoteRemovalPending, false);
  for (const value of [null, 1, "true", []]) {
    assert.throws(() => folders.requireStatus(status({ shares: [share({ phase: "removed", remoteRemovalPending: value })] })));
  }
  assert.throws(() => folders.requireStatus(status({ shares: [share({ remoteRemovalPending: true })] })));
});

test("lost renewal response retries the expired ID and preserves an unrelated draft", async () => {
  const storage = new MemoryStorage();
  const draft = { peerId, folderId, label: "Other plans", selectedRoot: "/other", linkPolicy };
  folders.pending.save(storage, deviceId, draft);
  const replacementId = "55555555-5555-4555-8555-555555555555";
  const calls = [];
  const controller = folders.coordinator({
    storage,
    api: async (path, options) => {
      if (path === "/api/v1/sync/status") {
        return status({ shares: [share({ incoming: false, expired: true, phase: "paused" })] });
      }
      calls.push({ path, method: options.method, body: JSON.parse(options.body) });
      if (calls.length === 1) throw new TypeError("response lost after renewal committed");
      return { schemaVersion: 1, offerId: replacementId, lifecycle: "running", issue: null };
    },
  });
  enable(controller);
  await controller.refresh();
  await assert.rejects(controller.renew(offerId), TypeError);
  assert.equal(controller.isMutationLocked(), false);
  assert.equal((await controller.renew(offerId)).offerId, replacementId);
  assert.deepEqual(calls, Array(2).fill({ path: "/api/v1/sync/renew", method: "POST", body: { offerId } }));
  assert.deepEqual(controller.loadPending(), draft);
});

test("renewal rejects a response without a different invitation ID", async () => {
  for (const returnedId of [undefined, null, offerId]) {
    const controller = folders.coordinator({
      storage: new MemoryStorage(),
      api: async (path) => path === "/api/v1/sync/status"
        ? status({ shares: [share({ incoming: false, expired: true })] })
        : { schemaVersion: 1, offerId: returnedId, lifecycle: "running", issue: null },
    });
    enable(controller);
    await controller.refresh();
    await assert.rejects(controller.renew(offerId), /folder sync guidance/);
    assert.equal(controller.isMutationLocked(), false);
  }
});

test("renewal serializes mutations and refuses a result for the previous server", async () => {
  let finish;
  let mutations = 0;
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path) => {
      if (path === "/api/v1/sync/status") return status({ shares: [share({ incoming: false, expired: true })] });
      mutations += 1;
      return new Promise((resolve) => { finish = resolve; });
    },
  });
  enable(controller);
  await controller.refresh();
  const request = controller.renew(offerId);
  await assert.rejects(controller.remove(offerId), /folder sync guidance/);
  assert.equal(mutations, 1);
  controller.setAccess({ deviceId: otherDeviceId, unlocked: true });
  finish({ schemaVersion: 1, offerId: folderId, lifecycle: "running", issue: null });
  await assert.rejects(request, (error) => /previous server/.test(error.covalentGuidance));
  assert.equal(controller.current(), null);
  assert.equal(controller.isMutationLocked(), false);
});

test("storage failure prevents an offer mutation", async () => {
  let calls = 0;
  const brokenStorage = {
    getItem() { return null; },
    setItem() { throw new Error("quota"); },
    removeItem() {},
  };
  const controller = folders.coordinator({
    storage: brokenStorage,
    api: async () => { calls += 1; },
  });
  enable(controller);
  await assert.rejects(
    controller.sendOffer({ peerId, folderId, label: "Plans", selectedRoot: "/sync", linkPolicy }),
    (error) => /no request was sent/i.test(error.covalentGuidance),
  );
  assert.equal(calls, 0);
});

test("a throwing sessionStorage getter does not prevent independent initialization", async () => {
  const scope = {};
  Object.defineProperty(scope, "sessionStorage", {
    get() { throw new DOMException("storage disabled", "SecurityError"); },
  });
  const lazyStorage = folders.lazySessionStorage(scope);
  let statusCalls = 0;
  let mutationCalls = 0;
  const controller = folders.coordinator({
    storage: lazyStorage,
    api: async (path) => {
      if (path === "/api/v1/sync/status") {
        statusCalls += 1;
        return status();
      }
      mutationCalls += 1;
      return { schemaVersion: 1, offerId, lifecycle: "running", issue: null };
    },
  });
  enable(controller);
  assert.equal((await controller.refresh()).applied, true);
  await assert.rejects(
    controller.sendOffer({ peerId, folderId, label: "Plans", selectedRoot: "/sync", linkPolicy }),
    (error) => /could not read its pending folder offer/i.test(error.covalentGuidance),
  );
  assert.equal(statusCalls, 1);
  assert.equal(mutationCalls, 0);
});

test("an older status response cannot overwrite a newer poll", async () => {
  const resolvers = [];
  const applied = [];
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: () => new Promise((resolve) => resolvers.push(resolve)),
    onStatus: (value) => applied.push(value.lifecycle),
  });
  enable(controller);
  const older = controller.refresh();
  const newer = controller.refresh();
  resolvers[1](status({ lifecycle: "initialScanning", healthFreshness: "neverObserved" }));
  assert.equal((await newer).applied, true);
  resolvers[0](status({ lifecycle: "running" }));
  assert.equal((await older).applied, false);
  assert.deepEqual(applied, ["initialScanning"]);
  assert.equal(controller.current().lifecycle, "initialScanning");
});

test("a delayed status response cannot cross an authoritative device-context change", async () => {
  let resolveStatus;
  const applied = [];
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: () => new Promise((resolve) => { resolveStatus = resolve; }),
    onStatus: (value) => applied.push(value.lifecycle),
  });
  enable(controller);
  const oldRequest = controller.refresh();
  controller.setAccess({ deviceId: otherDeviceId, unlocked: true });
  controller.setPollingEnabled(true);
  resolveStatus(status());
  assert.equal((await oldRequest).applied, false);
  assert.equal(controller.current(), null);
  assert.deepEqual(applied, []);
});

test("a delayed offer response retains the old server receipt after context changes", async () => {
  const storage = new MemoryStorage();
  const body = { peerId, folderId, label: "Plans", selectedRoot: "/sync", linkPolicy };
  let resolveMutation;
  const controller = folders.coordinator({
    storage,
    api: () => new Promise((resolve) => { resolveMutation = resolve; }),
  });
  enable(controller);
  const oldRequest = controller.sendOffer(body);
  controller.setAccess({ deviceId: otherDeviceId, unlocked: true });
  resolveMutation({ schemaVersion: 1, offerId, lifecycle: "running", issue: null });
  await assert.rejects(
    oldRequest,
    (error) => /previous server.*exact retry copy was retained/i.test(error.covalentGuidance),
  );
  assert.deepEqual(folders.pending.load(storage, deviceId), body);
  assert.equal(folders.pending.load(storage, otherDeviceId), null);
  assert.equal(controller.loadPending(), null);
});

test("folder status decoding rejects malformed and oversized responses", () => {
  assert.throws(
    () => folders.requireStatus(status({ lifecycle: "invented" })),
    /folder sync guidance/,
  );
  assert.throws(
    () => folders.requireStatus(status({ peers: Array(folders.MAX_COLLECTION + 1).fill({}) })),
    /folder sync guidance/,
  );
  assert.throws(
    () => folders.requireStatus(status({ shares: [share({ expired: "false" })] })),
    /folder sync guidance/,
  );
  assert.throws(
    () => folders.requireStatus(status({ connectionFreshness: null })),
    /folder sync guidance/,
  );
  assert.throws(
    () => folders.requireStatus(status({ shares: [share({ peerConnection: null })] })),
    /folder sync guidance/,
  );
});

test("folder response bytes are bounded before JSON parsing", async () => {
  const declared = new Response("{}", {
    headers: { "content-length": String(folders.MAX_RESPONSE_BYTES + 1) },
  });
  await assert.rejects(folders.readJson(declared), /folder sync guidance/);

  const oversized = new Response(`"${"x".repeat(folders.MAX_RESPONSE_BYTES)}"`);
  await assert.rejects(folders.readJson(oversized), /folder sync guidance/);
});

test("fresh reachability names the peer without claiming transfer completion", () => {
  const readyShare = share({ incoming: false, phase: "ready", expiresAtUnixMs: null, peerConnection: "connected" });
  const decoded = folders.requireStatus(status({
    shares: [readyShare],
    folders: [{
      folderId,
      state: "idle",
      remainingFiles: 0,
      remainingBytes: 0,
      scanPullErrorCount: 0,
      reportedErrorRows: 0,
      statusError: false,
      watchError: false,
    }],
  }));
  assert.deepEqual(folders.shareView(decoded, decoded.shares[0]), { kind: "ready", text: "Connected to Kitchen server" });
  assert.doesNotMatch(folders.shareView(decoded, decoded.shares[0]).text, /up to date|complete/i);

  const unknown = folders.requireStatus(status({
    connectionFreshness: "stale",
    shares: [readyShare],
    folders: decoded.folders,
  }));
  assert.deepEqual(folders.shareView(unknown, unknown.shares[0]), {
    kind: "unknown",
    text: "Connection status unknown",
  });

  const disconnected = folders.requireStatus(status({
    shares: [share({ ...readyShare, peerConnection: "disconnected" })],
    folders: decoded.folders,
  }));
  assert.deepEqual(folders.shareView(disconnected, disconnected.shares[0]), {
    kind: "waiting", text: "Waiting for Kitchen server",
  });
});

test("older status safely defaults new reachability fields to unknown", () => {
  const legacyShare = share({ phase: "ready" });
  delete legacyShare.peerConnection;
  const legacy = status({ shares: [legacyShare] });
  delete legacy.connectionFreshness;
  const decoded = folders.requireStatus(legacy);
  assert.equal(decoded.connectionFreshness, "neverObserved");
  assert.equal(decoded.shares[0].peerConnection, "unknown");
  assert.equal(decoded.peers[0].address, null);
});

test("paired addresses accept bounded numeric endpoints and reject ambiguous input", () => {
  for (const address of ["192.0.2.10:8787", "[2001:db8::10]:8787"]) {
    const decoded = folders.requireStatus(status({
      peers: [{ peerId, displayName: "Kitchen server", address }],
    }));
    assert.equal(decoded.peers[0].address, address);
    assert.equal(folders.peerAddress(address), address);
  }
  for (const address of [
    "host.example:8787", "192.0.2.999:8787", "192.0.2.1:0",
    "[fe80::1%en0]:8787", "2001:db8::1:8787", " 192.0.2.1:8787",
  ]) {
    assert.throws(() => folders.peerAddress(address), /folder sync guidance/);
  }
});

test("address refresh freezes the exact old and new endpoint until status confirms it", async () => {
  const oldAddress = "192.0.2.10:8787";
  const newAddress = "[2001:db8::10]:8787";
  const calls = [];
  let statusCount = 0;
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path, options) => {
      calls.push({ path, method: options?.method, body: options?.body && JSON.parse(options.body) });
      if (path === "/api/v1/sync/status") {
        statusCount += 1;
        return status({ peers: [{ peerId, displayName: "Kitchen server", address: statusCount === 1 ? oldAddress : newAddress }] });
      }
      return { schemaVersion: 1, lifecycle: "stopped", issue: null };
    },
  });
  enable(controller);
  await controller.refresh();
  await controller.refreshPeerAddress(peerId, newAddress);
  assert.deepEqual(calls[1], {
    path: "/api/v1/sync/peers/refresh-address",
    method: "POST",
    body: { peerId, expectedAddress: oldAddress, candidateAddress: newAddress },
  });
  assert.equal(controller.current().peers[0].address, newAddress);
  assert.equal(controller.pendingPeerAddressRefresh(), null);
});

test("a lost address response preserves only the exact retry and survives polling", async () => {
  const oldAddress = "192.0.2.10:8787";
  const candidateAddress = "192.0.2.20:8787";
  const posts = [];
  let fail = true;
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path, options) => {
      if (path === "/api/v1/sync/status") {
        return status({ peers: [{ peerId, displayName: "Kitchen server", address: fail ? oldAddress : candidateAddress }] });
      }
      posts.push(JSON.parse(options.body));
      if (fail) throw new TypeError("response lost");
      return { schemaVersion: 1, lifecycle: "stopped", issue: null };
    },
  });
  enable(controller);
  await controller.refresh();
  await assert.rejects(controller.refreshPeerAddress(peerId, candidateAddress), TypeError);
  const exact = { peerId, expectedAddress: oldAddress, candidateAddress };
  assert.deepEqual(controller.pendingPeerAddressRefresh(), exact);
  await controller.refresh();
  assert.deepEqual(controller.pendingPeerAddressRefresh(), exact);
  assert.throws(() => controller.refreshPeerAddress(peerId, "192.0.2.30:8787"), /folder sync guidance/);
  fail = false;
  await controller.retryPeerAddressRefresh(peerId);
  assert.deepEqual(posts, [exact, exact]);
  assert.equal(controller.pendingPeerAddressRefresh(), null);
});

test("a known changed-address rejection reloads status and requires a new submission", async () => {
  const oldAddress = "192.0.2.10:8787";
  const currentAddress = "192.0.2.15:8787";
  let statuses = 0;
  let posts = 0;
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path) => {
      if (path === "/api/v1/sync/status") {
        statuses += 1;
        return status({ peers: [{ peerId, displayName: "Kitchen server", address: statuses === 1 ? oldAddress : currentAddress }] });
      }
      posts += 1;
      const conflict = new Error("redacted API failure");
      conflict.code = "peer_address_changed";
      throw conflict;
    },
  });
  enable(controller);
  await controller.refresh();
  await assert.rejects(controller.refreshPeerAddress(peerId, "192.0.2.20:8787"),
    (error) => /Review its current address/.test(error.covalentGuidance));
  assert.equal(posts, 1);
  assert.equal(statuses, 2);
  assert.equal(controller.current().peers[0].address, currentAddress);
  assert.equal(controller.pendingPeerAddressRefresh(), null);
});

test("a failed conflict reload leaves no stale address available for resubmission", async () => {
  let statusCalls = 0;
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path) => {
      if (path === "/api/v1/sync/status") {
        statusCalls += 1;
        if (statusCalls === 2) throw new TypeError("status unavailable");
        return status({ peers: [{ peerId, displayName: "Kitchen server", address: "192.0.2.10:8787" }] });
      }
      const conflict = new Error("redacted API failure");
      conflict.code = "peer_address_changed";
      throw conflict;
    },
  });
  enable(controller);
  await controller.refresh();
  await assert.rejects(controller.refreshPeerAddress(peerId, "192.0.2.20:8787"),
    (error) => /Review its current address/.test(error.covalentGuidance));
  assert.equal(controller.current(), null);
  assert.throws(() => controller.refreshPeerAddress(peerId, "192.0.2.20:8787"),
    (error) => /no longer available/.test(error.covalentGuidance));
  assert.equal((await controller.refresh()).applied, true);
  assert.equal(controller.current().peers[0].address, "192.0.2.10:8787");
});

test("a delayed conflict reload cannot overwrite a newer normal poll", async () => {
  const oldAddress = "192.0.2.10:8787";
  const newerAddress = "192.0.2.18:8787";
  let statusCalls = 0;
  let finishConflictReload;
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path) => {
      if (path !== "/api/v1/sync/status") {
        const conflict = new Error("redacted API failure");
        conflict.code = "peer_address_changed";
        throw conflict;
      }
      statusCalls += 1;
      if (statusCalls === 1) {
        return status({ peers: [{ peerId, displayName: "Kitchen server", address: oldAddress }] });
      }
      if (statusCalls === 2) {
        return new Promise((resolve) => { finishConflictReload = resolve; });
      }
      return status({ peers: [{ peerId, displayName: "Kitchen server", address: newerAddress }] });
    },
  });
  enable(controller);
  await controller.refresh();
  const rejected = controller.refreshPeerAddress(peerId, "192.0.2.20:8787");
  while (finishConflictReload === undefined) await Promise.resolve();
  assert.equal((await controller.refresh()).applied, true);
  finishConflictReload(status({ peers: [{ peerId, displayName: "Kitchen server", address: oldAddress }] }));
  await assert.rejects(rejected, (error) => /Review its current address/.test(error.covalentGuidance));
  assert.equal(controller.current().peers[0].address, newerAddress);
});

test("same-device access reset during address POST rejects the obsolete completion", async () => {
  let finishPost;
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path) => {
      if (path === "/api/v1/sync/status") {
        return status({ peers: [{ peerId, displayName: "Kitchen server", address: "192.0.2.10:8787" }] });
      }
      return new Promise((resolve) => { finishPost = resolve; });
    },
  });
  enable(controller);
  await controller.refresh();
  const request = controller.refreshPeerAddress(peerId, "192.0.2.20:8787");
  controller.setAccess({ deviceId, unlocked: true });
  finishPost({ schemaVersion: 1, lifecycle: "stopped", issue: null });
  await assert.rejects(request, (error) => /Console access changed/.test(error.covalentGuidance));
  assert.equal(controller.current(), null);
  assert.equal(controller.pendingPeerAddressRefresh(), null);
});

test("same-device access reset during confirmation cannot apply the old session status", async () => {
  let statusCalls = 0;
  let finishConfirmation;
  let applied = 0;
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    onStatus: () => { applied += 1; },
    api: async (path) => {
      if (path !== "/api/v1/sync/status") {
        return { schemaVersion: 1, lifecycle: "stopped", issue: null };
      }
      statusCalls += 1;
      if (statusCalls === 1) {
        return status({ peers: [{ peerId, displayName: "Kitchen server", address: "192.0.2.10:8787" }] });
      }
      return new Promise((resolve) => { finishConfirmation = resolve; });
    },
  });
  enable(controller);
  await controller.refresh();
  const request = controller.refreshPeerAddress(peerId, "192.0.2.20:8787");
  while (finishConfirmation === undefined) await Promise.resolve();
  controller.setAccess({ deviceId, unlocked: true });
  finishConfirmation(status({ peers: [{ peerId, displayName: "Kitchen server", address: "192.0.2.20:8787" }] }));
  await assert.rejects(request, (error) => /Console access changed/.test(error.covalentGuidance));
  assert.equal(applied, 1);
  assert.equal(controller.current(), null);
  assert.equal(controller.pendingPeerAddressRefresh(), null);
});

test("a busy mutation cannot fabricate a pending address request", async () => {
  let finish;
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path) => {
      if (path === "/api/v1/sync/status") {
        return status({ peers: [{ peerId, displayName: "Kitchen server", address: "192.0.2.10:8787" }] });
      }
      return new Promise((resolve) => { finish = resolve; });
    },
  });
  enable(controller);
  await controller.refresh();
  const active = controller.retryService();
  assert.throws(() => controller.refreshPeerAddress(peerId, "192.0.2.20:8787"),
    (error) => /still in progress/.test(error.covalentGuidance));
  assert.equal(controller.pendingPeerAddressRefresh(), null);
  finish({ schemaVersion: 1, lifecycle: "stopped", issue: null });
  await active;
});

test("provider refresh failure cannot turn a confirmed address save into failure", async () => {
  const committed = { schemaVersion: 1, lifecycle: "stopped", issue: null };
  const providerFailure = new TypeError("provider response lost");
  const calls = [];
  const completion = await folders.refreshPeerAddressAndProviders({
    async refreshPeerAddress(id, candidate) {
      calls.push({ id, candidate });
      return committed;
    },
  }, peerId, "192.0.2.20:8787", async () => { throw providerFailure; });
  assert.deepEqual(calls, [{ id: peerId, candidate: "192.0.2.20:8787" }]);
  assert.equal(completion.result, committed);
  assert.equal(completion.providerError, providerFailure);
});

test("address refresh rejects unknown or legacy peers and cancellation clears its retry", async () => {
  let calls = 0;
  const controller = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path, options) => {
      calls += 1;
      if (path === "/api/v1/sync/status") return status();
      return { schemaVersion: 1, lifecycle: "stopped", issue: null };
    },
  });
  enable(controller);
  await controller.refresh();
  assert.throws(() => controller.refreshPeerAddress(peerId, "192.0.2.20:8787"),
    (error) => /server version/.test(error.covalentGuidance));
  assert.throws(() => controller.refreshPeerAddress(folderId, "192.0.2.20:8787"),
    (error) => /no longer available/.test(error.covalentGuidance));
  assert.equal(calls, 1);

  // Reload a status carrying the additive address before exercising cancel.
  const addressed = folders.coordinator({
    storage: new MemoryStorage(),
    api: async (path) => {
      if (path === "/api/v1/sync/status") return status({ peers: [{ peerId, displayName: "Kitchen server", address: "192.0.2.10:8787" }] });
      throw new TypeError("response lost");
    },
  });
  enable(addressed);
  await addressed.refresh();
  await assert.rejects(addressed.refreshPeerAddress(peerId, "192.0.2.20:8787"), TypeError);
  addressed.cancelPeerAddressRefresh(peerId);
  assert.equal(addressed.pendingPeerAddressRefresh(), null);
  assert.throws(() => addressed.retryPeerAddressRefresh(peerId), /folder sync guidance/);
});

test("initial scanning reports local checking before offered or ready phases", () => {
  const decoded = folders.requireStatus(status({
    lifecycle: "initialScanning",
    healthFreshness: "neverObserved",
    shares: [share()],
  }));
  assert.equal(folders.statusSummary(decoded).text, "Checking local folder contents before sync starts.");
  assert.equal(folders.shareView(decoded, decoded.shares[0]).text, "Checking folder");
});

test("the primary Links tab uses server paths, confirmed names, and visible unlocked polling", async () => {
  const root = new URL("../", import.meta.url);
  const [html, app] = await Promise.all([
    readFile(new URL("index.html", root), "utf8"),
    readFile(new URL("app.js", root), "utf8"),
  ]);
  assert.match(html, /data-tab="folders">Links<\/button>/);
  assert.ok(html.indexOf('data-tab="pair"') < html.indexOf('data-tab="folders"'));
  assert.ok(html.indexOf('data-tab="folders"') < html.indexOf('data-tab="settings"'));
  assert.match(html, /<summary>Older backup and restore tools<\/summary>[\s\S]*data-tool-panel="backup"[\s\S]*data-tool-panel="restore"/);
  assert.match(html, /Advanced server folder path/);
  assert.match(html, /name="selectedRoot" value="\/sync"/);
  assert.match(html, /data-paired-devices/);
  assert.match(html, /Paired devices/);
  assert.match(html, /browser folder picker would select files on the computer running the browser/i);
  assert.match(app, /folderApi\("\/api\/v1\/transport\/identity"\)/);
  assert.match(app, /document\.visibilityState === "visible"/);
  assert.match(app, /getAttribute\("aria-selected"\) === "true"/);
  assert.match(app, /setInterval\([\s\S]*?5000\)/);
  assert.match(app, /option\.textContent = peer\.displayName/);
  assert.match(app, /Update address/);
  assert.match(app, /pendingPeerAddressRefresh/);
  assert.match(app, /update\.focus\(\)/);
  assert.match(app, /if \(form\.hidden\) \{\s*input\.value/);
  assert.match(app, /input\.disabled = true;[\s\S]*?finally \{\s*input\.disabled = false;/);
  assert.match(app, /foldersSelected \|\| pairSelected/);
  assert.doesNotMatch(app, /\[data-paired-devices\]"\)\.replaceChildren\([^)]/);
  assert.match(app, /const form = event\.currentTarget;[\s\S]*?await folderController\.sendOffer\(body\);[\s\S]*?form\.reset\(\)/);
  assert.match(app, /folderSync\.lazySessionStorage\(globalThis\)/);
  assert.doesNotMatch(app, /storage: globalThis\.sessionStorage/);
  assert.doesNotMatch(app, /innerHTML\s*=/);
});


test("one-way offers require explicit complete deletion options and preserve them on retry", () => {
  const body = { peerId, folderId, label: "Photos", selectedRoot: "/sync" };
  for (const policy of [undefined, null, {}, { propagateSourceDeletions: false },
    { ...linkPolicy, restoreLocalDeletions: "false" }, { ...linkPolicy, unknown: true }]) {
    assert.throws(() => folders.offerBody({ ...body, linkPolicy: policy }), /folder sync guidance/);
  }
  assert.deepEqual(folders.offerBody({ ...body, linkPolicy }).linkPolicy, linkPolicy);
});

test("idle one-way deleted-file counts do not pretend to be active transfers", () => {
  const health = { folderId, state: "idle", remainingFiles: 1, remainingBytes: 32,
    scanPullErrorCount: 0, reportedErrorRows: 0, statusError: false, watchError: false };
  const candidate = share({
    phase: "ready", linkPolicy, linkSettings: linkState({ confirmed: true }), peerConnection: "connected",
  });
  const current = folders.requireStatus(status({ shares: [candidate], folders: [health] }));
  assert.equal(folders.shareView(current, current.shares[0]).kind, "ready");
  const legacy = folders.requireStatus(status({ shares: [share({ phase: "ready" })], folders: [health] }));
  assert.equal(legacy.shares[0].linkPolicy, null);
  assert.equal(folders.shareView(legacy, legacy.shares[0]).kind, "syncing");
  assert.match(folders.policyExplanation(null), /both directions/);
  assert.match(folders.policyExplanation(linkPolicy), /stay deleted/);
});

test("link settings require complete consistent bounded server state", () => {
  const valid = share({
    incoming: false,
    phase: "ready",
    linkPolicy,
    linkSettings: linkState({ confirmed: true, changedBy: deviceId }),
  });
  assert.equal(folders.requireStatus(status({ shares: [valid] })).shares[0].linkSettings.confirmed, true);

  for (const invalid of [
    { ...valid, linkSettings: null },
    { ...valid, linkPolicy: null },
    { ...valid, linkSettings: { ...valid.linkSettings, confirmed: "true" } },
    { ...valid, linkSettings: { ...valid.linkSettings, revision: 1 } },
    { ...valid, linkSettings: { ...valid.linkSettings,
      settings: { deletionPolicy: { ...linkPolicy, restoreLocalDeletions: true }, paused: false } } },
    { ...valid, linkSettings: { ...valid.linkSettings, extra: true } },
  ]) {
    assert.throws(() => folders.requireStatus(status({ shares: [invalid] })), /folder sync guidance/);
  }
});

test("a lost link-settings reply retries the exact retained revision and change ID", async () => {
  const storage = new MemoryStorage();
  const desired = { deletionPolicy: { ...linkPolicy, restoreLocalDeletions: true }, paused: false };
  const current = share({ incoming: false, phase: "ready", linkPolicy,
    linkSettings: linkState({ confirmed: true, changedBy: deviceId }) });
  const posts = [];
  let loseReply = true;
  const controller = folders.coordinator({
    storage,
    randomUuid: () => changeId,
    api: async (path, options) => {
      if (path === "/api/v1/sync/status") return status({ shares: [current] });
      posts.push(JSON.parse(options.body));
      if (loseReply) {
        loseReply = false;
        throw new TypeError("connection closed after request");
      }
      return { schemaVersion: 1, offerId: null, lifecycle: "running", issue: null };
    },
  });
  enable(controller);
  await controller.refresh();
  await assert.rejects(controller.updateDeletionPolicy(folderId, desired.deletionPolicy),
    (error) => /uncertain/.test(error.covalentGuidance));
  const retained = { folderId, changeId, expectedRevision: 0, settings: desired };
  assert.deepEqual(controller.pendingLinkSettings(), retained);
  await controller.retryPendingLinkSettings();
  assert.deepEqual(posts, [retained, retained]);
  assert.equal(controller.pendingLinkSettings(), null);
});

test("unconfirmed or removed links cannot create a settings retry", async () => {
  for (const candidate of [
    share({ phase: "awaitingCommit", linkPolicy, linkSettings: linkState() }),
    share({ phase: "removed", linkPolicy, linkSettings: linkState({ confirmed: true }) }),
  ]) {
    let posts = 0;
    const controller = folders.coordinator({ storage: new MemoryStorage(), api: async (path) => {
      if (path === "/api/v1/sync/status") return status({ shares: [candidate] });
      posts += 1;
    } });
    enable(controller);
    await controller.refresh();
    assert.throws(() => controller.pause(offerId, true), /folder sync guidance/);
    assert.equal(posts, 0);
    assert.equal(controller.pendingLinkSettings(), null);
  }
});

test("current server state resolves a lost settings reply without another mutation", async () => {
  const storage = new MemoryStorage();
  const desired = { deletionPolicy: { ...linkPolicy, propagateSourceDeletions: true }, paused: false };
  const retained = { folderId, changeId, expectedRevision: 0, settings: desired };
  folders.pendingSettings.save(storage, deviceId, retained);
  const applied = share({ incoming: false, phase: "ready", linkPolicy: desired.deletionPolicy,
    linkSettings: linkState({ revision: 1, settings: desired, changeId, changedBy: deviceId, confirmed: true }) });
  let posts = 0;
  const controller = folders.coordinator({ storage, api: async (path) => {
    if (path === "/api/v1/sync/status") return status({ shares: [applied] });
    posts += 1;
  } });
  enable(controller);
  await controller.refresh();
  assert.equal((await controller.retryPendingLinkSettings()).outcome, "applied");
  assert.equal(posts, 0);
  assert.equal(controller.pendingLinkSettings(), null);
});

test("a stale saved settings request never overwrites a newer revision", async () => {
  const storage = new MemoryStorage();
  const desired = { deletionPolicy: { ...linkPolicy, restoreLocalDeletions: true }, paused: false };
  const retained = { folderId, changeId, expectedRevision: 0, settings: desired };
  folders.pendingSettings.save(storage, deviceId, retained);
  const current = share({ incoming: false, phase: "ready", linkPolicy,
    linkSettings: linkState({ revision: 1, changeId: otherDeviceId, changedBy: peerId, confirmed: true }) });
  let posts = 0;
  const controller = folders.coordinator({ storage, api: async (path) => {
    if (path === "/api/v1/sync/status") return status({ shares: [current] });
    posts += 1;
  } });
  enable(controller);
  await controller.refresh();
  await assert.rejects(controller.retryPendingLinkSettings(),
    (error) => /old revision/.test(error.covalentGuidance));
  assert.equal(posts, 0);
  assert.deepEqual(controller.pendingLinkSettings(), retained);
});

test("destination pending and conflicted requests remain distinct from current settings", async () => {
  const storage = new MemoryStorage();
  const desired = { deletionPolicy: { ...linkPolicy, restoreLocalDeletions: true }, paused: true };
  const request = {
    folderId, sourceId: peerId, requesterId: deviceId, changeId, expectedRevision: 0, settings: desired,
  };
  const pendingShare = share({ phase: "ready", linkPolicy,
    linkSettings: linkState({ pendingChange: request }) });
  const decoded = folders.requireStatus(status({ shares: [pendingShare] })).shares[0];
  assert.equal(decoded.linkSettings.settings.deletionPolicy.restoreLocalDeletions, false);
  assert.equal(decoded.linkSettings.pendingChange.settings.deletionPolicy.restoreLocalDeletions, true);

  const retained = { folderId, changeId, expectedRevision: 0, settings: desired };
  folders.pendingSettings.save(storage, deviceId, retained);
  const conflicted = share({ phase: "ready", linkPolicy,
    linkSettings: linkState({
      revision: 1,
      changeId: otherDeviceId,
      changedBy: peerId,
      confirmed: true,
      conflictedChange: request,
    }) });
  let posts = 0;
  const controller = folders.coordinator({ storage, api: async (path) => {
    if (path === "/api/v1/sync/status") return status({ shares: [conflicted] });
    posts += 1;
  } });
  enable(controller);
  await controller.refresh();
  await assert.rejects(controller.retryPendingLinkSettings(),
    (error) => /old revision/.test(error.covalentGuidance));
  assert.equal(posts, 0);
  assert.equal(controller.pendingLinkSettings(), null);
  assert.deepEqual(controller.current().shares[0].linkSettings.conflictedChange.settings, desired);
});
