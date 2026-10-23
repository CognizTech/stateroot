const assert = require("node:assert/strict");
assert.ok(process.env.STATEROOT_CLI_TEST_BINARY, "test:cli requires STATEROOT_CLI_TEST_BINARY pointing to the current cargo-built CLI");
require("./delegation-contract.test.cjs");
