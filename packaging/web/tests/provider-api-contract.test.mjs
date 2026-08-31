import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const repositoryRoot = new URL("../../../", import.meta.url);

test("OpenAPI and the web fixture expose only current provider probe states", async () => {
  const openapi = await readFile(new URL("docs/api/openapi.yaml", repositoryRoot), "utf8");
  const reachability = /reachability:\n\s+description:[^\n]*\n\s+enum: \[([^\]]+)\]/.exec(openapi);
  assert.ok(reachability, "ProviderConnection.reachability schema was not found");
  assert.deepEqual(
    reachability[1].split(",").map((value) => value.trim()),
    ["reachable", "unreachable", "unknown"],
  );
});
