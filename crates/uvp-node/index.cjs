const { existsSync } = require("node:fs");
const { dirname, join } = require("node:path");

const here = __dirname;
const candidates = [
  join(here, "uvp_node.node"),
  join(here, "uvp-core-node.node"),
  join(here, "../../target/debug/libuvp_node.dylib"),
  join(here, "../../target/debug/libuvp_node.so"),
  join(here, "../../target/debug/uvp_node.dll")
];

let native;
for (const candidate of candidates) {
  if (!existsSync(candidate)) {
    continue;
  }
  const mod = { exports: {} };
  process.dlopen(mod, candidate);
  native = mod.exports;
  break;
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

exports.compile = function compile(request) {
  return unwrap(native.compileJson(JSON.stringify(request)));
};

exports.parseHook = function parseHook(request) {
  return unwrap(native.parseHookJson(JSON.stringify(request)));
};

exports.evaluateHook = function evaluateHook(request) {
  return unwrap(native.evalHookJson(JSON.stringify(request)));
};

exports.replay = function replay(request) {
  return unwrap(native.replayJson(JSON.stringify(request)));
};

exports.version = function version() {
  return native.version();
};
