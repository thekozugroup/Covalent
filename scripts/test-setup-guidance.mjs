#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");
const setupFiles = [
  "README.md",
  "docs/getting-started.md",
  "docs/troubleshooting.md",
  "docs/platform/macos.md",
  "docs/platform/android.md",
  "docs/platform/unraid.md",
  "docs/platform/atlas-tailscale.md",
  "packaging/docker/README.md",
  "apps/apple/README.md",
  "apps/android/README.md",
];

const errors = [];
const requireFile = (relative) => {
  const absolute = path.join(root, relative);
  if (!fs.existsSync(absolute)) {
    errors.push(`missing setup file: ${relative}`);
    return "";
  }
  return fs.readFileSync(absolute, "utf8");
};

const documents = new Map(setupFiles.map((relative) => [relative, requireFile(relative)]));

function slugForHeading(heading) {
  return heading
    .trim()
    .toLowerCase()
    .replace(/<[^>]*>/g, "")
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-");
}

function headings(markdown) {
  return new Set(
    markdown
      .split(/\r?\n/u)
      .filter((line) => /^#{1,6}\s+/u.test(line))
      .map((line) => slugForHeading(line.replace(/^#{1,6}\s+/u, ""))),
  );
}

for (const [relative, markdown] of documents) {
  const sourceDirectory = path.dirname(path.join(root, relative));
  const linkPattern = /!?(?:\[[^\]]*\])\(([^)]+)\)/gu;
  for (const match of markdown.matchAll(linkPattern)) {
    let destination = match[1].trim();
    if (destination.startsWith("<") && destination.endsWith(">")) {
      destination = destination.slice(1, -1);
    }
    if (/^(?:https?:|mailto:)/u.test(destination) || destination.startsWith("#")) continue;
    destination = destination.split(/\s+["']/u, 1)[0];
    const [rawTarget, rawFragment = ""] = destination.split("#", 2);
    const target = decodeURIComponent(rawTarget);
    if (target === "") continue;
    const absoluteTarget = path.resolve(sourceDirectory, target);
    if (!fs.existsSync(absoluteTarget)) {
      errors.push(`${relative}: broken local link ${destination}`);
      continue;
    }
    if (rawFragment !== "" && fs.statSync(absoluteTarget).isFile() && absoluteTarget.endsWith(".md")) {
      const targetHeadings = headings(fs.readFileSync(absoluteTarget, "utf8"));
      if (!targetHeadings.has(decodeURIComponent(rawFragment).toLowerCase())) {
        errors.push(`${relative}: missing heading #${rawFragment} in ${target}`);
      }
    }
  }
}

function requireText(file, needle, purpose) {
  const text = documents.get(file) ?? "";
  if (!text.includes(needle)) errors.push(`${file}: missing ${purpose}: ${needle}`);
}

requireText("README.md", "[Back up your first folder](docs/getting-started.md)", "primary setup link");
requireText("docs/getting-started.md", "Apple Developer ID/notarization is not part", "macOS personal-use scope");
requireText("docs/getting-started.md", "Android production signing is deferred", "Android personal-use scope");
requireText("docs/getting-started.md", "Unraid template and Atlas deployment remain blocked", "honest unavailable-server scope");
requireText("docs/platform/android.md", "app-debug.apk", "installable personal APK");
requireText("docs/platform/android.md", "app-release-unsigned.apk", "unsigned release APK warning");
requireText("docs/platform/android.md", "./scripts/build-personal-android-apk.sh", "one-command Android builder");
requireText("docs/platform/android.md", "Choose token file", "protected token-file handoff");
requireText("docs/platform/macos.md", "ad-hoc", "personal macOS signing explanation");
requireText("docs/platform/macos.md", "not notarized", "personal-use notarization boundary");
requireText("docs/platform/macos.md", "./scripts/build-personal-macos-app.sh", "one-command macOS builder");
requireText("docs/platform/macos.md", "apps/apple/Covalent.xcodeproj", "generated-project side effect");
requireText("docs/platform/unraid.md", "/mnt/user/system/covalent-secrets/key-encryption-key", "Unraid KEK path");
requireText("packaging/docker/README.md", "/run/secrets/covalent-kek", "Docker KEK mount");
requireText("packaging/docker/README.md", "first-backup.txt", "restorable Docker starter file");
requireText("packaging/docker/README.md", "COVALENT_HTTPS_BIND_IP=192.168.1.50", "ordinary LAN publishing example");
requireText("packaging/docker/README.md", 'covalent_host_root="$HOME/.covalent-server"', "Docker Desktop shared host root");
requireText("packaging/docker/README.md", "## Enroll or remove the claimed CA", "exact CA enrollment anchor");
requireText("docs/platform/atlas-tailscale.md", "operator@atlas.example-tailnet.ts.net sh -s", "remote Atlas path validation");
for (const file of ["docs/platform/unraid.md", "docs/platform/atlas-tailscale.md"]) {
  requireText(file, "../../packaging/docker/README.md#enroll-or-remove-the-claimed-ca", "exact CA enrollment link");
}

const macosGuide = documents.get("docs/platform/macos.md") ?? "";
const localCheckpoint = macosGuide.indexOf("## 4. Complete the local first recovery checkpoint");
const optionalAtlas = macosGuide.indexOf("## 5. Optional after the checkpoint: prepare Atlas");
if (localCheckpoint < 0 || optionalAtlas < 0 || localCheckpoint >= optionalAtlas) {
  errors.push("docs/platform/macos.md: local recovery checkpoint must precede optional Atlas setup");
}

const gettingStarted = documents.get("docs/getting-started.md") ?? "";
const orderedHeadings = [
  "## 1. Install the server or local app",
  "## 2. Claim an always-on server",
  "## 3. Connect a client",
  "## 4. Make a small first backup",
  "## 5. Verify it",
  "## 6. Restore into a different folder",
  "## 7. Add real source-loss protection",
];
let priorIndex = -1;
for (const heading of orderedHeadings) {
  const index = gettingStarted.indexOf(heading);
  if (index < 0) errors.push(`docs/getting-started.md: missing ordered heading ${heading}`);
  else if (index <= priorIndex) errors.push(`docs/getting-started.md: heading out of order ${heading}`);
  priorIndex = index;
}

const activeSetup = [
  "README.md",
  "docs/getting-started.md",
  "docs/troubleshooting.md",
  "docs/platform/macos.md",
  "docs/platform/android.md",
  "docs/platform/unraid.md",
  "docs/platform/atlas-tailscale.md",
  "packaging/docker/README.md",
].map((relative) => documents.get(relative) ?? "").join("\n");

if (/&lt;[^&\n]+&gt;|<replacement-[^>]+>/u.test(activeSetup)) {
  errors.push("active setup guidance contains a copy-paste placeholder that looks executable");
}
if (/-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/u.test(activeSetup)) {
  errors.push("active setup guidance contains private-key material");
}
if (/COVALENT_ADVERTISED_PEER_ADDRESS=[A-Za-z][A-Za-z0-9.-]*:/u.test(activeSetup)) {
  errors.push("advertised peer examples must use a numeric IP:port, not a hostname");
}

for (const required of ["TCP `8443`", "UDP `8787`", "setup-doctor.sh", "validate-setup-paths.sh"]) {
  if (!gettingStarted.includes(required)) errors.push(`docs/getting-started.md: missing ${required}`);
}

const scriptReferences = new Set();
for (const markdown of documents.values()) {
  for (const match of markdown.matchAll(/(?:^|[\s`(])((?:\.\/)?scripts\/[A-Za-z0-9._/-]+\.sh)\b/gu)) {
    scriptReferences.add(match[1].replace(/^\.\//u, ""));
  }
}
for (const relative of scriptReferences) {
  const absolute = path.join(root, relative);
  if (!fs.existsSync(absolute)) errors.push(`documented script does not exist: ${relative}`);
  else if ((fs.statSync(absolute).mode & 0o111) === 0) errors.push(`documented script is not executable: ${relative}`);
}

const compose = fs.readFileSync(path.join(root, "packaging/docker/compose.yaml"), "utf8");
for (const expected of [
  "65532:65532",
  "8443}:8443/tcp",
  "8787}:8787/udp",
  "target: /source",
  "read_only: true",
  "target: /restore",
]) {
  if (!compose.includes(expected)) errors.push(`packaging/docker/compose.yaml: missing setup contract ${expected}`);
}

const unraid = fs.readFileSync(path.join(root, "packaging/unraid/covalent.xml"), "utf8");
for (const expected of [
  'Target="/run/secrets/covalent-kek"',
  'Target="/source"',
  'Target="/restore"',
  'Mode="ro"',
  '99:100',
]) {
  if (!unraid.includes(expected)) errors.push(`packaging/unraid/covalent.xml: missing setup contract ${expected}`);
}

if (errors.length > 0) {
  for (const error of errors) console.error(`setup guidance: ${error}`);
  process.exit(1);
}

console.log(`setup guidance: ${setupFiles.length} guides and ${scriptReferences.size} script references verified`);
