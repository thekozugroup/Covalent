import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const preview = require("../restore-preview-flow.js");

test("a delayed obsolete preview is discarded and cannot become current", async () => {
  const coordinator = preview.coordinator();
  const first = coordinator.begin();
  let release;
  const response = new Promise((resolve) => { release = resolve; });
  const discarded = [];
  coordinator.invalidate(); // Models changing the selected backup or target while POST waits.
  release({ planId: "old-plan" });
  const candidate = await response;
  assert.equal(await coordinator.discardIfStale(first, candidate, async (plan) => discarded.push(plan.planId)), true);
  assert.deepEqual(discarded, ["old-plan"]);
  assert.equal(coordinator.isCurrent(first), false);
});

test("only the newest concurrent preview remains publishable", async () => {
  const coordinator = preview.coordinator();
  const first = coordinator.begin();
  const second = coordinator.begin();
  const discarded = [];
  assert.equal(await coordinator.discardIfStale(first, { planId: "first" }, async (plan) => discarded.push(plan.planId)), true);
  assert.equal(await coordinator.discardIfStale(second, { planId: "second" }, async (plan) => discarded.push(plan.planId)), false);
  assert.deepEqual(discarded, ["first"]);
});

test("a delayed page for an old preview cannot replace the current plan", async () => {
  const coordinator = preview.coordinator();
  const oldPlan = { planId: "old" };
  const oldRevision = coordinator.begin();
  let release;
  const page = new Promise((resolve) => { release = resolve; });
  coordinator.begin(); // A newer preview begins while the old page is loading.
  release({ entries: ["old entry"] });
  await page;
  assert.equal(coordinator.isCurrentPlan(oldRevision, oldPlan, oldPlan), false);
  const currentPlan = { planId: "new" };
  const currentRevision = coordinator.begin();
  assert.equal(coordinator.isCurrentPlan(currentRevision, oldPlan, currentPlan), false);
  assert.equal(coordinator.isCurrentPlan(currentRevision, currentPlan, currentPlan), true);
});
