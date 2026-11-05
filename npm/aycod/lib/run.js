"use strict";
// Fallback of bin/aycod when the shell launcher cannot find the binary on disk (Yarn Plug'n'Play,
// unusual layouts): resolves it the Node way, or explains what is missing. Any failure exits 2:
// the agents treat it as "block", so a broken install never lets an action through.

const { spawnSync } = require("node:child_process");
const { binaryPath } = require("./platform.js");

let bin;
try {
  bin = binaryPath();
} catch (e) {
  process.stderr.write(`aycod: ${e.message}\n`);
  process.exit(2);
}
const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
if (r.error) {
  process.stderr.write(`aycod: cannot run ${bin}: ${r.error.message}\n`);
  process.exit(2);
}
if (r.signal) {
  process.kill(process.pid, r.signal);
}
process.exit(r.status ?? 2);
