import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

async function source(name) {
  return readFile(new URL(name, root), "utf8");
}

test("backup submission exposes progress, prevents duplicate posts, and retries the same job", async () => {
  const [html, app] = await Promise.all([source("index.html"), source("app.js")]);
  assert.match(html, /data-backup-status role="status" aria-live="polite"/);
  assert.match(html, /data-backup-retry hidden>Try again</);
  assert.match(app, /form\.setAttribute\("aria-busy", String\(inFlight\)\)/);
  assert.match(app, /if \(backupSubmissionInFlight\) return;/);
  assert.match(app, /jobId: randomId\("backup"\)/);
  assert.match(app, /pending\?\.phase === "request" \? pending\.attempt : failedBackupAttempt/);
  assert.match(app, /await submitBackupLocked\(retryAttempt\)/);
});

test("console access uses trusted claim output and never a server-state token path", async () => {
  const html = await source("index.html");
  assert.match(html, /owner-only output directory created by <code>covalent claim<\/code> on a trusted computer/);
  assert.match(html, /This console accepts a token only; it does not accept a setup code\./);
  assert.doesNotMatch(html, /\/data\/local-api-token/);
  assert.doesNotMatch(html, /token from <code>\/data\//);
});

test("backup providers come from verified connected devices, never a raw ID entry field", async () => {
  const [html, app, pairing] = await Promise.all([
    source("index.html"), source("app.js"), source("pairing-flow.js"),
  ]);
  assert.match(html, /data-providers-list/);
  assert.match(html, /data-providers-refresh/);
  assert.doesNotMatch(html, /<textarea name="providers"/);
  assert.match(app, /pairing\.providers\.listNamed\(api\)/);
  assert.match(app, /pairing\.providers\.selectedIds\(providerConnections, selected\(form, "providers"\)\)/);
  assert.match(pairing, /api\("\/api\/v1\/providers\/connect"/);
  assert.match(pairing, /JSON\.stringify\(\{ peerTransport: transport \}\)/);
  assert.match(pairing, /api\("\/api\/v1\/rosters\/current"\)/);
  assert.match(pairing, /async function networkDismiss/);
});

test("ordinary restore selects a completed remembered backup and keeps manual identifiers as recovery only", async () => {
  const [html, app, selection] = await Promise.all([
    source("index.html"), source("app.js"), source("backup-selection-flow.js"),
  ]);
  assert.match(html, /data-restore-choice/);
  assert.match(html, /Manual recovery identifiers/);
  assert.match(html, /Advanced backup identifiers/);
  assert.doesNotMatch(html, /value="snapshot-1"/);
  assert.match(app, /backupSelection\.choices\(backups, receipt\)/);
  assert.match(app, /const preserved = choices\.find/);
  assert.match(app, /identifiers\.backupId\.value = ""/);
  assert.match(app, /discardRestorePreviewForChangedInput\(\)/);
  assert.match(app, /backupSelection\.resolve\(rememberedRestoreChoices/);
  assert.match(app, /backupSelection\.nextSnapshotId\(\(\) => crypto\.randomUUID\(\)\)/);
  assert.match(app, /option\.textContent = choice\.label/);
  assert.match(selection, /remembered backup is no longer available/);
});

test("restore preview and backup completion copy avoid raw implementation identifiers", async () => {
  const [html, app, restore] = await Promise.all([
    source("index.html"), source("app.js"), source("restore-plan-flow.js"),
  ]);
  assert.match(html, /<ol class="restore-preview-list" data-restore-plan><\/ol>/);
  assert.doesNotMatch(app, /textContent = display\(page\.entries\)/);
  assert.match(app, /restore\.describePage\(firstPage, candidate\.authorizedRoot\)/);
  assert.match(app, /textContent = entry\.destination/);
  assert.match(app, /function backupCompletionCopy\(result, attempt\)/);
  assert.doesNotMatch(app, /Backup complete: \$\{result\.backupId\}/);
  assert.match(app, /local-only backup/);
  assert.match(restore, /cannot safely explain/);
});

test("normal backup results are decoded before their terminal job receipt is acknowledged", async () => {
  const [html, app, terminal] = await Promise.all([
    source("index.html"), source("app.js"), source("backup-terminal-flow.js"),
  ]);
  assert.match(html, /backup-terminal-flow\.js/);
  assert.match(app, /response\.headers\.get\("x-covalent-job-ack-required"\)/);
  assert.match(app, /if \(acknowledgement !== "true"\)/);
  assert.match(app, /acknowledgementRequired: true/);
  assert.match(app, /backupTerminal\.submit/);
  assert.match(app, /backupTerminal\.acknowledge/);
  assert.match(app, /setBackupSubmissionState\("complete", complete\);[\s\S]*?await acknowledgeBackupTerminalReceipt/);
  assert.match(app, /backupTerminal\.load\(globalThis\.localStorage, backupServerContext\)/);
  assert.match(app, /globalThis\.localStorage,[\s\S]*?backupServerContext,[\s\S]*?apiResponse/);
  assert.doesNotMatch(app, /backupTerminal\.(?:load|submit|acknowledge)\(globalThis\.sessionStorage/);
  assert.match(app, /pairing\.storage\.saveTabSession\(globalThis\.sessionStorage/);
  assert.match(terminal, /persist\(storage, receipt\)/);
  assert.match(terminal, /RECEIPT_REVIEW_AFTER_MS/);
  assert.match(terminal, /serverContext/);
  assert.match(terminal, /locks\.request\([\s\S]*?mode: "exclusive", ifAvailable: true/);
  assert.match(app, /withBackupTerminalLock\([\s\S]*?submitBackupLocked/);
  assert.match(app, /exclusive same-origin lock remains held[\s\S]*?acknowledgeBackupTerminalReceipt/);
  assert.match(terminal, /response\.status !== 204/);
  assert.match(terminal, /apiResponse\("\/api\/v1\/jobs\/acknowledge"/);
});

test("link cadence controls use the shared settings and Run Now contracts without exposing counters", async () => {
  const [html, app, flow] = await Promise.all([
    source("index.html"), source("app.js"), source("folder-sync-flow.js"),
  ]);
  assert.match(html, /name="cadenceMode" value="manual" required/);
  assert.match(html, /name="cadenceMode" value="scheduled" required/);
  assert.match(html, /name="cadenceMode" value="continuous" required/);
  assert.match(html, /name="intervalMinutes" type="number" min="15" max="525600"/);
  assert.match(html, /Wi-Fi only on Android devices/);
  assert.match(html, /Charging only on Android devices/);
  assert.match(app, /folderController\.updateLinkSettings\(share\.folderId, settings\)/);
  assert.match(app, /folderController\.runNow\(share\.folderId\)/);
  assert.match(flow, /"\/api\/v1\/sync\/run"/);
  assert.match(flow, /expectedGeneration: share\.linkRun\.generation/);
  assert.match(flow, /settingsRevision: share\.linkSettings\.revision/);
  assert.doesNotMatch(html, /generation|settings revision/i);
  assert.doesNotMatch(app, /textContent\s*=.*(?:generation|revision)/i);
  assert.doesNotMatch(app, /\/api\/v1\/sync\/conditions/);
});
