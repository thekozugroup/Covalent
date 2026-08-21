#!/usr/bin/env node

import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, relative, resolve } from "node:path";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const routerSource = readFileSync(resolve(repositoryRoot, "crates/covalent-node/src/lib.rs"), "utf8");
const openapiSource = readFileSync(resolve(repositoryRoot, "docs/api/openapi.yaml"), "utf8");
const methods = new Set(["get", "post", "put", "patch", "delete"]);

// Native clients are the third party to this contract. Diffing only the router
// against OpenAPI lets a route that neither side declares pass silently while
// every client 404s on it, which is how POST /api/v1/pair/network/start shipped
// with no handler. Each client tree is scanned for its own path literals, with
// the language's string interpolation standing in for a path parameter.
const clientTrees = [
  {
    label: "Apple client",
    directory: "apps/apple/Sources",
    extension: ".swift",
    interpolation: /\\\([^)]*\)/g,
  },
  {
    label: "Android client",
    directory: "apps/android/app/src/main/java",
    extension: ".kt",
    interpolation: /\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*/g,
  },
];

function normalizePath(path) {
  return path.replaceAll(/\{[^}]+\}/g, "{}");
}

function operationKey(method, path) {
  return `${method.toUpperCase()} ${normalizePath(path)}`;
}

function routerOperations(source) {
  const operations = new Set();
  let searchOffset = 0;
  while (true) {
    const marker = source.indexOf(".route(", searchOffset);
    if (marker === -1) break;
    const opening = source.indexOf("(", marker);
    let depth = 0;
    let quote = null;
    let escaped = false;
    let closing = -1;
    for (let index = opening; index < source.length; index += 1) {
      const character = source[index];
      if (quote !== null) {
        if (escaped) escaped = false;
        else if (character === "\\") escaped = true;
        else if (character === quote) quote = null;
        continue;
      }
      if (character === '"' || character === "'") quote = character;
      else if (character === "(") depth += 1;
      else if (character === ")") {
        depth -= 1;
        if (depth === 0) {
          closing = index;
          break;
        }
      }
    }
    if (closing === -1) throw new Error("unterminated Axum route expression");
    const expression = source.slice(opening + 1, closing);
    const path = expression.match(/^\s*"(\/api\/v1[^"?]*)"\s*,/)?.[1];
    if (path !== undefined) {
      for (const match of expression.matchAll(/\b(get|post|put|patch|delete)\s*\(/g)) {
        operations.add(operationKey(match[1], path));
      }
    }
    searchOffset = closing + 1;
  }
  return operations;
}

function openapiOperations(source) {
  const operations = new Set();
  let currentPath = null;
  for (const line of source.split(/\r?\n/)) {
    const path = line.match(/^  (\/api\/v1[^:]*):\s*$/)?.[1];
    if (path !== undefined) {
      currentPath = path;
      continue;
    }
    const method = line.match(/^    ([a-z]+):\s*$/)?.[1];
    if (currentPath !== null && method !== undefined && methods.has(method)) {
      operations.add(operationKey(method, currentPath));
    }
    if (/^[^ ]/.test(line)) currentPath = null;
  }
  return operations;
}

function sourceFiles(directory, extension) {
  const absolute = resolve(repositoryRoot, directory);
  return readdirSync(absolute, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(extension))
    .map((entry) => resolve(entry.parentPath ?? entry.path, entry.name));
}

function clientPathReferences(tree) {
  const references = [];
  for (const file of sourceFiles(tree.directory, tree.extension)) {
    const lines = readFileSync(file, "utf8").split(/\r?\n/);
    lines.forEach((line, index) => {
      for (const match of line.matchAll(/"(\/?api\/v1[^"]*)"/g)) {
        const path = `/${match[1]
          .replaceAll(tree.interpolation, "{}")
          .split("?")[0]
          .replace(/^\/+/, "")}`;
        references.push({
          client: tree.label,
          location: `${relative(repositoryRoot, file)}:${index + 1}`,
          path,
        });
      }
    });
  }
  return references;
}

// A literal ending in a separator is a prefix the caller appends to, so it is
// satisfied by any route living beneath it. Every other literal is a whole path
// and must name a route exactly.
function isRoutedClientPath(path, routerPaths) {
  if (path.endsWith("/")) {
    return [...routerPaths].some((route) => route.startsWith(path) && route.length > path.length);
  }
  return routerPaths.has(path);
}

const runtime = routerOperations(routerSource);
const documented = openapiOperations(openapiSource);
const undocumented = [...runtime].filter((operation) => !documented.has(operation)).sort();
const unimplemented = [...documented].filter((operation) => !runtime.has(operation)).sort();

const routerPaths = new Set([...runtime].map((operation) => operation.split(" ")[1]));
const clientReferences = clientTrees.flatMap(clientPathReferences);
const unroutedClientPaths = clientReferences
  .filter((reference) => !isRoutedClientPath(reference.path, routerPaths))
  .map((reference) => `${reference.path} (${reference.client}, ${reference.location})`)
  .sort();

if (runtime.size === 0 || documented.size === 0) {
  throw new Error("API route extraction unexpectedly returned no operations");
}
for (const tree of clientTrees) {
  if (!clientReferences.some((reference) => reference.client === tree.label)) {
    throw new Error(`no API path literals found in ${tree.directory}; the extractor is stale`);
  }
}
if (undocumented.length > 0 || unimplemented.length > 0 || unroutedClientPaths.length > 0) {
  if (undocumented.length > 0) console.error(`runtime operations absent from OpenAPI:\n${undocumented.join("\n")}`);
  if (unimplemented.length > 0) console.error(`OpenAPI operations absent from runtime:\n${unimplemented.join("\n")}`);
  if (unroutedClientPaths.length > 0) {
    console.error(`client-called paths absent from the Axum router:\n${unroutedClientPaths.join("\n")}`);
  }
  process.exitCode = 1;
} else {
  const clientPaths = new Set(clientReferences.map((reference) => reference.path)).size;
  console.log(`OpenAPI/runtime route coverage: ${runtime.size} operations match`);
  console.log(`Client/runtime route coverage: ${clientPaths} client paths are routed`);
}
