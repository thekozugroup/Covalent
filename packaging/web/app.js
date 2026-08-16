const $ = (selector) => document.querySelector(selector);
const PROTOCOL_VERSION = 1;
const message = $("[data-message]");
let token = "";
let restorePlan = null;

class NodeApiError extends Error {
  constructor(status, payload) {
    super(payload.message || `Request failed (${status})`);
    this.name = "NodeApiError";
    this.status = status;
    this.code = payload.code || `http_${status}`;
    this.retryable = payload.retryable === true;
    this.protocolVersion = payload.protocolVersion;
  }
}

function say(text, isError = false) {
  message.textContent = text;
  message.classList.toggle("error", isError);
}

function fail(error) {
  const recovery = error instanceof NodeApiError && error.retryable
    ? " You can retry this unchanged request."
    : "";
  say(`${error.message}${recovery}`, true);
}

async function api(path, options = {}) {
  const headers = new Headers(options.headers || {});
  headers.set("Accept", "application/json");
  if (token) headers.set("Authorization", `Bearer ${token}`);
  if (options.body) headers.set("Content-Type", "application/json");
  const response = await fetch(path, { ...options, headers, cache: "no-store" });
  if (!response.ok) {
    const decoded = await response.json().catch(() => ({}));
    const body = decoded && typeof decoded === "object" ? decoded : {};
    if (body.protocolVersion !== undefined && body.protocolVersion !== PROTOCOL_VERSION) {
      throw new Error(`The node uses unsupported protocol ${body.protocolVersion}.`);
    }
    throw new NodeApiError(response.status, body);
  }
  if (response.status === 204) return null;
  return response.json();
}

async function loadStatus() {
  try {
    const status = await api("/api/v1/status");
    if (status.protocolVersion !== PROTOCOL_VERSION) {
      throw new Error(`The node uses unsupported protocol ${status.protocolVersion}.`);
    }
    $("[data-device-name]").textContent = status.deviceName;
    $("[data-state]").textContent = `Service state: ${status.state}. Protocol ${status.protocolVersion}.`;
    document.querySelectorAll("[data-discovery]").forEach((el) => { el.textContent = status.lanDiscovery ? "On" : "Off"; });
  } catch (error) {
    $("[data-device-name]").textContent = "Node unavailable";
    $("[data-state]").textContent = "The local Covalent service did not respond. Check the container or daemon logs.";
    fail(error);
  }
}

async function loadBackups() {
  if (!token) return;
  const backups = await api("/api/v1/backups");
  const list = $("[data-backups-list]");
  list.replaceChildren();
  backups.forEach((backup) => {
    const item = document.createElement("li");
    const latest = backup.latestSnapshotId ? `latest ${backup.latestSnapshotId}` : "no local snapshot";
    item.textContent = `${backup.name} — ${latest}; ${backup.snapshotCount} retained; ${backup.selectedProviderIds.length} explicitly selected providers`;
    list.append(item);
  });
  $("[data-backups-empty]").hidden = backups.length > 0;
  if (backups.length === 0) $("[data-backups-empty]").textContent = "No remembered backups on this node.";
}

function formData(form) { return new FormData(form); }
function selected(form, name) { return [...form.querySelectorAll(`[name="${name}"]:checked`)].map((input) => input.value); }
function randomId(prefix) { return `${prefix}-${crypto.randomUUID().replaceAll("-", "").slice(0, 16)}`; }
function display(value) { return JSON.stringify(value, null, 2); }

$("[data-token-form]").addEventListener("submit", async (event) => {
  event.preventDefault();
  token = formData(event.currentTarget).get("token").trim();
  try { await api("/api/v1/config/export", { method: "POST" }); await loadBackups(); say("Console unlocked for this tab only."); }
  catch (error) { token = ""; fail(error); }
});

$("[data-refresh]").addEventListener("click", async () => { await loadStatus(); if (token) await loadBackups(); });
$("[data-backups-refresh]").addEventListener("click", async () => {
  try { await loadBackups(); say("Backup list refreshed from the node."); }
  catch (error) { fail(error); }
});
document.querySelectorAll("[data-tab]").forEach((tab) => tab.addEventListener("click", () => {
  document.querySelectorAll("[data-tab]").forEach((button) => button.setAttribute("aria-selected", String(button === tab)));
  document.querySelectorAll("[data-panel]").forEach((panel) => { panel.hidden = panel.dataset.panel !== tab.dataset.tab; });
}));

$("[data-pair-create]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  try {
    const invitation = await api("/api/v1/pair/invitations", { method: "POST", body: JSON.stringify({ endpoints: [data.get("endpoint")], lifetimeMs: Number(data.get("minutes")) * 60000 }) });
    say(`Invitation created. Copy this JSON to the other device:\n${display(invitation)}`);
  } catch (error) { fail(error); }
});

$("[data-pair-accept]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  try {
    const session = await api("/api/v1/pair/accept", { method: "POST", body: JSON.stringify({ invitation: JSON.parse(data.get("invitation")), responderName: data.get("name"), responderRoles: selected(event.currentTarget, "responderRole"), inviterRoles: ["backup_writer", "backup_reader"] }) });
    say(`Compare this code on both devices: ${session.authenticationString}\nThen paste this session into Confirm and finish:\n${display(session)}`);
  } catch (error) { fail(error); }
});

$("[data-pair-confirm]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget); const submitter = event.submitter;
  try {
    const session = JSON.parse(data.get("session")); const side = submitter.value;
    const confirmed = await api(`/api/v1/pair/confirm/${side}`, { method: "POST", body: JSON.stringify({ session, displayedCode: data.get("code") }) });
    const confirmation = await api(`/api/v1/pair/finalize/${side}`, { method: "POST", body: JSON.stringify({ session: confirmed }) });
    say(`Pairing finalized. Save this confirmation for the other device if it still needs to finalize:\n${display(confirmation)}`);
  } catch (error) { fail(error); }
});

$("[data-backup-form]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  try {
    const providers = data.get("providers").split(/\s+/).filter(Boolean);
    const result = await api("/api/v1/backups", { method: "POST", body: JSON.stringify({ sourceRoot: data.get("sourceRoot"), displayName: data.get("displayName"), snapshotId: data.get("snapshotId"), jobId: randomId("backup"), selectedProviderIds: providers }) });
    await loadBackups();
    say(`Backup complete: ${result.backupId}, ${result.entries} entries, ${result.selectedProviders} explicitly selected providers.`);
  } catch (error) { fail(error); }
});

$("[data-restore-preview]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  try {
    restorePlan = await api("/api/v1/restores/preview", { method: "POST", body: JSON.stringify({ backupId: data.get("backupId"), snapshotId: data.get("snapshotId"), targetRoot: data.get("targetRoot"), conflictPolicy: data.get("conflictPolicy"), jobId: randomId("restore") }) });
    $("[data-restore-summary]").textContent = `${restorePlan.entries.length} entries are signed for ${restorePlan.authorizedRoot}.`;
    $("[data-restore-plan]").textContent = display(restorePlan.entries);
    $("[data-restore-result]").hidden = false; $("[data-restore-confirm]").checked = false; $("[data-restore-execute]").disabled = true;
    say("Preview complete. Review the target and conflict actions before authorizing the write.");
  } catch (error) { restorePlan = null; fail(error); }
});
$("[data-restore-confirm]").addEventListener("change", (event) => { $("[data-restore-execute]").disabled = !event.currentTarget.checked || !restorePlan; });
$("[data-restore-execute]").addEventListener("click", async () => {
  if (!restorePlan) return;
  try { const result = await api("/api/v1/restores/execute", { method: "POST", body: JSON.stringify({ plan: restorePlan }) }); say(`Restore complete: ${result.filesRestored} files, ${result.directoriesCreated} directories.`); restorePlan = null; $("[data-restore-execute]").disabled = true; }
  catch (error) { fail(error); }
});

$("[data-settings-export]").addEventListener("click", async () => {
  try { const settings = await api("/api/v1/config/export", { method: "POST" }); const blob = new Blob([display(settings)], { type: "application/json" }); const link = Object.assign(document.createElement("a"), { href: URL.createObjectURL(blob), download: "covalent-settings.json" }); link.click(); URL.revokeObjectURL(link.href); say("Safe settings downloaded."); }
  catch (error) { fail(error); }
});
$("[data-settings-import]").addEventListener("submit", async (event) => {
  event.preventDefault(); const data = formData(event.currentTarget);
  try { await api("/api/v1/config/import", { method: "POST", body: JSON.stringify({ confirmed: data.get("confirmed") === "on", settings: JSON.parse(data.get("settings")) }) }); say("Safe settings imported. Restart the node before expecting a changed LAN-discovery service state."); await loadStatus(); }
  catch (error) { fail(error); }
});

loadStatus();
