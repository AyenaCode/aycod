"use strict";
// Where the native binary of this machine lives. Used by lib/run.js and npm/build.mjs (bin/aycod mirrors the table in sh).

const SCOPE = "@ay-code";

/** `${process.platform} ${process.arch}` → npm package suffix and Rust target of the release. */
const PLATFORMS = {
  "linux x64": { suffix: "linux-x64", target: "x86_64-unknown-linux-musl", label: "Linux x64 (static)" },
  "linux arm64": { suffix: "linux-arm64", target: "aarch64-unknown-linux-musl", label: "Linux arm64 (static)" },
  "darwin arm64": { suffix: "darwin-arm64", target: "aarch64-apple-darwin", label: "macOS Apple silicon" },
  "darwin x64": { suffix: "darwin-x64", target: "x86_64-apple-darwin", label: "macOS Intel" },
};

const packageName = (suffix) => `${SCOPE}/cli-${suffix}`;

/** Absolute path of the native binary, or throws with what to do about it. */
function binaryPath() {
  const key = `${process.platform} ${process.arch}`;
  const platform = PLATFORMS[key];
  if (!platform) {
    throw new Error(
      `no prebuilt binary for ${key}. aycod runs on Linux and macOS, x64 and arm64 (Windows: not yet). ` +
        "Build it from source: https://github.com/AyenaCode/aycod#install",
    );
  }
  const name = packageName(platform.suffix);
  try {
    return require.resolve(`${name}/bin/aycod`);
  } catch {
    throw new Error(
      `${name} is missing: it is an optional dependency of @ay-code/cli. ` +
        "Reinstall without --omit=optional / --no-optional, or install the binary directly: " +
        "https://github.com/AyenaCode/aycod#install",
    );
  }
}

module.exports = { SCOPE, PLATFORMS, packageName, binaryPath };
