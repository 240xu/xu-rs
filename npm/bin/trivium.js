#!/usr/bin/env node
// Launcher shim: exec the platform binary fetched by install.js into ../vendor/.
const { spawnSync } = require("node:child_process");
const { existsSync } = require("node:fs");
const { join, dirname } = require("node:path");

const bin = join(dirname(__dirname), "vendor", "trivium");
if (!existsSync(bin)) {
  console.error(
    "[trivium] platform binary missing. Re-run install: npm rebuild @240xu/trivium"
  );
  process.exit(1);
}
const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
process.exit(r.status === null ? 1 : r.status);
