import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const candidates = [
  join(here, "uvp_node.node"),
  join(here, "uvp-core-node.node"),
  join(here, "../../target/debug/libuvp_node.dylib"),
  join(here, "../../target/debug/libuvp_node.so"),
  join(here, "../../target/debug/uvp_node.dll")
];

let native;
for (const candidate of candidates) {
  if (existsSync(candidate)) {
    const mod = { exports: {} };
    process.dlopen(mod, candidate);
    native = mod.exports;
    break;
  }
}

if (!native) {
  throw new Error("uvp-core native module is not built; run `cargo build -p uvp-node`");
}

function unwrap(raw) {
  const envelope = JSON.parse(raw);
  if (!envelope.ok) {
    const message = envelope.diagnostics?.map((item) => item.message).join("; ") || "uvp-core error";
    throw new Error(message);
  }
  return envelope.value;
}

export function compile(request) {
  return unwrap(native.compileJson(JSON.stringify(request)));
}

export function parseHook(request) {
  return unwrap(native.parseHookJson(JSON.stringify(request)));
}

export function evaluateHook(request) {
  return unwrap(native.evalHookJson(JSON.stringify(request)));
}

export function replay(request) {
  return unwrap(native.replayJson(JSON.stringify(request)));
}

export function version() {
  return native.version();
}
