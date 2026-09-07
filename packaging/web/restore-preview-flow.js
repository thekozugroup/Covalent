(function exportRestorePreviewFlow(root, factory) {
  const flow = factory();
  if (typeof module === "object" && module.exports) module.exports = flow;
  root.CovalentRestorePreviewFlow = flow;
})(typeof globalThis === "object" ? globalThis : this, function createRestorePreviewFlow() {
  "use strict";

  // Preview creation uses two API requests. A user may change the backup or
  // target while either is pending, so only the newest revision may publish a
  // signed plan into the console.
  function coordinator() {
    let revision = 0;
    return Object.freeze({
      begin() {
        revision += 1;
        return revision;
      },
      invalidate() {
        revision += 1;
        return revision;
      },
      isCurrent(candidate) {
        return Number.isSafeInteger(candidate) && candidate === revision;
      },
      isCurrentPlan(candidateRevision, candidatePlan, currentPlan) {
        return this.isCurrent(candidateRevision) && candidatePlan === currentPlan;
      },
      async discardIfStale(candidateRevision, plan, discard) {
        if (this.isCurrent(candidateRevision)) return false;
        await discard(plan);
        return true;
      },
    });
  }

  return { coordinator };
});
