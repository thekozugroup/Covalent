(function exposePairingFlow(scope) {
  "use strict";

  // Authored copy, marked so the console's error presenter can tell a sentence
  // written in this repository apart from a runtime string it must never show.
  function guidance(text) {
    const error = new Error(text);
    error.covalentGuidance = text;
    return error;
  }

  function requireSession(session) {
    if (!session || typeof session !== "object" || Array.isArray(session)) {
      throw guidance("Pairing session JSON must be an object.");
    }
    const invitation = session.invitation;
    if (!invitation || typeof invitation !== "object") {
      throw guidance("Pairing session is missing its signed invitation.");
    }
    if (typeof session.authenticationString !== "string" || !session.authenticationString) {
      throw guidance("Pairing session is missing its comparison code.");
    }
    return session;
  }

  function mutuallyConfirmed(session) {
    const checked = requireSession(session);
    return Boolean(
      checked.responderConfirmationSignature
      && checked.inviterConfirmationSignature,
    );
  }

  function roleList(roles) {
    return Array.isArray(roles) && roles.length > 0
      ? roles.join(", ")
      : "no roles";
  }

  function summary(session) {
    const checked = requireSession(session);
    return {
      code: checked.authenticationString,
      inviter: {
        id: checked.invitation.inviterDeviceId,
        name: checked.invitation.inviterDeviceName,
        roles: roleList(checked.inviterRoles),
        confirmed: Boolean(checked.inviterConfirmationSignature),
      },
      responder: {
        id: checked.responderDeviceId,
        name: checked.responderName,
        roles: roleList(checked.responderRoles),
        confirmed: Boolean(checked.responderConfirmationSignature),
      },
      expiresAtUnixMs: checked.invitation.expiresAtUnixMs,
      mutuallyConfirmed: mutuallyConfirmed(checked),
    };
  }

  // The manual pairing exchange is the only sensitive thing a tab may retain.
  // The node remains the cryptographic authority; this is a strict shape and
  // size gate before the exact signed exchange reaches tab storage.
  const TAB_SESSION_MAX_BYTES = 64 * 1024;
  const SESSION_KEYS = new Set([
    "invitation", "responderDeviceId", "responderPublicKey", "responderName",
    "responderTransport", "responderRoles", "inviterRoles", "authenticationString",
    "responderAcceptanceSignature", "responderConfirmationSignature", "inviterConfirmationSignature",
  ]);
  const INVITATION_KEYS = new Set([
    "protocolVersion", "minimumProtocolVersion", "inviterDeviceId", "inviterPublicKey",
    "inviterDeviceName", "invitationId", "invitationSecret", "invitationSecretCommitment",
    "expiresAtUnixMs", "endpoints", "transportBinding", "signature",
  ]);
  const TRANSPORT_BINDING_KEYS = new Set([
    "peerId", "displayName", "address", "certificateDer", "certificateFingerprint",
  ]);
  const ROLE_NAMES = new Set(["storage_provider", "backup_reader", "backup_writer"]);
  const DEVICE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  const CERTIFICATE_FINGERPRINT = /^[0-9a-f]{64}$/;
  // Provider list probes are fresh on every request. “stale” was never an
  // emitted runtime state; expiry is handled locally below if a response ages
  // past its signed five-second window before it reaches the selection control.
  const PROVIDER_REACHABILITY = new Set(["reachable", "unreachable", "unknown"]);
  const PROVIDER_CONNECTION_KEYS = new Set([
    "peerId", "address", "certificateFingerprint", "reachability",
    "observedAtUnixMs", "validUntilUnixMs", "usableBytes", "allocatedBytes", "quotaBytes",
  ]);
  const PEER_GRANT_KEYS = new Set([
    "peerDeviceId", "publicKey", "displayName", "roles", "confirmedAtUnixMs", "revoked",
  ]);
  const SIGNED_ROSTER_KEYS = new Set([
    "protocolVersion", "epoch", "previousDigest", "grants", "signerDeviceId", "signature",
  ]);
  const PROVIDER_CAPABILITY_FRESHNESS_MS = 5_000;
  const MAX_CONNECTED_PROVIDERS = 128;

  function plainObject(value) {
    return value !== null && typeof value === "object" && !Array.isArray(value)
      && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
  }

  function onlyKnownKeys(value, keys) {
    return Object.keys(value).every((key) => keys.has(key));
  }

  function boundedString(value, maximum = 4096) {
    return typeof value === "string" && value.length > 0 && value.length <= maximum;
  }

  function isTransportBinding(value) {
    return plainObject(value) && onlyKnownKeys(value, TRANSPORT_BINDING_KEYS)
      && [value.peerId, value.displayName, value.address, value.certificateDer, value.certificateFingerprint]
        .every((field) => boundedString(field));
  }

  // `peerTransport` is a finished pairing transcript, not a convenient source
  // for reconstructing an endpoint. Keep this stricter than the generic tab
  // storage shape gate before it reaches providers/connect.
  function isFinalizedProviderTransport(value) {
    return isTransportBinding(value)
      && DEVICE_ID.test(value.peerId)
      && value.displayName.length <= 80
      && !/[\u0000-\u001f\u007f]/.test(value.displayName)
      && value.address.length <= 128
      && CERTIFICATE_FINGERPRINT.test(value.certificateFingerprint)
      && value.certificateDer.length <= 128 * 1_024;
  }

  function requireFinalizedProviderTransport(confirmation) {
    if (!plainObject(confirmation) || !Object.hasOwn(confirmation, "peerTransport")
      || !isFinalizedProviderTransport(confirmation.peerTransport)) {
      throw guidance("This finalized pairing did not include a valid signed backup-device connection.");
    }
    // Return the original signed object. Callers serialize this exact object
    // beneath `peerTransport`; they never rebuild fields from a name, address,
    // or fingerprint.
    return confirmation.peerTransport;
  }

  function nullableCounter(value) {
    return value === null || (Number.isSafeInteger(value) && value >= 0);
  }

  function requireProviderConnection(value) {
    if (!plainObject(value) || !onlyKnownKeys(value, PROVIDER_CONNECTION_KEYS)
      || !DEVICE_ID.test(value.peerId)
      || !boundedString(value.address, 128)
      || !CERTIFICATE_FINGERPRINT.test(value.certificateFingerprint)
      || !PROVIDER_REACHABILITY.has(value.reachability)
      || ![
        value.observedAtUnixMs, value.validUntilUnixMs, value.usableBytes,
        value.allocatedBytes, value.quotaBytes,
      ].every(nullableCounter)) {
      throw guidance("This node returned a provider record Covalent cannot verify.");
    }
    return value;
  }

  function requireNamedProviderConnection(value) {
    if (!plainObject(value) || !Object.hasOwn(value, "displayName")
      || !boundedString(value.displayName, 80)
      || /[\u0000-\u001f\u007f]/.test(value.displayName)) {
      throw guidance("This node returned a provider record Covalent cannot verify.");
    }
    const { displayName: _displayName, ...connection } = value;
    requireProviderConnection(connection);
    return value;
  }

  function requireProviderCandidate(value) {
    return plainObject(value) && Object.hasOwn(value, "displayName")
      ? requireNamedProviderConnection(value)
      : requireProviderConnection(value);
  }

  function providerAvailability(value, now = Date.now()) {
    const provider = requireProviderCandidate(value);
    if (provider.reachability === "unreachable") {
      return Object.freeze({ eligible: false, status: "This device did not answer — cannot select." });
    }
    if (provider.reachability !== "reachable") {
      return Object.freeze({ eligible: false, status: "Capacity is unknown — cannot select." });
    }
    const capacity = [
      provider.observedAtUnixMs, provider.validUntilUnixMs, provider.usableBytes,
      provider.allocatedBytes, provider.quotaBytes,
    ];
    if (!capacity.every(Number.isSafeInteger)
      || provider.validUntilUnixMs - provider.observedAtUnixMs !== PROVIDER_CAPABILITY_FRESHNESS_MS
      || provider.validUntilUnixMs <= now) {
      return Object.freeze({ eligible: false, status: "Capacity check is out of date — cannot select." });
    }
    const usedAndUsable = provider.allocatedBytes + provider.usableBytes;
    if (!Number.isSafeInteger(usedAndUsable) || usedAndUsable > provider.quotaBytes) {
      return Object.freeze({ eligible: false, status: "Capacity could not be verified — cannot select." });
    }
    if (provider.usableBytes === 0 || provider.quotaBytes === 0) {
      return Object.freeze({ eligible: false, status: "No usable space — cannot select." });
    }
    return Object.freeze({
      eligible: true,
      status: `Usable now: ${provider.usableBytes} bytes; ${provider.allocatedBytes} of ${provider.quotaBytes} bytes allocated.`,
    });
  }

  function requireProviderList(value) {
    if (!Array.isArray(value) || value.length > MAX_CONNECTED_PROVIDERS) {
      throw guidance("This node returned a provider list Covalent cannot verify.");
    }
    const providers = value.map(requireProviderCandidate);
    const peerIds = new Set(providers.map((provider) => provider.peerId));
    if (peerIds.size !== providers.length) {
      throw guidance("This node returned duplicate provider identities, so none can be selected.");
    }
    return providers;
  }

  async function listProviders(api) {
    return requireProviderList(await api("/api/v1/providers"));
  }

  function providerNamesFromRoster(value) {
    if (value === null) {
      throw guidance("This node has no signed provider roster, so connected devices cannot be selected.");
    }
    if (!plainObject(value) || !onlyKnownKeys(value, SIGNED_ROSTER_KEYS)
      || value.protocolVersion !== 1
      || !Number.isSafeInteger(value.epoch) || value.epoch < 1
      || typeof value.previousDigest !== "string" || value.previousDigest.length > 64
      || !DEVICE_ID.test(value.signerDeviceId)
      || !boundedString(value.signature, 4_096)
      || !Array.isArray(value.grants) || value.grants.length > MAX_CONNECTED_PROVIDERS) {
      throw guidance("This node returned a signed provider roster Covalent cannot verify.");
    }
    const names = new Map();
    for (const grant of value.grants) {
      if (!plainObject(grant) || !onlyKnownKeys(grant, PEER_GRANT_KEYS)
        || !DEVICE_ID.test(grant.peerDeviceId)
        || !boundedString(grant.publicKey, 4_096)
        || !boundedString(grant.displayName, 80)
        || /[\u0000-\u001f\u007f]/.test(grant.displayName)
        || !Array.isArray(grant.roles)
        || !grant.roles.every((role) => ROLE_NAMES.has(role))
        || new Set(grant.roles).size !== grant.roles.length
        || !Number.isSafeInteger(grant.confirmedAtUnixMs)
        || grant.confirmedAtUnixMs < 0
        || typeof grant.revoked !== "boolean") {
        throw guidance("This node returned a signed provider roster Covalent cannot verify.");
      }
      if (names.has(grant.peerDeviceId)) {
        throw guidance("This node returned duplicate provider identities, so none can be selected.");
      }
      if (!grant.revoked && grant.roles.includes("storage_provider")) {
        names.set(grant.peerDeviceId, grant.displayName);
      }
    }
    return names;
  }

  async function listNamedProviders(api) {
    const [listed, roster] = await Promise.all([
      listProviders(api),
      api("/api/v1/rosters/current"),
    ]);
    const names = providerNamesFromRoster(roster);
    return listed.map((provider) => {
      const displayName = names.get(provider.peerId);
      if (displayName === undefined) {
        throw guidance("A connected device was not present in the signed provider roster, so it cannot be selected.");
      }
      return Object.freeze({ ...provider, displayName });
    });
  }

  function providerForSignedTransport(providers, transport) {
    const matching = requireProviderList(providers).filter((provider) => (
      provider.peerId === transport.peerId
      && provider.certificateFingerprint === transport.certificateFingerprint
    ));
    if (matching.length !== 1) {
      throw guidance("The node did not retain the exact signed backup-device connection, so it cannot be selected.");
    }
    return matching[0];
  }

  async function activateProvider(api, confirmation) {
    const transport = requireFinalizedProviderTransport(confirmation);
    const connected = requireProviderConnection(await api("/api/v1/providers/connect", {
      method: "POST",
      // This is deliberately the finalized object itself. Do not make a new
      // object by copying public fields out of it: the node compares the exact
      // transcript-bound binding to its signed pairing record.
      body: JSON.stringify({ peerTransport: transport }),
    }));
    providerForSignedTransport([connected], transport);
    const providers = await listNamedProviders(api);
    const provider = providerForSignedTransport(providers, transport);
    if (provider.displayName !== transport.displayName) {
      throw guidance("The node did not retain the exact signed backup-device name, so it cannot be selected.");
    }
    return Object.freeze({ provider, providers });
  }

  function selectedProviderIds(providers, selectedIds, now = Date.now()) {
    const verified = requireProviderList(providers);
    if (!Array.isArray(selectedIds) || selectedIds.length > MAX_CONNECTED_PROVIDERS) {
      throw guidance("Choose backup devices from the verified provider list.");
    }
    const unique = new Set();
    for (const peerId of selectedIds) {
      if (typeof peerId !== "string" || !DEVICE_ID.test(peerId) || unique.has(peerId)) {
        throw guidance("Choose each backup device once from the verified provider list.");
      }
      const provider = verified.find((candidate) => candidate.peerId === peerId);
      if (!provider || !providerAvailability(provider, now).eligible) {
        throw guidance("Choose only a reachable backup device with usable space.");
      }
      unique.add(peerId);
    }
    return [...unique];
  }

  function hasSafeSessionShape(session, now = Date.now()) {
    if (!plainObject(session) || !onlyKnownKeys(session, SESSION_KEYS)) return false;
    const invitation = session.invitation;
    if (!plainObject(invitation) || !onlyKnownKeys(invitation, INVITATION_KEYS)) return false;
    if (!Number.isSafeInteger(invitation.expiresAtUnixMs) || invitation.expiresAtUnixMs <= now) return false;
    const required = [
      invitation.inviterDeviceId, invitation.inviterPublicKey, invitation.inviterDeviceName,
      invitation.invitationId, invitation.invitationSecret, invitation.invitationSecretCommitment,
      invitation.signature, session.responderDeviceId, session.responderPublicKey,
      session.responderName, session.authenticationString, session.responderAcceptanceSignature,
    ];
    if (!required.every((value) => boundedString(value))) return false;
    if (!Array.isArray(invitation.endpoints) || invitation.endpoints.length > 16
      || !invitation.endpoints.every((endpoint) => boundedString(endpoint, 512))) return false;
    if (!Number.isSafeInteger(invitation.protocolVersion)
      || !Number.isSafeInteger(invitation.minimumProtocolVersion)) return false;
    for (const roles of [session.responderRoles, session.inviterRoles]) {
      if (!Array.isArray(roles) || roles.length > ROLE_NAMES.size
        || !roles.every((role) => typeof role === "string" && ROLE_NAMES.has(role))) return false;
    }
    for (const signature of [session.responderConfirmationSignature, session.inviterConfirmationSignature]) {
      if (signature !== null && signature !== undefined && !boundedString(signature)) return false;
    }
    // A binding holds only signed transport identity, never an arbitrary
    // nested object that could smuggle in a credential.
    if (session.responderTransport !== undefined && !isTransportBinding(session.responderTransport)) return false;
    if (invitation.transportBinding !== undefined && invitation.transportBinding !== null
      && !isTransportBinding(invitation.transportBinding)) return false;
    try { return new TextEncoder().encode(JSON.stringify(session)).length <= TAB_SESSION_MAX_BYTES; }
    catch (_) { return false; }
  }

  function isPersistableSession(session, now = Date.now()) {
    // A pending invitation carries material needed only to compare and sign.
    // Keep that in memory; tab storage may hold an exchange only after both
    // devices have confirmed the exact signed transcript.
    return hasSafeSessionShape(session, now)
      && boundedString(session.responderConfirmationSignature)
      && boundedString(session.inviterConfirmationSignature);
  }

  function saveTabSession(storage, key, session, now = Date.now()) {
    if (!isPersistableSession(session, now)) {
      try { storage.removeItem(key); } catch (_) {}
      return false;
    }
    try {
      storage.setItem(key, JSON.stringify(session));
      return true;
    } catch (_) { return false; }
  }

  function loadTabSession(storage, key, now = Date.now()) {
    try {
      const encoded = storage.getItem(key);
      if (!encoded || encoded.length > TAB_SESSION_MAX_BYTES) {
        if (encoded) storage.removeItem(key);
        return null;
      }
      const session = JSON.parse(encoded);
      if (!isPersistableSession(session, now)) {
        storage.removeItem(key);
        return null;
      }
      return session;
    } catch (_) {
      try { storage.removeItem(key); } catch (_) {}
      return null;
    }
  }

  function clearTabSession(storage, key) {
    try { storage.removeItem(key); } catch (_) {}
  }

  async function confirm(api, session, side, displayedCode) {
    if (side !== "responder" && side !== "inviter") {
      throw guidance("Choose whether this device created or accepted the invitation.");
    }
    const checked = requireSession(session);
    return api(`/api/v1/pair/confirm/${side}`, {
      method: "POST",
      body: JSON.stringify({ session: checked, displayedCode }),
    });
  }

  async function finalize(api, session, side) {
    const checked = requireSession(session);
    if (!mutuallyConfirmed(checked)) {
      throw guidance("Both devices must add their signed confirmation before finalizing.");
    }
    if (side !== "responder" && side !== "inviter") {
      throw guidance("Choose whether this device created or accepted the invitation.");
    }
    return api(`/api/v1/pair/finalize/${side}`, {
      method: "POST",
      body: JSON.stringify({ session: checked }),
    });
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
    incoming: "Incoming backup-device request",
    outgoing: "Outgoing backup-device request",
  });
  const NETWORK_STATES = Object.freeze({
    awaiting_local_confirmation: "Compare the code, then confirm it here.",
    awaiting_peer_confirmation: "Confirmed here. Waiting for the other device.",
    complete: "Backup device added, with its signed certificate fingerprint.",
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

  const storage = Object.freeze({ isPersistableSession, saveTabSession, loadTabSession, clearTabSession });
  const providers = Object.freeze({
    activate: activateProvider,
    availability: providerAvailability,
    finalizedTransport: requireFinalizedProviderTransport,
    list: listProviders,
    listNamed: listNamedProviders,
    require: requireProviderConnection,
    requireNamed: requireNamedProviderConnection,
    requireList: requireProviderList,
    selectedIds: selectedProviderIds,
  });
  const pairingFlow = Object.freeze({ confirm, finalize, mutuallyConfirmed, summary, network, providers, storage, guidance });
  scope.CovalentPairingFlow = pairingFlow;
  if (typeof module === "object" && module.exports) module.exports = pairingFlow;
}(typeof globalThis === "object" ? globalThis : window));
