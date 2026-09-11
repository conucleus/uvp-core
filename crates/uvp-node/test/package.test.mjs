import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";
import { lintHook, parseHook } from "../index.js";

const request = {
  profile: "cloud_compat",
  hookName: "HOOK",
  hook: "buyer::task.main.cmp"
};

test("loads the ESM and CommonJS package entry points", () => {
  const require = createRequire(import.meta.url);
  const commonjs = require("../index.cjs");

  const esmResult = parseHook(request);
  const commonjsResult = commonjs.parseHook(request);

  assert.equal(esmResult.normalizedExpression, request.hook);
  assert.deepEqual(commonjsResult, esmResult);
});

test("lintHook reports diagnostics without rejecting the legal hook", () => {
  const require = createRequire(import.meta.url);
  const commonjs = require("../index.cjs");

  const esmResult = lintHook({
    profile: "evm_strict",
    hookName: "DUP",
    hook: "buyer::task.main.cmp & task.main.cmp"
  });
  const commonjsResult = commonjs.lintHook({
    profile: "evm_strict",
    hookName: "DUP",
    hook: "buyer::task.main.cmp & task.main.cmp"
  });

  assert.deepEqual(commonjsResult, esmResult);
  assert.equal(esmResult.semanticVersion, "uvp.semantic.v1");
  assert.equal(esmResult.diagnostics.length, 1);
  assert.equal(esmResult.diagnostics[0].code, "UVP-L001");
  assert.equal(esmResult.diagnostics[0].severity, "warning");
});
