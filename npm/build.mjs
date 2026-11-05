#!/usr/bin/env node
// Builds the npm packages from the release archives:
//
//   node npm/build.mjs <dir with aycod-<target>.tar.gz> <out dir> [--partial]
//
// Writes <out>/aycod (the @ay-code/cli launcher) and one <out>/aycod-<platform> per binary.
// The version comes from Cargo.toml, so the npm and Rust versions can never drift.
// --partial skips missing archives (local tests); the release workflow requires all of them.

import { execFileSync } from "node:child_process";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..");
const { PLATFORMS, packageName } = createRequire(import.meta.url)("./aycod/lib/platform.js");

const args = process.argv.slice(2);
const partial = args.includes("--partial");
const [dist, out] = args.filter((a) => !a.startsWith("--")).map((a) => resolve(a));
if (!dist || !out) {
  console.error("usage: node npm/build.mjs <archives dir> <out dir> [--partial]");
  process.exit(2);
}

const cargo = readFileSync(join(repo, "Cargo.toml"), "utf8");
const section = cargo.split(/^\[/m).find((s) => s.startsWith("package]"));
const version = section?.match(/^version = "([^"]+)"/m)?.[1];
if (!version) throw new Error("no version in Cargo.toml [package]");

const main = JSON.parse(readFileSync(join(here, "aycod", "package.json"), "utf8"));
const licenses = ["LICENSE-MIT", "LICENSE-APACHE"];
const json = (v) => `${JSON.stringify(v, null, 2)}\n`;

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
const built = [];

for (const [key, p] of Object.entries(PLATFORMS)) {
  const archive = join(dist, `aycod-${p.target}.tar.gz`);
  if (!existsSync(archive)) {
    if (partial) continue;
    throw new Error(`missing ${archive}`);
  }
  const name = packageName(p.suffix);
  const dir = join(out, `aycod-${p.suffix}`);
  const tmp = mkdtempSync(join(tmpdir(), "aycod-npm-"));
  execFileSync("tar", ["-xzf", archive, "-C", tmp]);
  mkdirSync(join(dir, "bin"), { recursive: true });
  cpSync(join(tmp, `aycod-${p.target}`, "aycod"), join(dir, "bin", "aycod"));
  chmodSync(join(dir, "bin", "aycod"), 0o755);
  rmSync(tmp, { recursive: true, force: true });
  for (const l of licenses) cpSync(join(repo, l), join(dir, l));
  const [os, cpu] = key.split(" ");
  writeFileSync(
    join(dir, "package.json"),
    json({
      name,
      version,
      description: `The aycod binary for ${p.label}. Install @ay-code/cli instead.`,
      license: main.license,
      repository: main.repository,
      homepage: main.homepage,
      os: [os],
      cpu: [cpu],
      files: ["bin", ...licenses],
      preferUnplugged: true,
      publishConfig: main.publishConfig,
    }),
  );
  writeFileSync(
    join(dir, "README.md"),
    `# ${name}\n\nThe aycod ${version} binary for ${p.label} (Rust target \`${p.target}\`).\n\n` +
      "Do not install it directly: `npm install -g @ay-code/cli` picks the right one.\n" +
      "https://github.com/AyenaCode/aycod\n",
  );
  built.push(name);
}

if (built.length === 0) throw new Error(`no aycod-<target>.tar.gz in ${dist}`);

const dir = join(out, "aycod");
for (const f of ["bin", "lib", "README.md"]) cpSync(join(here, "aycod", f), join(dir, f), { recursive: true });
chmodSync(join(dir, "bin", "aycod"), 0o755);
for (const l of licenses) cpSync(join(repo, l), join(dir, l));
const optionalDependencies = Object.fromEntries(
  Object.values(PLATFORMS).map((p) => [packageName(p.suffix), version]),
);
writeFileSync(join(dir, "package.json"), json({ ...main, version, optionalDependencies }));

console.log(`npm packages ${version} in ${out}: ${[...built, main.name].join(", ")}`);
