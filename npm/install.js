"use strict";
// Post-unpack fetcher: downloads the prebuilt trivium binary for this platform
// from GitHub Releases, verifies sha256, extracts it into ./vendor/.
// Node builtins only. Needs a system `tar` for .tar.gz extraction.
const { createWriteStream, existsSync, mkdirSync, chmodSync, unlinkSync, statSync } = require("node:fs");
const { get } = require("node:https");
const { createHash } = require("node:crypto");
const { execFileSync } = require("node:child_process");
const { tmpdir } = require("node:os");
const { join } = require("node:path");

const VERSION = require("./package.json").version;
const REPO = "240xu/xu-rs";

// Version of the *prebuilt binary* this npm package ships. Intentionally
// decoupled from VERSION: the npm wrapper can be republished (installer fixes,
// docs) without rebuilding the Rust binary. Bump only when a new GitHub Release
// asset is cut.
const BIN_VERSION = "0.1.0";

// sha256 of the release tarballs, from dist/SHA256SUMS at release time.
const CHECKSUMS = {
  "trivium-0.1.0-android-aarch64.tar.gz":
    "3e4a41efdffa7d28b84ef8b660f7b532c18e956e0dea7c17ab813f4504282038",
};

// GitHub Release asset id for the BIN_VERSION tarball (from the releases API).
const ASSET_ID = "547284461";

// Expected byte size of the tarball; guards against truncated / HTML error pages
// being accepted as a valid download.
const EXPECTED_SIZE = 4407493;

function assetForPlatform() {
  const plat = process.platform; // 'android' on Termux, 'linux', 'darwin', 'win32'
  const arch = process.arch; // 'arm64', 'x64', ...
  if ((plat === "android" || plat === "linux") && arch === "arm64") {
    return `trivium-${BIN_VERSION}-android-aarch64.tar.gz`;
  }
  return null;
}

function download(url, dest, headers = {}) {
  return new Promise((resolve, reject) => {
    get(
      url,
      { headers: { "User-Agent": "xcc-npm-installer", ...headers } },
      (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          return resolve(download(res.headers.location, dest));
        }
        if (res.statusCode !== 200) {
          res.resume();
          try { unlinkSync(dest); } catch {}
          return reject(
            new Error(`download failed: HTTP ${res.statusCode} for ${url}`)
          );
        }
        const out = createWriteStream(dest);
        res.pipe(out);
        out.on("finish", () => resolve());
        out.on("error", reject);
      }
    ).on("error", reject);
  });
}

async function main() {
  // Allow offline / pre-seeded installs (tests, vendored mirrors).
  if (process.env.XCC_SKIP_DOWNLOAD === "1") {
    console.log("[trivium] XCC_SKIP_DOWNLOAD=1, skipping binary fetch.");
    return;
  }
  const asset = assetForPlatform();
  if (!asset) {
    console.error(
      `[trivium] no prebuilt binary for ${process.platform}-${process.arch} in v${VERSION} yet. ` +
        `Build from source: https://github.com/${REPO} (cargo build --release). ` +
        `Currently shipped: android-aarch64 (Termux).`
    );
    process.exit(1);
  }
  const expected = CHECKSUMS[asset];
  if (!expected || expected === "REPLACE_WITH_SHA256") {
    console.error(`[trivium] no checksum recorded for ${asset}; refusing to install.`);
    process.exit(1);
  }
  // Prefer the CDN direct link (no rate limit). The API asset endpoint is a
  // fallback: it works when the caller supplies a token, but ANONYMOUS API
  // requests are capped at 60/hour, which will fail installs on shared IPs.
  const direct = `https://github.com/${REPO}/releases/download/v${BIN_VERSION}/${asset}`;
  const api = `https://api.github.com/repos/${REPO}/releases/assets/${ASSET_ID}`;
  const tmp = join(tmpdir(), asset);
  console.log(`[trivium] fetching ${asset}`);
  try {
    await download(direct, tmp);
  } catch (e) {
    console.log(`[trivium] direct link failed (${e.message}), trying API endpoint`);
    await download(api, tmp, { Accept: "application/octet-stream" });
  }
  const got = statSync(tmp).size;
  if (got !== EXPECTED_SIZE) {
    try { unlinkSync(tmp); } catch {}
    throw new Error(`[trivium] size mismatch for ${asset}: got ${got}, want ${EXPECTED_SIZE}`);
  }
  const sum = createHash("sha256").update(require("node:fs").readFileSync(tmp)).digest("hex");
  if (sum !== expected) {
    throw new Error(`[trivium] checksum mismatch for ${asset}: got ${sum}, want ${expected}`);
  }
  const vendor = join(__dirname, "vendor");
  mkdirSync(vendor, { recursive: true });
  // Tarball layout: <name>/bin/{trivium,xcc,spec}. Extract binary + compat aliases.
  const base = asset.replace(/\.tar\.gz$/, "");
  execFileSync("tar", [
    "-xzf", tmp, "-C", vendor, "--strip-components=2",
    `${base}/bin/trivium`, `${base}/bin/xcc`, `${base}/bin/spec`,
  ]);
  for (const b of ["trivium", "xcc", "spec"]) {
    chmodSync(join(vendor, b), 0o755);
  }
  fixBinShebangs();
  console.log(`[trivium] installed ${asset} -> vendor/{trivium,xcc,spec}`);
}

// Termux/bionic has no /usr/bin/env; termux-exec's LD_PRELOAD normally covers
// the npm bin shims, but shells without it (daemons, other sandboxes) get
// "bad interpreter: No such file or directory". Rewrite to the absolute node
// path on Android so the entry points always work.
function fixBinShebangs() {
  if (process.platform !== "android") return;
  const node = process.execPath;
  for (const b of ["trivium.js", "xcc.js", "spec.js"]) {
    const p = join(__dirname, "bin", b);
    try {
      const src = require("node:fs").readFileSync(p, "utf8");
      if (src.startsWith("#!/usr/bin/env")) {
        require("node:fs").writeFileSync(p, src.replace(/^#!.*\n/, `#!${node}\n`));
      }
    } catch {}
  }
}

main().catch((e) => {
  console.error(e.message || e);
  process.exit(1);
});
