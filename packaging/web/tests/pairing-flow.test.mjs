import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const pairing = require("../pairing-flow.js");

function acceptedSession() {
  return {
    invitation: {
      inviterDeviceId: "11111111-1111-1111-1111-111111111111",
      inviterDeviceName: "Home Mac",
      expiresAtUnixMs: 2_000_000_000_000,
    },
    responderDeviceId: "22222222-2222-2222-2222-222222222222",
    responderName: "Unraid",
    responderRoles: ["storage_provider", "backup_reader"],
    inviterRoles: ["backup_writer"],
    authenticationString: "amber maple river",
    responderConfirmationSignature: null,
    inviterConfirmationSignature: null,
  };
}

test("two browser roles exchange both signed confirmations before finalizing", async () => {
  const calls = [];
  const api = async (path, options) => {
    calls.push({ path, body: JSON.parse(options.body) });
    if (path === "/api/v1/pair/confirm/responder") {
      return { ...calls.at(-1).body.session, responderConfirmationSignature: "responder-signature" };
    }
    if (path === "/api/v1/pair/confirm/inviter") {
      return { ...calls.at(-1).body.session, inviterConfirmationSignature: "inviter-signature" };
    }
    return { finalizedBy: path.endsWith("responder") ? "responder" : "inviter" };
  };

  const accepted = acceptedSession();
  const responderSigned = await pairing.confirm(api, accepted, "responder", accepted.authenticationString);
  assert.equal(pairing.mutuallyConfirmed(responderSigned), false);
  await assert.rejects(
    pairing.finalize(api, responderSigned, "responder"),
    /Both devices must add their signed confirmation/,
  );

  const inviterSigned = await pairing.confirm(
    api,
    JSON.parse(JSON.stringify(responderSigned)),
    "inviter",
    accepted.authenticationString,
  );
  assert.equal(pairing.mutuallyConfirmed(inviterSigned), true);
  await pairing.finalize(api, inviterSigned, "inviter");
  await pairing.finalize(api, inviterSigned, "responder");

  // Assert what the module sent, not what the stub chose to hand back: both
  // finalize requests must carry the same session, with both signatures still
  // on it and nothing else alongside them.
  const finalizeBodies = calls.filter((call) => call.path.includes("/finalize/")).map((call) => call.body);
  assert.equal(finalizeBodies.length, 2);
  for (const body of finalizeBodies) {
    assert.deepEqual(Object.keys(body), ["session"]);
    assert.equal(body.session.responderConfirmationSignature, "responder-signature");
    assert.equal(body.session.inviterConfirmationSignature, "inviter-signature");
    assert.equal(body.session.authenticationString, accepted.authenticationString);
  }

  assert.deepEqual(calls.map((call) => call.path), [
    "/api/v1/pair/confirm/responder",
    "/api/v1/pair/confirm/inviter",
    "/api/v1/pair/finalize/inviter",
    "/api/v1/pair/finalize/responder",
  ]);
});

test("role and identity summary exposes exact consent on both sides", () => {
  const details = pairing.summary(acceptedSession());
  assert.deepEqual(details.inviter, {
    id: "11111111-1111-1111-1111-111111111111",
    name: "Home Mac",
    roles: "backup_writer",
    confirmed: false,
  });
  assert.deepEqual(details.responder, {
    id: "22222222-2222-2222-2222-222222222222",
    name: "Unraid",
    roles: "storage_provider, backup_reader",
    confirmed: false,
  });
});
