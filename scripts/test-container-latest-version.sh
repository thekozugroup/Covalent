#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
node --input-type=module - "$root/scripts/container-latest-version.mjs" <<'JS'
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
const script = process.argv[2];
const digest = `sha256:${'a'.repeat(64)}`;
const old = `sha256:${'b'.repeat(64)}`;
const run = (...args) => spawnSync(process.execPath, [script, ...args], { encoding: 'utf8' });
for (const prefix of ['v', 'container-v']) {
  let result = run('0.2.2', digest, '0.2.1', old, prefix);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), { action: 'promote', currentTag: `${prefix}0.2.1` });
  assert.notEqual(run('0.2.1', digest, '0.2.2', old, prefix).status, 0);
  assert.notEqual(run('0.2.2', digest, '0.2.2', old, prefix).status, 0);
}
assert.equal(run('0.2.2', digest, 'absent', 'absent', 'container-v').status, 0);
assert.notEqual(run('0.2.2', digest, 'absent', 'absent', 'arbitrary').status, 0);
let full = run('0.2.2', digest, '0.2.2', old, 'v', 'container-v');
assert.equal(full.status, 0, full.stderr);
assert.deepEqual(JSON.parse(full.stdout), { action: 'promote', currentTag: 'container-v0.2.2' });
assert.notEqual(run('0.2.2', digest, '0.2.2', old, 'container-v', 'v').status, 0);
assert.equal(run('0.2.3', digest, '0.2.2', old, 'v', 'container-v').status, 0);
assert.equal(run('0.2.3', digest, '0.2.2', old, 'container-v', 'v').status, 0);
console.log('Container channel ordering and immutable provenance checks passed.');
JS
