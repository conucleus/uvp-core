import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";
import { parseHook } from "../index.js";

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
