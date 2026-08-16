#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const routerSource = readFileSync(resolve(repositoryRoot, "crates/covalent-node/src/lib.rs"), "utf8");
const openapiSource = readFileSync(resolve(repositoryRoot, "docs/api/openapi.yaml"), "utf8");
const methods = new Set(["get", "post", "put", "patch", "delete"]);

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

const runtime = routerOperations(routerSource);
const documented = openapiOperations(openapiSource);
const undocumented = [...runtime].filter((operation) => !documented.has(operation)).sort();
const unimplemented = [...documented].filter((operation) => !runtime.has(operation)).sort();

if (runtime.size === 0 || documented.size === 0) {
  throw new Error("API route extraction unexpectedly returned no operations");
}
if (undocumented.length > 0 || unimplemented.length > 0) {
  if (undocumented.length > 0) console.error(`runtime operations absent from OpenAPI:\n${undocumented.join("\n")}`);
  if (unimplemented.length > 0) console.error(`OpenAPI operations absent from runtime:\n${unimplemented.join("\n")}`);
  process.exitCode = 1;
} else {
  console.log(`OpenAPI/runtime route coverage: ${runtime.size} operations match`);
}
