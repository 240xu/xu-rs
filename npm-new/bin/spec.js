#!/usr/bin/env node
// `spec` is an alias entry point for the same xcc binary.
const { spawnSync } = require("node:child_process");
const { existsSync } = require("node:fs");
const { join, dirname } = require("node:path");

const bin = join(dirname(__dirname), "vendor", "xcc");
if (!existsSync(bin)) {
  console.error(
    "[spec] platform binary missing. Re-run install: npm rebuild @240xu/xcc"
  );
  process.exit(1);
}
const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
process.exit(r.status === null ? 1 : r.status);
