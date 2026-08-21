import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const pairing = require("../pairing-flow.js");
const network = pairing.network;

function pendingItem(overrides = {}) {
  return {
    pairingId: "pair-42",
    direction: "outgoing",
    peerName: "Home Mac",
    authenticationString: "1234-5678-9012-3456",
    expiresAtUnixMs: 2_000_000_000_000,
    state: "awaiting_local_confirmation",
    ...overrides,
  };
}

// Records what the console actually sent, so every assertion below is about the
// module's own behaviour rather than about what a stub chose to hand back.
function recorder(responder) {
  const calls = [];
  const api = async (path, options = {}) => {
    calls.push({ path, method: options.method ?? "GET", body: options.body === undefined ? null : JSON.parse(options.body) });
    return responder(path, calls.at(-1));
  };
  return { api, calls };
}

test("discovered devices are named in words, not in transport identifiers", async () => {
  const { api, calls } = recorder(() => [
    { source: "lan_mdns", endpoint: "10.0.0.4:8787", serviceId: "a", minimumProtocolVersion: 1, maximumProtocolVersion: 1 },
    { source: "tailscale", endpoint: "node-a.tailnet.ts.net:8787", serviceId: "b", minimumProtocolVersion: 1, maximumProtocolVersion: 1 },
  ]);
  const found = await network.candidates(api);
  assert.deepEqual(calls, [{ path: "/api/v1/discovery", method: "GET", body: null }]);
  assert.deepEqual(found, [
    { endpoint: "10.0.0.4:8787", source: "lan_mdns", where: "On this network" },
    { endpoint: "node-a.tailnet.ts.net:8787", source: "tailscale", where: "Over Tailscale" },
  ]);
});

test("starting pairing sends only the chosen address and returns a checked request", async () => {
  const { api, calls } = recorder(() => pendingItem());
  const started = await network.start(api, "  node-a.tailnet.ts.net:8787  ");
  assert.deepEqual(calls, [{
    path: "/api/v1/pair/network/start",
    method: "POST",
    body: { candidateAddress: "node-a.tailnet.ts.net:8787" },
  }]);
  assert.equal(started.pairingId, "pair-42");

  const empty = recorder(() => pendingItem());
  await assert.rejects(network.start(empty.api, "   "), /Enter the other device's name or address/);
  assert.deepEqual(empty.calls, [], "an empty address must not reach the node");
});

test("confirming sends back the code this console displayed, never typed input", async () => {
  const { api, calls } = recorder(() => pendingItem({ state: "awaiting_peer_confirmation" }));
  const item = pendingItem();
  const confirmed = await network.confirm(api, item);
  assert.deepEqual(calls, [{
    path: "/api/v1/pair/network/pair-42/confirm",
    method: "POST",
    // The node computed this string and this console showed it; a human only
    // compared it. Nothing the person types can change what is sent.
    body: { displayedCode: "1234-5678-9012-3456" },
  }]);
  assert.equal(confirmed.state, "awaiting_peer_confirmation");
});

test("a request that is no longer awaiting this device is not re-confirmed", async () => {
  for (const state of ["awaiting_peer_confirmation", "complete", "failed"]) {
    const { api, calls } = recorder(() => pendingItem());
    await assert.rejects(
      network.confirm(api, pendingItem({ state })),
      /no longer waiting for your confirmation/,
      state,
    );
    assert.deepEqual(calls, [], `${state} must not reach the node`);
  }
});

test("cancelling deletes exactly the named request", async () => {
  const { api, calls } = recorder(() => null);
  await network.cancel(api, "pair-42");
  assert.deepEqual(calls, [{ path: "/api/v1/pair/network/pair-42", method: "DELETE", body: null }]);
});

test("a pairing identifier can never widen the path it is spliced into", async () => {
  const hostile = ["../../status", "pair/42", "pair 42", "", "x".repeat(129), "a?b", null, 42];
  for (const pairingId of hostile) {
    const { api, calls } = recorder(() => null);
    await assert.rejects(network.cancel(api, pairingId), /identifier Covalent cannot use/, String(pairingId));
    assert.deepEqual(calls, [], `${String(pairingId)} must not reach the node`);
  }
});

test("a request the node describes badly is refused rather than displayed", async () => {
  const malformed = [
    pendingItem({ authenticationString: "1234-5678" }),
    pendingItem({ authenticationString: "abcd-efgh-ijkl-mnop" }),
    pendingItem({ state: "awaiting_everything" }),
    pendingItem({ direction: "sideways" }),
    pendingItem({ peerName: "" }),
    pendingItem({ peerName: "n".repeat(81) }),
    pendingItem({ expiresAtUnixMs: 0 }),
    pendingItem({ pairingId: "with/slash" }),
    null,
    [pendingItem()],
  ];
  // Every refusal must be authored copy, marked so the console's presenter
  // shows the sentence rather than collapsing it to the generic fallback.
  const isGuidance = (error) => typeof error.covalentGuidance === "string" && error.covalentGuidance.length > 20;
  for (const item of malformed) {
    const { api } = recorder(() => item);
    await assert.rejects(network.start(api, "10.0.0.4:8787"), isGuidance, JSON.stringify(item));
    assert.throws(() => network.summary(item), isGuidance, JSON.stringify(item));
  }

  const notAList = recorder(() => ({ pairingId: "pair-42" }));
  await assert.rejects(network.pending(notAList.api), /pairing list Covalent cannot read/);
});

test("pending requests are listed and each one is checked", async () => {
  const { api, calls } = recorder(() => [
    pendingItem(),
    pendingItem({ pairingId: "pair-43", direction: "incoming", state: "awaiting_peer_confirmation" }),
  ]);
  const pending = await network.pending(api);
  assert.deepEqual(calls, [{ path: "/api/v1/pair/network/pending", method: "GET", body: null }]);
  assert.deepEqual(pending.map((item) => item.pairingId), ["pair-42", "pair-43"]);
});

test("the pairing summary is display copy with the code spoken group by group", () => {
  const details = network.summary(pendingItem({ direction: "incoming" }));
  assert.equal(details.direction, "Incoming backup-device request");
  assert.equal(details.stateCopy, "Compare the code, then confirm it here.");
  assert.equal(details.awaitingLocalConfirmation, true);
  assert.equal(details.settled, false);
  assert.equal(details.code, "1234-5678-9012-3456");
  // Screen readers run the hyphenated form together; the spoken form keeps the
  // four groups apart, matching the Android pairing card.
  assert.equal(details.spokenCode, "1234 5678 9012 3456");

  const waiting = network.summary(pendingItem({ state: "awaiting_peer_confirmation" }));
  assert.equal(waiting.stateCopy, "Confirmed here. Waiting for the other device.");
  assert.equal(waiting.awaitingLocalConfirmation, false);
  assert.equal(waiting.settled, false);

  const done = network.summary(pendingItem({ state: "complete" }));
  assert.equal(done.stateCopy, "Backup device added, with its signed certificate fingerprint.");
  assert.equal(done.settled, true);

  // A failure keeps the engine's code for the console to map, and holds the
  // server's own sentence back as diagnostics.
  const failed = network.summary(pendingItem({
    state: "failed",
    failureCode: "pairing_endpoint_mismatch",
    failureMessage: "peer certificate 3f:aa did not match the signed binding",
  }));
  assert.equal(failed.stateCopy, "Secure pairing failed. Nothing was trusted.");
  assert.equal(failed.failureCode, "pairing_endpoint_mismatch");
  assert.ok(!failed.stateCopy.includes("3f:aa"));

  const copy = require("../app.js");
  assert.equal(
    copy.CATALOG[failed.failureCode][0],
    "That device answered from a different address than the one you paired with. Pair with it again.",
  );
});

test("the network flow never asks anyone to copy JSON", async () => {
  const { api } = recorder((path) => (path.endsWith("/pending") ? [] : pendingItem()));
  const started = await network.start(api, "10.0.0.4:8787");
  const details = network.summary(started);
  // Everything the console shows for this flow is a name, a code and a state.
  assert.deepEqual(Object.keys(details).sort(), [
    "awaitingLocalConfirmation",
    "code",
    "direction",
    "expiresAtUnixMs",
    "failureCode",
    "failureMessage",
    "pairingId",
    "peerName",
    "settled",
    "spokenCode",
    "state",
    "stateCopy",
  ]);
  assert.ok(!("invitation" in started), "a network pairing carries no invitation to copy");
  assert.ok(!("session" in started), "a network pairing carries no session to copy");
});
