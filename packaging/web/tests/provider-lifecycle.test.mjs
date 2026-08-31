import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const pairing = require("../pairing-flow.js");
const providers = pairing.providers;

const NOW = 2_000_000_000_000;
const PEER_ID = "11111111-1111-4111-8111-111111111111";
const FINGERPRINT = "a".repeat(64);

function transport(overrides = {}) {
  return {
    peerId: PEER_ID,
    displayName: "Archive server",
    address: "100.100.100.11:8787",
    certificateDer: "c2lnbmVkLWNlcnRpZmljYXRl",
    certificateFingerprint: FINGERPRINT,
    ...overrides,
  };
}

function provider(overrides = {}) {
  return {
    peerId: PEER_ID,
    address: "100.100.100.11:8787",
    certificateFingerprint: FINGERPRINT,
    reachability: "reachable",
    observedAtUnixMs: NOW - 1_000,
    validUntilUnixMs: NOW + 4_000,
    usableBytes: 4_096,
    allocatedBytes: 2_048,
    quotaBytes: 8_192,
    ...overrides,
  };
}

function roster(overrides = {}) {
  return {
    protocolVersion: 1,
    epoch: 1,
    previousDigest: "",
    signerDeviceId: "33333333-3333-4333-8333-333333333333",
    signature: "signed-roster",
    grants: [{
      peerDeviceId: PEER_ID,
      publicKey: "peer-public-key",
      displayName: "Archive server",
      roles: ["storage_provider"],
      confirmedAtUnixMs: 1,
      revoked: false,
    }],
    ...overrides,
  };
}

function recorder(responder) {
  const calls = [];
  const api = async (path, options = {}) => {
    const call = {
      path,
      method: options.method ?? "GET",
      body: options.body === undefined ? null : JSON.parse(options.body),
    };
    calls.push(call);
    return responder(path, call);
  };
  return { api, calls };
}

test("manual finalization activates only its exact signed transport, then verifies the persisted provider", async () => {
  const signedTransport = transport();
  const { api, calls } = recorder((path) => {
    if (path === "/api/v1/providers/connect") return provider();
    if (path === "/api/v1/providers") return [provider()];
    if (path === "/api/v1/rosters/current") return roster();
    throw new Error(`unexpected path ${path}`);
  });

  // `peerTransport` is what finalize returned. No name, address, or pin is
  // supplied separately to activation.
  const activated = await providers.activate(api, { peerTransport: signedTransport });
  assert.deepEqual(calls, [
    {
      path: "/api/v1/providers/connect",
      method: "POST",
      body: { peerTransport: signedTransport },
    },
    { path: "/api/v1/providers", method: "GET", body: null },
    { path: "/api/v1/rosters/current", method: "GET", body: null },
  ]);
  assert.equal(activated.provider.displayName, "Archive server");
  assert.equal(activated.provider.peerId, PEER_ID);
  assert.equal(providers.availability(activated.provider, NOW).eligible, true);

  const selectedProviderIds = providers.selectedIds(activated.providers, [PEER_ID], NOW);
  const backupRequest = { jobId: "backup-signed-provider", selectedProviderIds };
  assert.deepEqual(backupRequest.selectedProviderIds, [PEER_ID]);
});

test("fast pairing loads the node's persisted provider list before it can select a replica", async () => {
  const fast = recorder((path) => {
    if (path.endsWith("/confirm")) {
      return {
        pairingId: "network-pairing-1",
        direction: "outgoing",
        peerName: "Archive server",
        authenticationString: "1234-5678-9012-3456",
        expiresAtUnixMs: NOW + 60_000,
        state: "complete",
      };
    }
    if (path === "/api/v1/providers") return [provider()];
    if (path === "/api/v1/rosters/current") return roster();
    throw new Error(`unexpected path ${path}`);
  });
  const confirmed = await pairing.network.confirm(fast.api, {
    pairingId: "network-pairing-1",
    direction: "outgoing",
    peerName: "Archive server",
    authenticationString: "1234-5678-9012-3456",
    expiresAtUnixMs: NOW + 60_000,
    state: "awaiting_local_confirmation",
  });
  assert.equal(confirmed.state, "complete");
  const listed = await providers.listNamed(fast.api);
  assert.deepEqual(providers.selectedIds(listed, [PEER_ID], NOW), [PEER_ID]);
  assert.equal(fast.calls[1].path, "/api/v1/providers");
  assert.equal(fast.calls[2].path, "/api/v1/rosters/current");
});

test("unreachable, expired, unknown, and zero-capacity providers fail closed", () => {
  const cases = [
    ["unreachable", provider({ reachability: "unreachable", usableBytes: null, allocatedBytes: null, quotaBytes: null, observedAtUnixMs: null, validUntilUnixMs: null })],
    ["expired", provider({ validUntilUnixMs: NOW })],
    ["unknown", provider({ reachability: "unknown", usableBytes: null, allocatedBytes: null, quotaBytes: null, observedAtUnixMs: null, validUntilUnixMs: null })],
    ["zero", provider({ usableBytes: 0 })],
  ];
  for (const [name, value] of cases) {
    assert.equal(providers.availability(value, NOW).eligible, false, name);
    assert.throws(
      () => providers.selectedIds([value], [PEER_ID], NOW),
      /Choose only a reachable backup device with usable space/,
      name,
    );
  }
});

test("a loose, malformed, or mismatched finalized transport never reaches connect", async () => {
  const cases = [
    {},
    { peerTransport: null },
    { peerTransport: transport({ certificateFingerprint: "A".repeat(64) }) },
    { peerTransport: { ...transport(), address: "100.100.100.11:8787", extra: "not signed" } },
  ];
  for (const confirmation of cases) {
    const { api, calls } = recorder(() => provider());
    await assert.rejects(providers.activate(api, confirmation), /valid signed backup-device connection/);
    assert.deepEqual(calls, []);
  }

  const mismatched = recorder((path) => (path === "/api/v1/providers/connect"
    ? provider({ certificateFingerprint: "b".repeat(64) })
    : []));
  await assert.rejects(
    providers.activate(mismatched.api, { peerTransport: transport() }),
    /did not retain the exact signed backup-device connection/,
  );
  assert.equal(mismatched.calls.length, 1, "the mismatched connect response blocks a provider list refresh");
});

test("the web provider contract accepts only runtime reachability states", () => {
  for (const reachability of ["reachable", "unreachable", "unknown"]) {
    assert.equal(providers.require(provider({ reachability })).reachability, reachability);
  }
  assert.throws(
    () => providers.require(provider({ reachability: "stale" })),
    /provider record Covalent cannot verify/,
  );
});
