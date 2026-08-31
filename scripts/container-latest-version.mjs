import process from "node:process";

const DIGEST = /^sha256:[0-9a-f]{64}$/;
const STABLE_VERSION = /^v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
const PRERELEASE_VERSION = /^v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)$/;
const LEGACY_VERSION_BY_DIGEST = new Map([
  ["sha256:8b8b96bdea7437fecf6d9c3297c248fd9de7eeb25fe7d701aa6f0a5b633cf8a6", "0.1.0"],
]);

function fail(message) {
  throw new Error(message);
}

function requireDigest(value, label) {
  if (!DIGEST.test(value)) fail(`${label} is not a sha256 OCI digest`);
  return value;
}

function parseStable(value, label) {
  if (typeof value !== "string" || value.length > 128) fail(`${label} is not a bounded stable semantic version`);
  const match = STABLE_VERSION.exec(value);
  if (match === null) fail(`${label} is not a canonical stable semantic version`);
  const canonical = `${match[1]}.${match[2]}.${match[3]}`;
  return {
    canonical,
    tag: `v${canonical}`,
    parts: match.slice(1, 4).map((part) => BigInt(part)),
  };
}

function compare(left, right) {
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] < right[index]) return -1;
    if (left[index] > right[index]) return 1;
  }
  return 0;
}

function evaluate(requestedValue, candidateDigest, currentValue, currentDigest) {
  requireDigest(candidateDigest, "candidate digest");
  if (PRERELEASE_VERSION.test(requestedValue)) {
    return { action: "skip-prerelease", currentTag: null };
  }
  const requested = parseStable(requestedValue, "requested version");

  if (currentValue === "absent" || currentDigest === "absent") {
    if (currentValue !== "absent" || currentDigest !== "absent") {
      fail("current latest must be wholly absent or wholly present");
    }
    return { action: "promote", currentTag: null };
  }

  requireDigest(currentDigest, "current latest digest");
  let resolvedCurrent = currentValue;
  if (currentValue === "unannotated") {
    resolvedCurrent = LEGACY_VERSION_BY_DIGEST.get(currentDigest);
    if (resolvedCurrent === undefined) {
      fail("current latest has no trusted version annotation or legacy digest mapping");
    }
  }
  const current = parseStable(resolvedCurrent, "current latest version");
  const ordering = compare(requested.parts, current.parts);
  if (ordering < 0) {
    fail(`refusing to move latest backward from ${current.tag} to ${requested.tag}`);
  }
  if (ordering === 0) {
    if (currentDigest !== candidateDigest) {
      fail(`latest ${current.tag} already points to a different digest`);
    }
    return { action: "keep", currentTag: current.tag };
  }
  return { action: "promote", currentTag: current.tag };
}

const args = process.argv.slice(2);
if (args.length !== 4) {
  console.error("usage: node scripts/container-latest-version.mjs REQUESTED CANDIDATE_DIGEST CURRENT_VERSION CURRENT_DIGEST");
  process.exit(64);
}

try {
  process.stdout.write(`${JSON.stringify(evaluate(...args))}\n`);
} catch (error) {
  console.error(`container latest guard: ${error.message}`);
  process.exit(1);
}
