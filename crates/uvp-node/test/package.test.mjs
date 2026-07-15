import assert from "node:assert/strict";
import test from "node:test";
import { semanticVersion, version } from "../index.js";

test("publishes the semantic compatibility versions through the Node package", () => {
  assert.equal(version(), "0.1.0");
  assert.equal(semanticVersion(), "uvp-semantic/0.1");
});
