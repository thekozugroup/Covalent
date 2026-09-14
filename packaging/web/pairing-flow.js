(function exposePairingFlow(scope) {
  "use strict";

  // Authored copy, marked so the console's error presenter can tell a sentence
  // written in this repository apart from a runtime string it must never show.
  function guidance(text) {
    const error = new Error(text);
    error.covalentGuidance = text;
    return error;
  }

  // ------------------------------------------------------------------ network
  //
  // The node pairs with another node over QUIC on its own: the console only
  // picks a discovered address, shows the short authentication string the node
  // computed, and records that a human compared it. Nothing signed crosses this
  // browser, which is why there is no JSON to copy on this path. The steps and
  // the words match the mobile clients (discover, start, compare, confirm), so
  // the same person recognises the same flow on every surface.

  const NETWORK_PAIRING_ID = /^[A-Za-z0-9._~-]{1,128}$/;
  const AUTHENTICATION_STRING = /^[0-9]{4}(-[0-9]{4}){3}$/;
  const DISCOVERY_SOURCES = Object.freeze({
    lan_mdns: "On this network",
    tailscale: "Over Tailscale",
  });
  const NETWORK_DIRECTIONS = Object.freeze({
    incoming: "Incoming pairing request",
    outgoing: "Outgoing pairing request",
  });
  const NETWORK_STATES = Object.freeze({
    awaiting_local_confirmation: "Compare the code, then confirm it here.",
    awaiting_peer_confirmation: "Confirmed here. Waiting for the other device.",
    complete: "Device paired with its signed certificate fingerprint.",
    failed: "Secure pairing failed. Nothing was trusted.",
  });

  function requireNetworkPairingId(pairingId) {
    if (typeof pairingId !== "string" || !NETWORK_PAIRING_ID.test(pairingId)) {
      throw guidance("This pairing request has an identifier Covalent cannot use.");
    }
    return pairingId;
  }

  function requireNetworkPairing(item) {
    if (!item || typeof item !== "object" || Array.isArray(item)) {
      throw guidance("This node described a pairing request Covalent cannot read.");
    }
    requireNetworkPairingId(item.pairingId);
    if (!Object.hasOwn(NETWORK_DIRECTIONS, item.direction)) {
      throw guidance("This node described a pairing request Covalent cannot read.");
    }
    if (!Object.hasOwn(NETWORK_STATES, item.state)) {
      throw guidance("This node described a pairing request Covalent cannot read.");
    }
    if (typeof item.peerName !== "string" || item.peerName.length === 0 || item.peerName.length > 80) {
      throw guidance("This node described a pairing request Covalent cannot read.");
    }
    if (typeof item.authenticationString !== "string" || !AUTHENTICATION_STRING.test(item.authenticationString)) {
      throw guidance("This pairing request arrived without a code Covalent can ask you to compare.");
    }
    if (!Number.isSafeInteger(item.expiresAtUnixMs) || item.expiresAtUnixMs < 1) {
      throw guidance("This node described a pairing request Covalent cannot read.");
    }
    return item;
  }

  function requireCandidate(candidate) {
    if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) {
      throw guidance("This node described a nearby device Covalent cannot read.");
    }
    if (!Object.hasOwn(DISCOVERY_SOURCES, candidate.source)) {
      throw guidance("This node described a nearby device Covalent cannot read.");
    }
    if (typeof candidate.endpoint !== "string" || candidate.endpoint.length === 0 || candidate.endpoint.length > 512) {
      throw guidance("This node described a nearby device Covalent cannot read.");
    }
    return candidate;
  }

  function requireList(value, copy) {
    if (!Array.isArray(value)) throw guidance(copy);
    return value;
  }

  async function networkCandidates(api) {
    const found = requireList(
      await api("/api/v1/discovery"),
      "This node returned a device list Covalent cannot read.",
    );
    return found.map((candidate) => {
      const checked = requireCandidate(candidate);
      return {
        endpoint: checked.endpoint,
        source: checked.source,
        where: DISCOVERY_SOURCES[checked.source],
      };
    });
  }

  async function networkStart(api, candidateAddress) {
    const address = typeof candidateAddress === "string" ? candidateAddress.trim() : "";
    if (address.length === 0 || address.length > 512) {
      throw guidance("Enter the other device's name or address, such as node-a.tailnet.ts.net:8787.");
    }
    return requireNetworkPairing(await api("/api/v1/pair/network/start", {
      method: "POST",
      body: JSON.stringify({ candidateAddress: address }),
    }));
  }

  async function networkPending(api) {
    const pending = requireList(
      await api("/api/v1/pair/network/pending"),
      "This node returned a pairing list Covalent cannot read.",
    );
    return pending.map(requireNetworkPairing);
  }

  // The code is never typed. The node computed it, this console displayed it,
  // and confirming records that a human saw the same groups on both devices —
  // exactly what the phone and Mac clients send.
  async function networkConfirm(api, item) {
    const checked = requireNetworkPairing(item);
    if (checked.state !== "awaiting_local_confirmation") {
      throw guidance("This pairing request is no longer waiting for your confirmation.");
    }
    const pairingId = encodeURIComponent(checked.pairingId);
    return requireNetworkPairing(await api(`/api/v1/pair/network/${pairingId}/confirm`, {
      method: "POST",
      body: JSON.stringify({ displayedCode: checked.authenticationString }),
    }));
  }

  async function networkCancel(api, pairingId) {
    const encoded = encodeURIComponent(requireNetworkPairingId(pairingId));
    return api(`/api/v1/pair/network/${encoded}`, { method: "DELETE" });
  }

  // A settled pairing still occupies the node's bounded retained-pairing
  // queue. DELETE it when the person dismisses the card. A concurrent peer
  // cleanup is harmless: both “already gone” outcomes mean this console no
  // longer needs to retain the card.
  async function networkDismiss(api, pairingId) {
    try {
      return await networkCancel(api, pairingId);
    } catch (error) {
      if (error && (error.status === 404 || error.status === 410)) return null;
      throw error;
    }
  }

  function networkSummary(item) {
    const checked = requireNetworkPairing(item);
    return {
      pairingId: checked.pairingId,
      peerName: checked.peerName,
      direction: NETWORK_DIRECTIONS[checked.direction],
      code: checked.authenticationString,
      // Screen readers run "1234-5678" together; spacing the groups makes each
      // one its own spoken chunk, matching the Android pairing card.
      spokenCode: checked.authenticationString.replaceAll("-", " "),
      state: checked.state,
      stateCopy: NETWORK_STATES[checked.state],
      awaitingLocalConfirmation: checked.state === "awaiting_local_confirmation",
      settled: checked.state === "complete" || checked.state === "failed",
      // Machine-readable only. The console maps it to copy; the server's own
      // failureMessage is diagnostics and never becomes the headline.
      failureCode: typeof checked.failureCode === "string" ? checked.failureCode : null,
      failureMessage: typeof checked.failureMessage === "string" ? checked.failureMessage : null,
      expiresAtUnixMs: checked.expiresAtUnixMs,
    };
  }

  const network = Object.freeze({
    candidates: networkCandidates,
    start: networkStart,
    pending: networkPending,
    confirm: networkConfirm,
    cancel: networkCancel,
    dismiss: networkDismiss,
    summary: networkSummary,
  });

  const pairingFlow = Object.freeze({ network, guidance });
  scope.CovalentPairingFlow = pairingFlow;
  if (typeof module === "object" && module.exports) module.exports = pairingFlow;
}(typeof globalThis === "object" ? globalThis : window));
