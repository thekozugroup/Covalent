import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import test from "node:test";

const require = createRequire(import.meta.url);
// app.js exports its error presenter and boots the DOM console only when a
// `document` exists, so requiring it here yields the presenter alone.
const copy = require("../app.js");

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const consoleSource = readFileSync(resolve(repositoryRoot, "packaging/web/app.js"), "utf8");

// The node builds every error code with one of these constructors, called as
// `ApiError::name(...)` or `Self::name(...)` from inside its own impl, or by
// naming `code:` directly in a struct literal.
const CONSTRUCTORS = [
  "bad_request",
  "payload_too_large",
  "conflict",
  "not_found",
  "unprocessable",
  "insufficient_storage",
  "too_many_requests",
  "upload_offset",
  "pairing_peer_unreachable",
  "gone",
  "forbidden",
  "unauthorized",
  "internal",
].join("|");

function engineErrorCodes() {
  const sources = [
    "crates/covalent-node/src/lib.rs",
    "crates/covalent-node/src/pairing_transport.rs",
    "crates/covalent-node/src/network_pairing.rs",
    "crates/covalent-node/src/transport.rs",
  ];
  const codes = new Set();
  for (const relative of sources) {
    // Fail closed: a moved source means this test is stale, not satisfied.
    const text = readFileSync(resolve(repositoryRoot, relative), "utf8");
    const constructed = new RegExp(`(?:ApiError|Self)::(?:${CONSTRUCTORS})\\s*\\(\\s*"([a-z0-9_]+)"`, "g");
    for (const match of text.matchAll(constructed)) codes.add(match[1]);
    for (const match of text.matchAll(/code:\s*"([a-z0-9_]+)"/g)) codes.add(match[1]);
  }
  codes.delete("ok");
  if (codes.size < 50) {
    throw new Error(`only ${codes.size} engine error codes were extracted; this test's extractor is stale`);
  }
  return [...codes].sort();
}

test("every error code the node can emit has console copy with a recovery action", () => {
  const missing = engineErrorCodes().filter((code) => !Object.hasOwn(copy.CATALOG, code));
  assert.deepEqual(
    missing,
    [],
    `these engine error codes have no console copy; add them to CATALOG in packaging/web/app.js: ${missing.join(", ")}`,
  );

  const recoveries = new Set(Object.values(copy.RECOVERY));
  for (const [code, entry] of Object.entries(copy.CATALOG)) {
    const [summary, recovery] = entry;
    assert.ok(summary.length > 20, `${code} has no real sentence`);
    assert.ok(/[.!]$/.test(summary), `${code} copy is not a sentence: ${summary}`);
    assert.ok(recoveries.has(recovery), `${code} names an unknown recovery action: ${recovery}`);
    assert.ok(!/[_{}]|snake_case|HTTP \d/.test(summary), `${code} copy leaks machine text: ${summary}`);
  }
});

test("mapped engine codes produce their intended sentence and next step", () => {
  const cases = [
    ["authentication_required", 401, "This console is no longer unlocked. Enter the local access token again to continue.", "reconnect"],
    ["insufficient_storage", 507, "Your backup server is out of space. Free some up, or choose a different device to keep this copy.", "freeUpSpace"],
    ["source_unreadable", 422, "Covalent couldn't read part of the folder you chose. Choose the folder again.", "chooseFolderAgain"],
    ["restore_plan_mismatch", 409, "This restore changed after you previewed it. Preview it again before restoring.", "previewRestoreAgain"],
    ["pairing_rejected", 409, "That device turned down the pairing request. Nothing was trusted.", "chooseAnotherDevice"],
    ["node_busy", 503, "Your backup server is busy with something else. Try again in a moment.", "retry"],
    ["backup_corrupt", 422, "Some of this backup's encrypted data is damaged. Verify the backup to see what can still be restored.", "none"],
  ];
  for (const [code, status, summary, recovery] of cases) {
    const failure = copy.describeApi(status, code, "engine text nobody should read", false);
    assert.equal(failure.summary, summary, code);
    assert.equal(failure.recovery, recovery, code);
    // The server's own words survive, but only as diagnostics.
    assert.equal(failure.detail, `HTTP ${status} · ${code} · engine text nobody should read`);
  }
});

test("first-run setup and endpoint codes read as instructions, never as diagnostics", () => {
  // These are the codes a person meets before they have any Covalent
  // vocabulary at all: the very first screen, on a server they cannot log
  // into. Each one has to say what to do next without naming a token, a
  // certificate file, a bearer header, or a container path.
  const cases = [
    ["peer_endpoint_unavailable", 409, "This server doesn't know which address other devices should dial yet. Set the address other devices dial in its settings, then try again.", "none"],
    ["claim_unavailable", 409, "This server already has an owner, so it can't be set up again.", "none"],
    ["claim_code_incorrect", 401, "That setup code isn't correct. Check the code shown in your server's log and try again.", "none"],
    ["claim_window_expired", 410, "That setup code has expired. Restart Covalent on your server to get a new one.", "none"],
    ["claim_window_exhausted", 410, "Too many incorrect setup codes were entered. Restart Covalent on your server to get a new code.", "none"],
    ["claim_rate_limited", 429, "Setup codes are being entered too quickly. Wait a moment, then try again.", "retry"],
    ["claim_certificate_unavailable", 503, "This server is still preparing its security certificate. Wait a few seconds, then try again.", "retry"],
    ["claim_state_unavailable", 503, "This server couldn't finish setting up safely. Retry the exact saved claim request; restart only if that remains unavailable.", "retry"],
  ];
  for (const [code, status, summary, recovery] of cases) {
    const failure = copy.describeApi(status, code, "engine text nobody should read", false);
    assert.equal(failure.summary, summary, code);
    assert.equal(failure.recovery, recovery, code);
    assert.equal(failure.detail, `HTTP ${status} \u00b7 ${code} \u00b7 engine text nobody should read`);
    // The specific regression this guards: `claim_code_incorrect` arrives as a
    // 401, and the unrecognised-401 fallback tells the reader to unlock the
    // console with a local access token -- advice that is exactly backwards for
    // someone who has no token and is trying to obtain one.
    assert.ok(!failure.summary.includes("local access token"), code);
    assert.ok(!failure.summary.includes("engine text"), code);
  }
});

test("an unrecognised code still falls back on its status, never on server text", () => {
  const cases = [
    [401, "Your backup server refused this request. Unlock the console again with a current local access token.", "reconnect"],
    [403, "Your backup server refused this request. Unlock the console again with a current local access token.", "reconnect"],
    [404, "Covalent couldn't find that on your backup server.", "none"],
    [409, "Something else changed on your backup server first. Reload the page, then try again.", "retry"],
    [413, "That's larger than your backup server accepts in one go. Try a smaller folder.", "none"],
    [422, "Your backup server wouldn't accept what this console sent. Check the values in the form, then try again.", "none"],
    [500, "Something went wrong on your backup server. Try again; if it keeps happening, check its logs.", "retry"],
    [503, "Your backup server isn't ready to answer yet. Wait for it to finish starting, then try again.", "retry"],
    [507, "Your backup server is out of space.", "freeUpSpace"],
  ];
  for (const [status, summary, recovery] of cases) {
    const failure = copy.describeApi(status, "a_code_from_a_newer_node", "/srv/covalent/keys/identity.key is unreadable", false);
    assert.equal(failure.summary, summary, `status ${status}`);
    assert.equal(failure.recovery, recovery, `status ${status}`);
    assert.ok(!failure.summary.includes("identity.key"), `status ${status} leaked server text into the headline`);
  }
  assert.equal(
    copy.describeApi(418, "a_code_from_a_newer_node", "", true).summary,
    "Your backup server couldn't complete that request. Try again in a moment.",
  );
  assert.equal(
    copy.describeApi(418, "a_code_from_a_newer_node", "", false).summary,
    "Your backup server couldn't complete that request.",
  );
});

test("transport failures are classified rather than shown raw", () => {
  const offline = copy.describe(new TypeError("Failed to fetch"), { online: false });
  assert.equal(offline.summary, "This computer isn't connected to a network, so Covalent can't reach your backup server.");
  assert.equal(offline.recovery, "checkNetworkSettings");

  // The browser reports a stopped node and an untrusted certificate as the same
  // opaque TypeError, so one honest sentence has to cover both.
  const unreachable = copy.describe(new TypeError("Failed to fetch"), { online: true });
  assert.match(unreachable.summary, /couldn't reach your backup server/);
  assert.match(unreachable.summary, /certificate authority/);
  assert.equal(unreachable.recovery, "reconnect");
  assert.equal(copy.describe(new TypeError("Load failed"), { online: true }).summary, unreachable.summary);
  assert.equal(
    copy.describe(new TypeError("NetworkError when attempting to fetch resource."), { online: true }).summary,
    unreachable.summary,
  );

  const abort = new Error("aborted");
  abort.name = "AbortError";
  assert.equal(
    copy.describe(abort).summary,
    "Your backup server didn't answer in time. Make sure it's turned on and awake, then try again.",
  );
  assert.equal(copy.describe(abort).recovery, "retry");

  const blocked = new Error("The operation is insecure.");
  blocked.name = "SecurityError";
  assert.match(copy.describe(blocked).summary, /blocked the request/);

  assert.match(copy.describe(new SyntaxError("Unexpected token < in JSON at position 0")).summary, /couldn't read that as JSON/);

  assert.equal(copy.describe(new RangeError("boom")).summary, "Covalent couldn't finish that. Try again in a moment.");
});

test("a protocol mismatch is copy, not a version dump", () => {
  const failure = copy.describeProtocol(9);
  assert.equal(
    failure.summary,
    "This console and your backup server don't agree on how to talk to each other. Update both to the same version.",
  );
  assert.equal(failure.detail, "node protocol 9 · this console speaks 1");
});

test("no value can make the presenter emit a string it did not author", () => {
  const secret = "Bearer 5f3c9a-local-api-token /data/local-api-token";
  const adversarial = [
    new TypeError(secret),
    new Error(secret),
    Object.assign(new Error(secret), { name: "AbortError" }),
    Object.assign(new Error(secret), { name: "SecurityError" }),
    Object.assign(new Error(secret), { name: "SyntaxError" }),
    Object.assign(new Error("x"), { covalentFailureKind: "api", status: 500, code: "internal_error", serverMessage: secret }),
    Object.assign(new Error("x"), { covalentFailureKind: "api", status: 599, code: secret, serverMessage: secret }),
    Object.assign(new Error("x"), { covalentFailureKind: "protocol", observedVersion: secret }),
    // Values that are not errors at all, which is what a rejected promise can be.
    secret,
    { message: secret },
    { covalentGuidance: 42 },
    null,
    undefined,
    Symbol("x"),
  ];
  const allowed = new Set(copy.SUMMARIES);
  for (const value of adversarial) {
    const failure = copy.describe(value, { online: true });
    assert.ok(
      allowed.has(failure.summary),
      `describe() produced an unauthored headline for ${String(value?.name ?? typeof value)}: ${failure.summary}`,
    );
    assert.ok(!failure.summary.includes("local-api-token"), "a secret reached the headline");
  }
  // The technical text is demoted, not discarded.
  assert.ok(copy.describe(new TypeError(secret), { online: true }).detail.includes("local-api-token"));
});

test("copy authored in this repository survives the presenter intact", () => {
  const pairing = require("../pairing-flow.js");
  const authored = pairing.guidance("Both devices must add their signed confirmation before finalizing.");
  const failure = copy.describe(authored);
  assert.equal(failure.summary, "Both devices must add their signed confirmation before finalizing.");
  assert.equal(failure.detail, null);

  // An error that merely *claims* a sentence through its message cannot borrow
  // this path; only the marker property set by our own modules can.
  assert.notEqual(copy.describe(new Error("Both devices must add their signed confirmation.")).summary, "Both devices must add their signed confirmation.");
});

test("the console has exactly one error renderer and it never reads a raw message", () => {
  const body = /function fail\(error\) \{([\s\S]*?)\n\}/.exec(consoleSource);
  assert.ok(body !== null, "fail(error) is no longer the console's error renderer");
  assert.match(body[1], /errorCopy\.describe\(error\)/);
  assert.ok(!body[1].includes("error.message"), "fail() reads a raw error message");

  // A raw `.message` may only be read where it is turned into diagnostics.
  const presenterEnd = consoleSource.indexOf("function bootConsole() {");
  assert.ok(presenterEnd > 0, "app.js no longer separates its presenter from the DOM console");
  const presenter = consoleSource.slice(0, presenterEnd);
  const consoleHalf = consoleSource.slice(presenterEnd);
  assert.ok(!consoleHalf.includes("error.message"), "the DOM console reads a raw error message");

  const rawDetail = /function rawDetail\(error\) \{([\s\S]*?)\n  \}/.exec(presenter);
  assert.ok(rawDetail !== null, "rawDetail() is no longer where raw text is collected");
  const readsOutsideDiagnostics = presenter.split("error.message").length - 1
    - (rawDetail[1].split("error.message").length - 1);
  assert.equal(readsOutsideDiagnostics, 0, "the presenter reads a raw error message outside rawDetail()");

  assert.ok(
    !/\bsay\([^;]*\berror\b/.test(consoleSource),
    "app.js hands an error straight to say(); route it through fail() instead",
  );

  // Sentences the flow modules throw are marked, so the presenter can keep them
  // instead of collapsing them into the generic fallback.
  for (const relative of ["packaging/web/pairing-flow.js", "packaging/web/restore-plan-flow.js"]) {
    const source = readFileSync(resolve(repositoryRoot, relative), "utf8");
    assert.ok(!source.includes("throw new Error("), `${relative} throws an unmarked Error; use guidance() so its copy survives`);
  }
});
