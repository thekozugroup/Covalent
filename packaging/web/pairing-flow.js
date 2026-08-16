(function exposePairingFlow(scope) {
  "use strict";

  function requireSession(session) {
    if (!session || typeof session !== "object" || Array.isArray(session)) {
      throw new Error("Pairing session JSON must be an object.");
    }
    const invitation = session.invitation;
    if (!invitation || typeof invitation !== "object") {
      throw new Error("Pairing session is missing its signed invitation.");
    }
    if (typeof session.authenticationString !== "string" || !session.authenticationString) {
      throw new Error("Pairing session is missing its comparison code.");
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

  async function confirm(api, session, side, displayedCode) {
    if (side !== "responder" && side !== "inviter") {
      throw new Error("Choose whether this device created or accepted the invitation.");
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
      throw new Error("Both devices must add their signed confirmation before finalizing.");
    }
    if (side !== "responder" && side !== "inviter") {
      throw new Error("Choose whether this device created or accepted the invitation.");
    }
    return api(`/api/v1/pair/finalize/${side}`, {
      method: "POST",
      body: JSON.stringify({ session: checked }),
    });
  }

  const pairingFlow = Object.freeze({ confirm, finalize, mutuallyConfirmed, summary });
  scope.CovalentPairingFlow = pairingFlow;
  if (typeof module === "object" && module.exports) module.exports = pairingFlow;
}(typeof globalThis === "object" ? globalThis : window));
