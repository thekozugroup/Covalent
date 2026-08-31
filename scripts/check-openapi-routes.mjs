#!/usr/bin/env node

import { readFileSync, statSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

// The Axum router, the OpenAPI document and every HTTP client in this repository
// are three statements of one contract, and CI only ever compared the first two.
// POST /api/v1/pair/network/start therefore shipped with no handler at all: both
// mobile clients 404'd on the product's core flow while this gate stayed green.
//
// This gate closes the contract in both directions:
//
//   * every client tree is discovered from the tracked file list, so a client
//     that nobody remembered to declare fails the build instead of being skipped;
//   * a client call site is resolved to METHOD + PATH, so the right path with the
//     wrong verb is a failure;
//   * a path expression is folded across concatenation, interpolation and local
//     constants, and anything that cannot be folded down to a literal template
//     rooted at /api/v1 is a failure rather than a silent pass;
//   * the extractor's own assumptions (which helper carries which verb) are read
//     back out of the client sources, so a helper that changes verb breaks the
//     gate instead of quietly invalidating it.
//
// Nothing here is allowed to fail open. A missing directory, an empty glob, an
// unrecognized call shape, an unresolvable path or an unavailable tool all exit
// non-zero.

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const routerFile = "crates/covalent-node/src/lib.rs";
const openapiFile = "docs/api/openapi.yaml";
const httpMethods = new Set(["GET", "POST", "PUT", "PATCH", "DELETE"]);
const specMethods = new Set(["get", "post", "put", "patch", "delete"]);

const appleClientFile = "apps/apple/Sources/CovalentShared/NodeClient.swift";
const androidClientFile = "apps/android/app/src/main/java/life/michaelwong/covalent/data/CovalentNodeClient.kt";
const webConsoleFile = "packaging/web/app.js";

// Client trees are declared, but the declaration is not trusted to be complete:
// `classifyRepository` below proves that every tracked file mentioning the API
// prefix belongs to exactly one declared region, so a new client tree is a build
// failure until it is described here.
const clientTrees = [
  {
    label: "Apple client",
    kind: "request",
    directory: "apps/apple/Sources",
    extension: ".swift",
    interpolation: /\\\([^)]*\)/g,
    // A call site is only understood if its callee appears here. Anything else
    // fails, which is what makes the table safe to keep short.
    calls: {
      send: { path: { label: "path" }, method: { kind: "argument", label: "method", default: "sendDefault" } },
      sendNoContent: { path: { label: "path" }, method: { kind: "derived", derivation: "sendNoContent" } },
      execute: { path: { label: "path" }, method: { kind: "argument", label: "method" } },
      authenticatedRequest: { path: { label: "path" }, method: { kind: "argument", label: "method" } },
      previewRestoreReference: { path: { label: "path" }, method: { kind: "derived", derivation: "previewRestoreReference" } },
    },
    derivations: {
      sendDefault: { file: appleClientFile, pattern: /func send\b[^)]*?method: String = "([A-Za-z]+)"/g },
      sendNoContent: { file: appleClientFile, pattern: /func sendNoContent\b[\s\S]{0,400}?method: "([A-Za-z]+)"/g },
      previewRestoreReference: { file: appleClientFile, pattern: /func previewRestoreReference\b[\s\S]{0,400}?method: "([A-Za-z]+)"/g },
    },
    wiring: [
      {
        description: "URLRequest.httpMethod is taken from the caller's method argument",
        file: appleClientFile,
        pattern: /request\.httpMethod = method/g,
      },
    ],
  },
  {
    label: "Android client",
    kind: "request",
    directory: "apps/android/app/src/main/java",
    extension: ".kt",
    interpolation: /\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*/g,
    calls: {
      request: { path: { index: 2, label: "path" }, method: { kind: "argument", index: 1, label: "method" } },
      post: { path: { index: 2, label: "path" }, method: { kind: "derived", derivation: "post" } },
      postNoContent: { path: { index: 2, label: "path" }, method: { kind: "derived", derivation: "postNoContent" } },
      openConnection: { path: { index: 1, label: "path" }, method: { kind: "argument", index: 2, label: "method" } },
      // queueTransfer stores the path for TransferWorker to replay later, so the
      // literal is a constant rather than a request. It is checked as a handoff:
      // the client must issue that exact path somewhere it can be verb-checked.
      queueTransfer: { path: { index: 3, label: "path" }, method: { kind: "handoff" } },
    },
    derivations: {
      post: {
        file: androidClientFile,
        pattern: /fun post\(baseUrl: String, token: String, path: String, payload: JSONObject\): JSONObject = request\(baseUrl, "([A-Za-z]+)", path,/g,
      },
      postNoContent: {
        file: androidClientFile,
        pattern: /fun postNoContent\(baseUrl: String, token: String, path: String, payload: JSONObject\) \{ request\(baseUrl, "([A-Za-z]+)", path,/g,
      },
    },
    wiring: [
      {
        description: "HttpURLConnection.requestMethod is taken from the caller's method argument",
        file: androidClientFile,
        pattern: /requestMethod = method/g,
      },
    ],
  },
  {
    label: "Web console",
    kind: "request",
    directory: "packaging/web",
    extension: ".js",
    exclude: ["packaging/web/tests"],
    interpolation: /\$\{[^}]*\}/g,
    calls: {
      api: { path: { index: 0 }, method: { kind: "options", index: 1, default: "webDefault" } },
      apiResponse: { path: { index: 0 }, method: { kind: "options", index: 1, default: "webDefault" } },
    },
    derivations: {
      // apiResponse() forwards caller options to fetch untouched and sets no
      // method of its own, so an omitted method is a GET. Pinning the exact
      // forwarding line makes that inference break loudly if it stops holding.
      webDefault: { file: webConsoleFile, pattern: /await fetch\(path, \{ \.\.\.options, headers, cache: "no-store" \}\)/g, constant: "GET" },
    },
    wiring: [
      { description: "the console apiResponse() helper takes its method from the caller's options", file: webConsoleFile, pattern: /async function apiResponse\(path, options = \{\}\)/g },
      { description: "the console api() helper forwards its path and options to apiResponse()", file: webConsoleFile, pattern: /return \(await apiResponse\(path, options\)\)\.body;/g },
    ],
  },
  // Mock-server assertions are not requests, so they carry no verb; their path
  // literals are still checked against the router, because a test asserting a
  // path the node never serves is exactly how the pairing bug stayed invisible.
  { label: "Apple client tests", kind: "assertion", directory: "apps/apple/Tests", extension: ".swift", interpolation: /\\\([^)]*\)/g },
  { label: "Android client tests", kind: "assertion", directory: "apps/android/app/src/test", extension: ".kt", interpolation: /\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*/g },
  {
    label: "Android instrumentation tests",
    kind: "assertion",
    directory: "apps/android/app/src/androidTest",
    extension: ".kt",
    interpolation: /\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*/g,
    mayBeEmpty: true,
  },
  { label: "Web console tests", kind: "assertion", directory: "packaging/web/tests", extension: ".mjs", interpolation: /\$\{[^}]*\}/g },
];

// Every other place the API prefix may legitimately appear. A tracked file that
// matches neither a client tree nor one of these is an undeclared HTTP client
// until proven otherwise, and fails the build.
const nonClientRegions = [
  { label: "Axum router", match: (path) => path === routerFile },
  { label: "OpenAPI document", match: (path) => path === openapiFile },
  { label: "node implementation and its tests", match: (path) => path.startsWith("crates/") },
  { label: "gate tooling", match: (path) => path.startsWith("scripts/") },
  { label: "CI configuration", match: (path) => path.startsWith(".github/") },
  { label: "documentation", match: (path) => path.startsWith("docs/") || path.endsWith(".md") },
];

const failures = [];
const fail = (message) => failures.push(message);
const flatCache = new Map();
const derivedMethods = new Map();

function trackedFiles() {
  const listing = execFileSync("git", ["-C", repositoryRoot, "ls-files", "-z"], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  const files = listing.split("\0").filter(Boolean);
  if (files.length === 0) throw new Error("git ls-files returned no files; the repository inventory is unusable");
  return files;
}

// Collapses a source file to a single line with comments removed, keeping a
// line number for every surviving character. String bodies are preserved
// verbatim so path literals survive intact.
// A `/` that begins a regular expression, not a division. Deciding needs the
// previous significant token, and this is the standard heuristic: a regex can
// only start where a value is expected. Enabled for JavaScript sources alone,
// so a Rust, Swift or Kotlin division is never mistaken for one.
const REGEX_MAY_FOLLOW = new Set([
  undefined, "(", ",", "=", ":", "[", "!", "&", "|", "?", "{", "}", ";", "+", "-", "*", "%", "<", ">", "~", "^",
]);

function flatten(text, allowRegex = false) {
  const characters = [];
  const lines = [];
  let line = 1;
  let index = 0;
  const push = (character) => {
    characters.push(character);
    lines.push(line);
  };
  const pushString = (quote) => {
    push(text[index]);
    index += 1;
    while (index < text.length) {
      const character = text[index];
      if (character === "\\") {
        push(character);
        if (index + 1 < text.length) push(text[index + 1]);
        index += 2;
        continue;
      }
      if (character === "\n") line += 1;
      push(character);
      index += 1;
      if (character === quote) return;
    }
  };
  let lastSignificant;
  const pushRegex = () => {
    push(text[index]);
    index += 1;
    let inClass = false;
    while (index < text.length) {
      const character = text[index];
      if (character === "\n") return; // Unterminated: not a regex after all.
      if (character === "\\") {
        push(character);
        if (index + 1 < text.length) push(text[index + 1]);
        index += 2;
        continue;
      }
      push(character);
      index += 1;
      if (character === "[") inClass = true;
      else if (character === "]") inClass = false;
      else if (character === "/" && !inClass) {
        while (index < text.length && /[a-z]/.test(text[index])) {
          push(text[index]);
          index += 1;
        }
        return;
      }
    }
  };
  while (index < text.length) {
    const character = text[index];
    if (character === "/" && text[index + 1] === "/") {
      while (index < text.length && text[index] !== "\n") index += 1;
      continue;
    }
    if (character === "/" && text[index + 1] === "*") {
      index += 2;
      while (index < text.length && !(text[index] === "*" && text[index + 1] === "/")) {
        if (text[index] === "\n") line += 1;
        index += 1;
      }
      index += 2;
      continue;
    }
    if (text.startsWith('"""', index)) {
      push('"');
      push('"');
      push('"');
      index += 3;
      while (index < text.length && !text.startsWith('"""', index)) {
        if (text[index] === "\n") line += 1;
        push(text[index]);
        index += 1;
      }
      push('"');
      push('"');
      push('"');
      index += 3;
      continue;
    }
    if (allowRegex && character === "/" && REGEX_MAY_FOLLOW.has(lastSignificant)) {
      pushRegex();
      lastSignificant = "/";
      continue;
    }
    if (character === '"' || character === "`" || character === "'") {
      pushString(character);
      lastSignificant = character;
      continue;
    }
    if (/\s/.test(character)) {
      if (characters.length > 0 && characters[characters.length - 1] !== " ") push(" ");
      if (character === "\n") line += 1;
      index += 1;
      continue;
    }
    push(character);
    lastSignificant = character;
    index += 1;
  }
  return { flat: characters.join(""), lineAt: (offset) => lines[Math.min(offset, lines.length - 1)] ?? 1 };
}

function loadFlat(relativePath) {
  if (!flatCache.has(relativePath)) {
    let text;
    try {
      text = readFileSync(resolve(repositoryRoot, relativePath), "utf8");
    } catch (error) {
      throw new Error(`${relativePath} could not be read (${error.code ?? error.message}); this gate names it and is now stale`);
    }
    // Regex literals are JavaScript-only. A `/` in Rust, Swift or Kotlin is
    // division, and treating it as a regex would swallow real code.
    const isJavaScript = relativePath.endsWith(".js") || relativePath.endsWith(".mjs");
    flatCache.set(relativePath, flatten(text, isJavaScript));
  }
  return flatCache.get(relativePath);
}

function matchOnce(spec, description) {
  const { flat } = loadFlat(spec.file);
  const matches = [...flat.matchAll(spec.pattern)];
  if (matches.length !== 1) {
    throw new Error(
      `${description}: expected exactly one match in ${spec.file} but found ${matches.length}; the client changed shape and this extractor is stale`,
    );
  }
  return matches[0];
}

function matchAtLeastOnce(spec, description) {
  const { flat } = loadFlat(spec.file);
  if ([...flat.matchAll(spec.pattern)].length === 0) {
    throw new Error(`${description}: no longer present in ${spec.file}; the client changed shape and this extractor is stale`);
  }
}

function deriveMethod(tree, name) {
  const key = `${tree.label}:${name}`;
  if (derivedMethods.has(key)) return derivedMethods.get(key);
  const spec = tree.derivations?.[name];
  if (spec === undefined) throw new Error(`${tree.label}: no derivation named ${name}`);
  const match = matchOnce(spec, `${tree.label} ${name} method wiring`);
  const method = (spec.constant ?? match[1]).toUpperCase();
  if (!httpMethods.has(method)) throw new Error(`${tree.label}: derivation ${name} produced ${method}, which is not an HTTP method`);
  derivedMethods.set(key, method);
  return method;
}

// ---------------------------------------------------------------- expressions

function stringLiteralAt(flat, start) {
  const quote = flat[start];
  if (quote !== '"' && quote !== "`" && quote !== "'") return null;
  let index = start + 1;
  while (index < flat.length) {
    if (flat[index] === "\\") {
      index += 2;
      continue;
    }
    if (flat[index] === quote) return { start, end: index + 1, body: flat.slice(start + 1, index) };
    index += 1;
  }
  return null;
}

function stringLiterals(flat) {
  const literals = [];
  let index = 0;
  while (index < flat.length) {
    const literal = stringLiteralAt(flat, index);
    if (literal === null) {
      index += 1;
      continue;
    }
    literals.push(literal);
    index = literal.end;
  }
  return literals;
}

const closers = { "(": ")", "[": "]", "{": "}" };

// Splits an argument list (or any comma-separated run) at commas that are not
// nested inside brackets or strings.
function splitTopLevel(text) {
  const parts = [];
  let depth = 0;
  let start = 0;
  let index = 0;
  while (index < text.length) {
    const literal = stringLiteralAt(text, index);
    if (literal !== null) {
      index = literal.end;
      continue;
    }
    const character = text[index];
    if (character in closers) depth += 1;
    else if (character === ")" || character === "]" || character === "}") depth -= 1;
    else if (character === "," && depth === 0) {
      parts.push({ text: text.slice(start, index), start });
      start = index + 1;
    }
    index += 1;
  }
  parts.push({ text: text.slice(start), start });
  return parts;
}

function matchingClose(flat, open) {
  const depth = [];
  let index = open;
  while (index < flat.length) {
    const literal = stringLiteralAt(flat, index);
    if (literal !== null) {
      index = literal.end;
      continue;
    }
    const character = flat[index];
    if (character in closers) depth.push(closers[character]);
    else if (character === ")" || character === "]" || character === "}") {
      if (depth.pop() !== character) return -1;
      if (depth.length === 0) return index;
    }
    index += 1;
  }
  return -1;
}

// Walks backwards to the innermost bracket that is still open at `offset`.
function enclosingOpen(flat, offset) {
  const stack = [];
  let index = 0;
  while (index < offset) {
    const literal = stringLiteralAt(flat, index);
    if (literal !== null && literal.end <= offset) {
      index = literal.end;
      continue;
    }
    const character = flat[index];
    if (character in closers) stack.push({ character, index });
    else if (character === ")" || character === "]" || character === "}") stack.pop();
    index += 1;
  }
  return stack.length === 0 ? null : stack[stack.length - 1];
}

// True when `offset` sits in the path argument of a call this gate recognizes.
function isPathArgument(flat, offset, calls) {
  const call = enclosingCall(flat, offset);
  if (call === null || call.declaration === true) return false;
  const shape = calls[call.callee];
  if (shape === undefined) return false;
  return call.indexOf(offset) === selectArgument(call.parts.map((part) => part.text), shape.path);
}

// Decides whether a `buildString { … }` block is this gate's business. Kotlin
// uses buildString for plenty that is not a URL — accessibility labels, log
// lines, assertion messages — and running path analysis over those was never
// analysis, only a rewrite that happened to succeed until one of them was
// written differently.
//
// The test is positive and narrow: a block builds a path when the path is
// visible inside it, or when the value it produces reaches a request helper in
// that helper's path argument, either inline or through the local constant it
// is assigned to. Everything it admits is then held to the full standard — a
// block that lands here and cannot be folded throws rather than passing, which
// is the point of the guard. Everything it rejects is left byte-for-byte alone,
// and the scans downstream only ever read /api/v1 literals, so an untouched
// diagnostic block contributes nothing either way.
function buildsApiPath(flat, marker, open, close, tree) {
  for (const literal of stringLiterals(flat.slice(open + 1, close))) {
    if (literal.body.includes("api/v1")) return true;
  }
  const calls = tree?.calls;
  if (calls === undefined) return false;

  // Inline: `request(baseUrl, "GET", buildString { … }, token, null)`.
  if (isPathArgument(flat, marker, calls)) return true;

  // Named: `val path = buildString { … }` issued further down the same block.
  // This mirrors how a stored path constant is resolved, so the two agree about
  // which names are paths.
  const before = flat.slice(0, marker).replace(/\s+$/, "");
  const assignment = /(?:\bval|\bvar|\blet|\bconst)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?::[^=]*)?=\s*$/.exec(before);
  if (assignment === null) return false;
  const block = enclosingBlock(flat, marker);
  const uses = new RegExp(`\\b${assignment[1]}\\b`, "g");
  for (const use of flat.slice(block.start, block.end).matchAll(uses)) {
    const offset = block.start + use.index;
    if (offset >= marker && offset <= close) continue;
    if (isPathArgument(flat, offset, calls)) return true;
  }
  return false;
}

// `buildString { append(a) append(b) }` is a concatenation written as statements.
// Rewriting it to `a + b` lets one folding routine handle every client. The
// rewrite is padded back to the original width so reported line numbers stay
// exact for everything downstream of it. Only blocks `buildsApiPath` recognizes
// are rewritten; the rest of a source file's buildString blocks are none of this
// gate's concern and are passed through untouched.
function foldBuildStrings(flat, file, tree) {
  let result = flat;
  let searchFrom = 0;
  let folded = 0;
  // Folding only ever replaces a block with something no longer than it was and
  // never writes a fresh `buildString {`, so the blocks present at the start
  // bound the number of iterations and this cannot spin.
  const blocks = result.split("buildString {").length - 1;
  for (let guard = 0; guard <= blocks; guard += 1) {
    const marker = result.indexOf("buildString {", searchFrom);
    if (marker === -1) return result;
    const open = result.indexOf("{", marker);
    const close = matchingClose(result, open);
    if (close === -1) throw new Error(`${file}: unterminated buildString block`);
    if (!buildsApiPath(result, marker, open, close, tree)) {
      // Step over the marker only, so a path block nested inside an unrelated
      // one is still judged on its own.
      searchFrom = marker + "buildString".length;
      continue;
    }
    const body = result.slice(open + 1, close);
    const pieces = [];
    for (const match of body.matchAll(/\bappend\(/g)) {
      const argumentOpen = match.index + match[0].length - 1;
      const argumentClose = matchingClose(body, argumentOpen);
      if (argumentClose === -1) throw new Error(`${file}: unterminated append() inside buildString`);
      pieces.push(body.slice(argumentOpen + 1, argumentClose));
    }
    if (pieces.length === 0) {
      throw new Error(`${file}: buildString block building an API path with no append() calls; the path extractor is stale`);
    }
    const width = close + 1 - marker;
    const replacement = pieces.join(" + ");
    if (replacement.length > width) throw new Error(`${file}: buildString rewrite cannot preserve source offsets`);
    result = `${result.slice(0, marker)}${replacement.padEnd(width, " ")}${result.slice(close + 1)}`;
    searchFrom = marker + width;
    folded += 1;
    if (folded > 64) throw new Error(`${file}: more API path buildString blocks than this gate will fold; the path extractor is stale`);
  }
  throw new Error(`${file}: more buildString blocks than this gate will fold; the path extractor is stale`);
}

// Folds one path expression to a template. Unresolvable operands become a single
// `{}` placeholder, which the router matcher only accepts where a route really
// has a segment there — a bogus suffix therefore cannot hide behind one.
function foldExpression(text, tree, variables) {
  const pieces = [];
  let index = 0;
  const skipSpace = () => {
    while (index < text.length && text[index] === " ") index += 1;
  };
  const readOperand = () => {
    skipSpace();
    if (index >= text.length) return "{}";
    const literal = stringLiteralAt(text, index);
    if (literal !== null) {
      index = literal.end;
      return literal.body.replaceAll(tree.interpolation, "{}");
    }
    if (text[index] in closers) {
      const close = matchingClose(text, index);
      if (close === -1) return "{}";
      const inner = text.slice(index + 1, close);
      index = close + 1;
      return text[close] === ")" ? foldExpression(inner, tree, variables) : "{}";
    }
    const identifier = /^[A-Za-z_$][A-Za-z0-9_$]*/.exec(text.slice(index));
    if (identifier === null) {
      index += 1;
      return "{}";
    }
    const name = identifier[0];
    index += name.length;
    let suffix = false;
    while (index < text.length && (text[index] === "." || text[index] === "(" || text[index] === "?" || text[index] === "!")) {
      if (text[index] in closers) {
        const close = matchingClose(text, index);
        if (close === -1) break;
        index = close + 1;
      } else index += 1;
      suffix = true;
      const chained = /^[A-Za-z_$][A-Za-z0-9_$]*/.exec(text.slice(index));
      if (chained !== null) index += chained[0].length;
    }
    if (!suffix && variables.has(name)) return variables.get(name);
    return "{}";
  };
  pieces.push(readOperand());
  while (true) {
    skipSpace();
    if (text[index] !== "+") break;
    index += 1;
    pieces.push(readOperand());
  }
  skipSpace();
  return pieces.join("");
}

function normalizeTemplate(template) {
  const withoutQuery = template.split("?")[0].split("#")[0];
  // An absolute URL still names a path on the node, and the path is the half
  // this contract covers. Dropping a real origin means such a literal is held
  // against the router instead of being reported as unresolvable. The origin has
  // to be a literal scheme and host: a folded `{}` never matches, so an
  // unresolved prefix cannot dress itself up as a URL to get its suffix ignored.
  const origin = /^[A-Za-z][A-Za-z0-9+.-]*:\/\/[^/]+/.exec(withoutQuery);
  const path = origin === null ? withoutQuery : withoutQuery.slice(origin[0].length);
  return path.startsWith("/") ? path : `/${path}`;
}

// --------------------------------------------------------------- route tables

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
    const path = line.match(/^ {2}(\/api\/v1[^:]*):\s*$/)?.[1];
    if (path !== undefined) {
      currentPath = path;
      continue;
    }
    const method = line.match(/^ {4}([a-z]+):\s*$/)?.[1];
    if (currentPath !== null && method !== undefined && specMethods.has(method)) {
      operations.add(operationKey(method, currentPath));
    }
    if (/^[^ ]/.test(line)) currentPath = null;
  }
  return operations;
}

// ------------------------------------------------------ OpenAPI $ref integrity
//
// Five responses on POST /api/v1/claim pointed at #/components/schemas/Error, a
// schema that has never existed under that name; the error envelope is called
// ApiError, and every other error response in the document already says so. This
// gate read the very same file to extract operations and never noticed, because
// it only ever looked at two indentation levels and never asked whether the
// document refers to itself coherently.
//
// `redocly lint` does catch this, and CI already runs it on the next line of
// this same job, so the tool was never missing. What was missing is that the
// contract gate trusted a document whose internal coherence it never checked,
// and reported success on it. Shelling out to redocly from here would make an
// otherwise hermetic, offline, dependency-free gate require the network in order
// to run at all, and would duplicate the adjacent CI step rather than close the
// hole in this one. So the document is resolved in process, and redocly stays
// where it is as the broader schema validator.
//
// The reader below models YAML only as far as node identity -- mappings,
// sequences, block scalars, quoted keys -- which is all that resolving a JSON
// pointer requires. Being bespoke, it is not trusted on its own word: it is held
// against two facts derived independently of it, so a reader that silently
// degrades fails the build instead of quietly approving everything.

function encodePointerToken(key) {
  return key.replaceAll("~", "~0").replaceAll("/", "~1");
}

function decodePointerToken(token) {
  return token.replaceAll("~1", "/").replaceAll("~0", "~");
}

function normalizePointer(pointer) {
  const tokens = pointer.split("/").slice(1);
  if (tokens.length === 1 && tokens[0] === "") return "";
  return tokens.map((token) => `/${encodePointerToken(decodePointerToken(token))}`).join("");
}

// Returns the mapping key a line opens, or null when the line is not a mapping
// entry at all (a bare scalar in a sequence, say). Quoted keys are read as
// written, so "200" and 200 stay distinguishable the way a pointer sees them.
function readYamlKey(text) {
  if (text.startsWith('"')) {
    let index = 1;
    let key = "";
    while (index < text.length) {
      const character = text[index];
      if (character === "\\" && index + 1 < text.length) {
        key += text[index + 1];
        index += 2;
        continue;
      }
      if (character === '"') break;
      key += character;
      index += 1;
    }
    if (text[index] !== '"') return null;
    const after = text.slice(index + 1);
    if (!after.startsWith(":")) return null;
    return { key, rest: after.slice(1).trim() };
  }
  const separator = text.search(/:(\s|$)/);
  if (separator === -1) return null;
  return { key: text.slice(0, separator), rest: text.slice(separator + 1).trim() };
}

function yamlScalar(text) {
  if (text.length >= 2 && text.startsWith('"') && text.endsWith('"')) return text.slice(1, -1);
  if (text.length >= 2 && text.startsWith("'") && text.endsWith("'")) return text.slice(1, -1);
  return text;
}

// Walks the document and records the JSON pointer of every node in it, plus
// every $ref node with the line it sits on. Anything the reader cannot model is
// reported rather than skipped, so an unfamiliar construct fails the gate.
function documentStructure(source, file) {
  const problems = [];
  const nodes = new Set([""]);
  const references = [];
  const lines = source.split(/\r?\n/);
  // Each frame is an open container; `indent` is the column its entries start
  // at, and is null until the first entry fixes it.
  const frames = [{ indent: 0, path: "", kind: "map", nextIndex: 0 }];
  let blockScalarIndent = null;

  for (let number = 1; number <= lines.length; number += 1) {
    const line = lines[number - 1];
    if (blockScalarIndent !== null) {
      if (line.trim() === "") continue;
      if (line.search(/\S/) > blockScalarIndent) continue;
      blockScalarIndent = null;
    }
    if (line.trim() === "") continue;
    const indent = line.search(/\S/);
    let rest = line.slice(indent);
    if (rest.startsWith("#")) continue;
    if (rest === "---" || rest === "...") {
      problems.push(`${file}:${number}: multi-document YAML is not modelled by this gate`);
      continue;
    }

    // A container whose indent is still unknown adopts the first entry indented
    // past its parent; anything shallower means the container was empty.
    while (frames.length > 1 && frames[frames.length - 1].indent === null) {
      const parent = frames[frames.length - 2];
      if (indent > parent.indent) {
        frames[frames.length - 1].indent = indent;
        break;
      }
      frames.pop();
    }
    while (frames.length > 1 && indent < frames[frames.length - 1].indent) frames.pop();
    const frame = frames[frames.length - 1];
    if (indent !== frame.indent) {
      problems.push(`${file}:${number}: indentation ${indent} does not line up with the enclosing block at ${frame.indent}`);
      continue;
    }

    let entryIndent = indent;
    if (/^-(\s|$)/.test(rest)) {
      if (frame.kind === "map" && frame.path !== "" && frame.nextIndex === 0) frame.kind = "seq";
      if (frame.kind !== "seq") {
        problems.push(`${file}:${number}: a sequence entry appears where a mapping was expected`);
        continue;
      }
      const entryPath = `${frame.path}/${frame.nextIndex}`;
      frame.nextIndex += 1;
      nodes.add(entryPath);
      const afterDash = rest.slice(1);
      const offset = afterDash.length - afterDash.trimStart().length;
      rest = afterDash.trimStart();
      if (rest === "") {
        frames.push({ indent: null, path: entryPath, kind: "map", nextIndex: 0 });
        continue;
      }
      // A scalar entry is a leaf, and was recorded above.
      if (readYamlKey(rest) === null) continue;
      entryIndent = indent + 1 + offset;
      frames.push({ indent: entryIndent, path: entryPath, kind: "map", nextIndex: 0 });
    } else if (frame.kind === "seq") {
      problems.push(`${file}:${number}: a mapping entry appears where a sequence was expected`);
      continue;
    }

    const parsed = readYamlKey(rest);
    if (parsed === null) {
      problems.push(`${file}:${number}: this gate cannot read ${JSON.stringify(rest)} as a mapping entry`);
      continue;
    }
    const holder = frames[frames.length - 1];
    holder.nextIndex += 1;
    const path = `${holder.path}/${encodePointerToken(parsed.key)}`;
    nodes.add(path);

    if (parsed.key === "$ref") {
      references.push({ pointer: yamlScalar(parsed.rest), line: number });
      continue;
    }
    if (parsed.rest === "") {
      frames.push({ indent: null, path, kind: "map", nextIndex: 0 });
      continue;
    }
    if (/^[|>][-+0-9]*$/.test(parsed.rest)) {
      blockScalarIndent = entryIndent;
      continue;
    }
    // Anything else is a scalar or a flow collection: a leaf, for pointers.
  }

  return { nodes, references, problems };
}

function checkOpenapiReferences(source, documented) {
  const { nodes, references, problems } = documentStructure(source, openapiFile);
  for (const problem of problems) fail(problem);

  // First independent fact: every line in the file that writes a $ref key must
  // have become a reference node. If the reader ever loses one -- swallowed by a
  // block scalar, dropped by an unfamiliar shape -- the pointer on it would go
  // unchecked, so the discrepancy is named line by line instead of tolerated.
  const seen = new Set(references.map((reference) => reference.line));
  const lines = source.split(/\r?\n/);
  for (let number = 1; number <= lines.length; number += 1) {
    if (/^\s*(- )?\$ref:/.test(lines[number - 1]) && !seen.has(number)) {
      fail(`${openapiFile}:${number}: this line writes a $ref the structural reader did not resolve`);
    }
  }

  // Second independent fact: the operations this reader sees and the operations
  // the line-based extractor sees must be the same set. The two parse the file
  // by unrelated means, so agreement is evidence and divergence is a bug in one
  // of them -- either way the build stops rather than guessing which.
  const structural = new Set();
  for (const node of nodes) {
    const match = /^\/paths\/([^/]+)\/([a-z]+)$/.exec(node);
    if (match === null || !specMethods.has(match[2])) continue;
    const path = decodePointerToken(match[1]);
    if (path.startsWith("/api/v1")) structural.add(operationKey(match[2], path));
  }
  for (const operation of documented) {
    if (!structural.has(operation)) fail(`${openapiFile}: the structural reader lost the documented operation ${operation}`);
  }
  for (const operation of structural) {
    if (!documented.has(operation)) fail(`${openapiFile}: the structural reader invented the operation ${operation}`);
  }

  for (const reference of references) {
    const pointer = reference.pointer;
    if (pointer === "") {
      fail(`${openapiFile}:${reference.line}: $ref names no pointer`);
      continue;
    }
    if (!pointer.startsWith("#/")) {
      // Nothing in this contract lives outside the document, and a pointer this
      // gate cannot follow is not a pointer this gate may approve.
      fail(`${openapiFile}:${reference.line}: $ref "${pointer}" is not a document-internal JSON pointer`);
      continue;
    }
    if (!nodes.has(normalizePointer(pointer))) {
      fail(`${openapiFile}:${reference.line}: $ref "${pointer}" resolves to nothing in this document`);
    }
  }

  return references.length;
}

// A client segment matches a route segment when either side is a placeholder or
// the two literals are equal. Segment counts must agree: there is no prefix rule,
// so a literal appended past the end of a route can never be satisfied.
// A client placeholder came from a value the caller substitutes, so it may stand
// against a route parameter (`plans/{plan_id}`) or against a closed set of
// literal routes (`confirm/${side}`). A client *literal* is the opposite: the
// caller names that segment outright, so at a real request site it must name a
// segment the router really has. Letting a client literal slide into a route
// parameter is exactly what would let `pair/network/start-ghost` hide behind
// `pair/network/{pairing_id}`. Mock assertions are exempt, because they legibly
// hard-code sample identifiers.
function segmentsMatch(clientSegments, routeSegments, literalMayFillParameter) {
  if (clientSegments.length !== routeSegments.length) return false;
  return clientSegments.every((segment, position) => {
    const route = routeSegments[position];
    if (segment === route) return true;
    if (segment === "{}") return true;
    return route === "{}" && literalMayFillParameter;
  });
}

function servedMethods(path, routeTable, literalMayFillParameter) {
  const segments = path.split("/").filter(Boolean);
  const methods = new Set();
  for (const [routePath, routeMethods] of routeTable) {
    if (segmentsMatch(segments, routePath.split("/").filter(Boolean), literalMayFillParameter)) {
      for (const method of routeMethods) methods.add(method);
    }
  }
  return methods;
}

// ------------------------------------------------------------- client scanning

function treeFiles(tree, tracked) {
  const absolute = resolve(repositoryRoot, tree.directory);
  let stats;
  try {
    stats = statSync(absolute);
  } catch {
    throw new Error(`${tree.label}: ${tree.directory} does not exist; the client tree moved and this gate is stale`);
  }
  if (!stats.isDirectory()) throw new Error(`${tree.label}: ${tree.directory} is not a directory`);
  const files = tracked.filter(
    (path) =>
      path.startsWith(`${tree.directory}/`) &&
      path.endsWith(tree.extension) &&
      !(tree.exclude ?? []).some((excluded) => path.startsWith(`${excluded}/`)),
  );
  if (files.length === 0 && tree.mayBeEmpty !== true) {
    throw new Error(`${tree.label}: no tracked ${tree.extension} files under ${tree.directory}; the extractor is stale`);
  }
  return files;
}

function argumentLabel(text) {
  return /^\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*[:=](?![=])/.exec(text)?.[1] ?? null;
}

function argumentValue(text) {
  const label = argumentLabel(text);
  return label === null ? text.trim() : text.slice(text.indexOf(label) + label.length).replace(/^\s*[:=]/, "").trim();
}

function selectArgument(argumentTexts, slot) {
  if (slot.label !== undefined) {
    const labelled = argumentTexts.findIndex((text) => argumentLabel(text) === slot.label);
    if (labelled !== -1) return labelled;
  }
  if (slot.index !== undefined) {
    if (argumentTexts.some((text) => argumentLabel(text) !== null) && slot.label !== undefined) return -1;
    return slot.index;
  }
  return -1;
}

// Resolves the call a path expression sits in: the callee name and the
// comma-split argument list. Declarations are not calls and are reported as such.
function enclosingCall(flat, offset) {
  const open = enclosingOpen(flat, offset);
  if (open === null || open.character !== "(") return null;
  const close = matchingClose(flat, open.index);
  if (close === -1) return null;
  const prefix = flat.slice(0, open.index);
  const callee = /([A-Za-z_$][A-Za-z0-9_$]*)\s*$/.exec(prefix)?.[1];
  if (callee === undefined) return null;
  const beforeCallee = prefix.slice(0, prefix.length - callee.length);
  if (/\b(?:fun|func|function)\s*$/.test(beforeCallee)) return { declaration: true, callee };
  const parts = splitTopLevel(flat.slice(open.index + 1, close));
  return {
    declaration: false,
    callee,
    open: open.index,
    close,
    parts,
    indexOf(target) {
      return parts.findIndex((part) => {
        const start = open.index + 1 + part.start;
        return target >= start && target < start + part.text.length;
      });
    },
  };
}

function enclosingBlock(flat, offset) {
  const stack = [];
  let index = 0;
  while (index < offset) {
    const literal = stringLiteralAt(flat, index);
    if (literal !== null && literal.end <= offset) {
      index = literal.end;
      continue;
    }
    const character = flat[index];
    if (character === "{") stack.push(index);
    else if (character === "}") stack.pop();
    index += 1;
  }
  if (stack.length === 0) return { start: 0, end: flat.length };
  const start = stack[stack.length - 1];
  const close = matchingClose(flat, start);
  return { start, end: close === -1 ? flat.length : close };
}

function resolveMethod(tree, shape, call, location, path) {
  const spec = shape.method;
  if (spec.kind === "handoff") return "HANDOFF";
  if (spec.kind === "derived") return deriveMethod(tree, spec.derivation);
  const index = selectArgument(call.parts.map((part) => part.text), spec);
  const raw = index >= 0 && index < call.parts.length ? argumentValue(call.parts[index].text) : null;
  if (spec.kind === "options") {
    // No options object at all means the helper's own default verb applies. An
    // options object that is spread, aliased or built elsewhere does not: the
    // verb is then unknowable here, and guessing the default would let a POST
    // masquerade as a GET.
    if (raw === null || raw === "") return deriveMethod(tree, spec.default);
    if (!raw.startsWith("{") || !raw.endsWith("}")) {
      fail(`${location}: the request options for ${path} are ${raw}, not an object literal; the method cannot be statically resolved`);
      return null;
    }
    const method = /\bmethod:\s*"([A-Za-z]+)"/.exec(raw)?.[1];
    if (method !== undefined) {
      const resolved = method.toUpperCase();
      if (!httpMethods.has(resolved)) {
        fail(`${location}: ${resolved} is not an HTTP method`);
        return null;
      }
      return resolved;
    }
    if (/\bmethod\b/.test(raw) || raw.includes("...")) {
      fail(`${location}: the request options for ${path} carry a method this gate cannot read; name the verb as a literal`);
      return null;
    }
    return deriveMethod(tree, spec.default);
  }
  if (raw === null || raw === "") {
    if (spec.default !== undefined) return deriveMethod(tree, spec.default);
    fail(`${location}: ${shape.name ?? call.callee}() call for ${path} names no method argument`);
    return null;
  }
  const literal = /^"([A-Za-z]+)"$/.exec(raw);
  if (literal === null) {
    fail(`${location}: the method argument for ${path} is ${raw}, which is not a literal; it cannot be statically resolved`);
    return null;
  }
  const method = literal[1].toUpperCase();
  if (!httpMethods.has(method)) {
    fail(`${location}: ${method} is not an HTTP method`);
    return null;
  }
  return method;
}

function scanRequestTree(tree, tracked) {
  const references = [];
  for (const check of tree.wiring ?? []) matchAtLeastOnce(check, `${tree.label}: ${check.description}`);
  for (const file of treeFiles(tree, tracked)) {
    const raw = loadFlat(file);
    const flat = foldBuildStrings(raw.flat, file, tree);
    const lineAt = (offset) => raw.lineAt(Math.min(offset, raw.flat.length - 1));
    const variables = new Map();
    const pending = [];

    for (const literal of stringLiterals(flat)) {
      if (!literal.body.includes("api/v1")) continue;
      const location = `${file}:${lineAt(literal.start)}`;

      // Expand across any concatenation the literal takes part in, so the whole
      // built path is judged rather than the first fragment of it.
      let start = literal.start;
      const before = flat.slice(0, start).replace(/\s+$/, "");
      if (before.endsWith("+")) {
        fail(`${location}: the path expression for ${literal.body} is built onto an unresolved prefix; declare the whole path in one place`);
        continue;
      }
      let end = literal.end;
      while (true) {
        const rest = flat.slice(end);
        const operator = /^\s*\+/.exec(rest);
        if (operator === null) break;
        let cursor = end + operator[0].length;
        while (flat[cursor] === " ") cursor += 1;
        const next = stringLiteralAt(flat, cursor);
        if (next !== null) {
          end = next.end;
          continue;
        }
        const identifier = /^[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*/.exec(flat.slice(cursor));
        if (identifier === null) break;
        let after = cursor + identifier[0].length;
        if (flat[after] === "(") {
          const close = matchingClose(flat, after);
          if (close === -1) break;
          after = close + 1;
        }
        end = after;
      }
      const expression = flat.slice(start, end);
      const template = normalizeTemplate(foldExpression(expression, tree, variables));
      if (!/^\/api\/v1\//.test(template)) {
        fail(`${location}: the path expression ${expression} folds to ${template}, which is not a resolvable /api/v1 path`);
        continue;
      }

      // Context 1: a comparison against a stored path constant.
      const trailing = flat.slice(end).replace(/^\s+/, "");
      if (/[=!]=[=]?\s*$/.test(before) || /^[=!]=[=]?/.test(trailing)) {
        references.push({ tree, location, template, method: "HANDOFF" });
        continue;
      }

      // Context 2: assignment to a local constant, resolved where it is used.
      const assignment = /(?:\bval|\bvar|\blet|\bconst)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?::[^=]*)?=\s*$/.exec(before);
      if (assignment !== null) {
        variables.set(assignment[1], template);
        pending.push({ name: assignment[1], template, location, start, end });
        continue;
      }

      // Context 3: an argument of a recognized request helper.
      const call = enclosingCall(flat, start);
      if (call === null || call.declaration === true) {
        fail(`${location}: the path literal ${template} is not part of a recognized API call; teach this gate the new call shape`);
        continue;
      }
      const shape = tree.calls[call.callee];
      if (shape === undefined) {
        fail(`${location}: ${call.callee}() is not a recognized API call shape for the ${tree.label}; teach this gate about it`);
        continue;
      }
      const argumentTexts = call.parts.map((part) => part.text);
      const expected = selectArgument(argumentTexts, shape.path);
      if (call.indexOf(start) !== expected) {
        fail(`${location}: ${template} appears in argument ${call.indexOf(start)} of ${call.callee}(), which is not its path argument`);
        continue;
      }
      const method = resolveMethod(tree, { ...shape, name: call.callee }, call, location, template);
      if (method !== null) references.push({ tree, location, template, method });
    }

    // A stored path constant only counts once this gate can see the verb it is
    // eventually sent with.
    for (const constant of pending) {
      const block = enclosingBlock(flat, constant.start);
      let consumed = 0;
      const uses = new RegExp(`\\b${constant.name}\\b`, "g");
      for (const use of flat.slice(block.start, block.end).matchAll(uses)) {
        const offset = block.start + use.index;
        if (offset >= constant.start && offset < constant.end) continue;
        const call = enclosingCall(flat, offset);
        if (call === null || call.declaration === true) continue;
        const shape = tree.calls[call.callee];
        if (shape === undefined) continue;
        const slot = selectArgument(call.parts.map((part) => part.text), shape.path);
        if (call.indexOf(offset) !== slot) continue;
        const location = `${file}:${lineAt(offset)}`;
        // Fold the whole argument, not just the constant: `BASE + "network/start"`
        // is a different path from `BASE`, and judging only the constant is how a
        // concatenated suffix would slip past.
        const template = normalizeTemplate(foldExpression(argumentValue(call.parts[slot].text), tree, variables));
        if (!/^\/api\/v1\//.test(template)) {
          fail(`${location}: the path argument ${call.parts[slot].text.trim()} folds to ${template}, which is not a resolvable /api/v1 path`);
          consumed += 1;
          continue;
        }
        const method = resolveMethod(tree, { ...shape, name: call.callee }, call, location, template);
        if (method !== null) references.push({ tree, location, template, method });
        consumed += 1;
      }
      if (consumed === 0) {
        fail(`${constant.location}: the path constant ${constant.template} is never handed to a recognized request helper, so its method cannot be checked`);
      }
    }
  }
  if (references.length === 0) throw new Error(`${tree.label}: no API call sites found; the extractor is stale`);
  return references;
}

function scanAssertionTree(tree, tracked) {
  const references = [];
  for (const file of treeFiles(tree, tracked)) {
    const raw = loadFlat(file);
    const flat = foldBuildStrings(raw.flat, file, tree);
    for (const literal of stringLiterals(flat)) {
      if (!literal.body.includes("api/v1")) continue;
      const location = `${file}:${raw.lineAt(literal.start)}`;
      let end = literal.end;
      while (true) {
        const operator = /^\s*\+/.exec(flat.slice(end));
        if (operator === null) break;
        let cursor = end + operator[0].length;
        while (flat[cursor] === " ") cursor += 1;
        const next = stringLiteralAt(flat, cursor);
        if (next !== null) {
          end = next.end;
          continue;
        }
        const identifier = /^[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*/.exec(flat.slice(cursor));
        if (identifier === null) break;
        end = cursor + identifier[0].length;
      }
      const template = normalizeTemplate(foldExpression(flat.slice(literal.start, end), tree, new Map()));
      if (!/^\/api\/v1\//.test(template)) {
        fail(`${location}: the asserted path ${template} is not a resolvable /api/v1 path`);
        continue;
      }
      references.push({ tree, location, template, method: null });
    }
  }
  if (references.length === 0 && tree.mayBeEmpty !== true) {
    throw new Error(`${tree.label}: no API path literals found; the extractor is stale`);
  }
  return references;
}

// Proves the tree list above is complete: anything else that names the API
// prefix is an undeclared client until someone says otherwise.
function classifyRepository(tracked) {
  for (const path of tracked) {
    let contents;
    try {
      contents = readFileSync(resolve(repositoryRoot, path), "utf8");
    } catch {
      continue;
    }
    if (!contents.includes("api/v1")) continue;
    const tree = clientTrees.find(
      (candidate) =>
        path.startsWith(`${candidate.directory}/`) &&
        !(candidate.exclude ?? []).some((excluded) => path.startsWith(`${excluded}/`)),
    );
    if (tree !== undefined) {
      if (!path.endsWith(tree.extension)) {
        fail(`${path} names the API but is not a ${tree.extension} file, so the ${tree.label} extractor never reads it`);
      }
      continue;
    }
    if (nonClientRegions.some((region) => region.match(path))) continue;
    fail(`${path} calls the API but belongs to no declared client tree; add it to clientTrees in this gate before shipping it`);
  }
}

// ------------------------------------------------------------------------ main

try {
  const tracked = trackedFiles();
  const runtime = routerOperations(readFileSync(resolve(repositoryRoot, routerFile), "utf8"));
  const openapiSource = readFileSync(resolve(repositoryRoot, openapiFile), "utf8");
  const documented = openapiOperations(openapiSource);
  if (runtime.size === 0 || documented.size === 0) {
    throw new Error("API route extraction unexpectedly returned no operations");
  }
  const resolvedReferences = checkOpenapiReferences(openapiSource, documented);
  if (resolvedReferences === 0) {
    throw new Error("the OpenAPI document yielded no $ref nodes, so reference resolution proved nothing");
  }

  const routeTable = new Map();
  for (const operation of runtime) {
    const [method, path] = operation.split(" ");
    if (!routeTable.has(path)) routeTable.set(path, new Set());
    routeTable.get(path).add(method);
  }

  classifyRepository(tracked);

  const references = [];
  for (const tree of clientTrees) {
    references.push(...(tree.kind === "request" ? scanRequestTree(tree, tracked) : scanAssertionTree(tree, tracked)));
  }

  const issued = new Set(
    references.filter((reference) => reference.method !== null && reference.method !== "HANDOFF").map((reference) => reference.template),
  );
  const unrouted = [];
  for (const reference of references) {
    const methods = servedMethods(reference.template, routeTable, reference.tree.kind === "assertion");
    if (methods.size === 0) {
      unrouted.push(`${reference.method ?? "ANY"} ${reference.template} (${reference.tree.label}, ${reference.location}): no router path matches`);
      continue;
    }
    if (reference.method === "HANDOFF") {
      if (!issued.has(reference.template)) {
        unrouted.push(
          `${reference.template} (${reference.tree.label}, ${reference.location}): stored as a path constant but the client never issues it, so its method is unverifiable`,
        );
      }
      continue;
    }
    if (reference.method !== null && !methods.has(reference.method)) {
      unrouted.push(
        `${reference.method} ${reference.template} (${reference.tree.label}, ${reference.location}): the router serves only ${[...methods].sort().join(", ")} there`,
      );
    }
  }

  const undocumented = [...runtime].filter((operation) => !documented.has(operation)).sort();
  const unimplemented = [...documented].filter((operation) => !runtime.has(operation)).sort();

  if (undocumented.length > 0) console.error(`runtime operations absent from OpenAPI:\n${undocumented.join("\n")}`);
  if (unimplemented.length > 0) console.error(`OpenAPI operations absent from runtime:\n${unimplemented.join("\n")}`);
  if (unrouted.length > 0) console.error(`client calls the Axum router does not serve:\n${unrouted.sort().join("\n")}`);
  for (const failure of failures.sort()) console.error(failure);

  if (undocumented.length > 0 || unimplemented.length > 0 || unrouted.length > 0 || failures.length > 0) {
    process.exit(1);
  }

  const verbChecked = references.filter((reference) => reference.method !== null && reference.method !== "HANDOFF").length;
  const operations = new Set(
    references
      .filter((reference) => reference.method !== null && reference.method !== "HANDOFF")
      .map((reference) => `${reference.method} ${reference.template}`),
  );
  console.log(`OpenAPI/runtime route coverage: ${runtime.size} operations match`);
  console.log(`OpenAPI reference integrity: ${resolvedReferences} $ref pointers resolve within the document`);
  console.log(
    `Client/runtime route coverage: ${operations.size} distinct client operations from ${verbChecked} verb-checked call sites across ${clientTrees.length} trees are routed`,
  );
} catch (error) {
  console.error(`route contract check failed: ${error.message}`);
  process.exit(1);
}
