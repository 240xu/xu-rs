"use strict";
// Post-unpack fetcher: downloads the prebuilt xcc binary for this platform
// from GitHub Releases, verifies sha256, extracts it into ./vendor/.
// Node builtins only. Needs a system `tar` for .tar.gz extraction.
const { createWriteStream, existsSync, mkdirSync, chmodSync } = require("node:fs");
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
  "xcc-0.1.0-android-aarch64.tar.gz":
    "2c4453c5561fd265b7ff097fcee9b57504f18b7bf9825db0f60bc361f73479de",
};

// GitHub Release asset id for the BIN_VERSION tarball (from the releases API).
const ASSET_ID = "546889225";

function assetForPlatform() {
  const plat = process.platform; // 'android' on Termux, 'linux', 'darwin', 'win32'
  const arch = process.arch; // 'arm64', 'x64', ...
  if ((plat === "android" || plat === "linux") && arch === "arm64") {
    return `xcc-${BIN_VERSION}-android-aarch64.tar.gz`;
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
    console.log("[xcc] XCC_SKIP_DOWNLOAD=1, skipping binary fetch.");
    return;
  }
  const asset = assetForPlatform();
  if (!asset) {
    console.error(
      `[xcc] no prebuilt binary for ${process.platform}-${process.arch} in v${VERSION} yet. ` +
        `Build from source: https://github.com/${REPO} (cargo build --release). ` +
        `Currently shipped: android-aarch64 (Termux).`
    );
    process.exit(1);
  }
  const expected = CHECKSUMS[asset];
  if (!expected || expected === "REPLACE_WITH_SHA256") {
    console.error(`[xcc] no checksum recorded for ${asset}; refusing to install.`);
    process.exit(1);
  }
  // Fetch via the API asset endpoint: it 302s to release-assets.githubusercontent.com.
  // We do NOT use github.com/.../releases/download/... because github.com itself is
  // unreachable from some networks (China/Termux) and would hard-fail the install.
  const url = `https://api.github.com/repos/${REPO}/releases/assets/${ASSET_ID}`;
  const tmp = join(tmpdir(), asset);
  console.log(`[xcc] fetching ${asset} via GitHub API`);
  await download(url, tmp, { Accept: "application/octet-stream" });
  const sum = createHash("sha256").update(require("node:fs").readFileSync(tmp)).digest("hex");
  if (sum !== expected) {
    throw new Error(`[xcc] checksum mismatch for ${asset}: got ${sum}, want ${expected}`);
  }
  const vendor = join(__dirname, "vendor");
  mkdirSync(vendor, { recursive: true });
  // Tarball layout: <name>/xcc (+ README/LICENSE/...). Extract only the binary.
  execFileSync("tar", ["-xzf", tmp, "-C", vendor, "--strip-components=1", `${asset.replace(/\.tar\.gz$/, "")}/xcc`]);
  chmodSync(join(vendor, "xcc"), 0o755);
  console.log(`[xcc] installed ${asset} -> vendor/xcc`);
}

main().catch((e) => {
  console.error(e.message || e);
  process.exit(1);
});
