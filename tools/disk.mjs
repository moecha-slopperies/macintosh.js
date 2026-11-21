#!/usr/bin/env node
// Fetch and assemble Mac OS disk images for macintosh.js.
//
// The base images are downloaded separately and are not bundled in the
// repository. An assembled image is a pristine base plus an HFS overlay:
//
//   tools/disk-overlays/common/**     applied to every version
//   tools/disk-overlays/<version>/**  applied after the common overlay
//
// Each overlay has an `overlay.json` manifest listing HFS paths to delete and
// MacBinary files to copy. Files whose names start with "." are ignored.
//
// Usage:
//   node tools/disk.mjs list
//   node tools/disk.mjs fetch [--version 8.1] [--force]
//   node tools/disk.mjs assemble [--version 8.1] [--out src/basilisk/disk]
//   node tools/disk.mjs verify [--disk src/basilisk/disk]

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = join(root, "tools", "disks.json");
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
const cacheDir = join(root, ".cache", "macintosh-js-disks");
const defaultOutput = join(root, "src", "basilisk", "disk");

function usage() {
  console.log(`Usage:
  node tools/disk.mjs list
  node tools/disk.mjs fetch [--version <version>] [--force]
  node tools/disk.mjs assemble [--version <version>] [--out <path>] [--force]
  node tools/disk.mjs verify [--disk <path>]

Default version: ${manifest.default}
MACINTOSH_JS_DISK_VERSION overrides the default.`);
}

function parseArgs(argv) {
  const options = {
    command: argv[0] && !argv[0].startsWith("-") ? argv[0] : "help",
    version: process.env.MACINTOSH_JS_DISK_VERSION || manifest.default,
    output: defaultOutput,
    disk: defaultOutput,
    force: false,
  };
  const args = options.command === "help" ? argv : argv.slice(1);
  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    if (arg === "--version" || arg === "-v") {
      options.version = args[++index];
    } else if (arg.startsWith("--version=")) {
      options.version = arg.slice("--version=".length);
    } else if (arg === "--out") {
      options.output = resolve(args[++index]);
    } else if (arg.startsWith("--out=")) {
      options.output = resolve(arg.slice("--out=".length));
    } else if (arg === "--disk") {
      options.disk = resolve(args[++index]);
    } else if (arg.startsWith("--disk=")) {
      options.disk = resolve(arg.slice("--disk=".length));
    } else if (arg === "--force") {
      options.force = true;
    } else if (arg === "--help" || arg === "-h") {
      options.command = "help";
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  return options;
}

function diskSpec(version) {
  const spec = manifest.disks[version];
  if (!spec) {
    throw new Error(
      `unknown disk version ${version}; known versions: ${Object.keys(manifest.disks).join(", ")}`,
    );
  }
  return spec;
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function verifyHash(path, expected, label) {
  const actual = sha256(path);
  if (actual !== expected) {
    throw new Error(
      `${label} hash mismatch: expected ${expected}, got ${actual}`,
    );
  }
}

function run(command, args, { quiet = false } = {}) {
  try {
    return execFileSync(command, args, {
      encoding: "utf8",
      stdio: quiet ? ["ignore", "pipe", "pipe"] : "inherit",
    });
  } catch (error) {
    if (error.code === "ENOENT") {
      throw new Error(
        `${command} not found; HFS commands need hfsutils, extraction needs unzip`,
      );
    }
    const detail = quiet && error.stderr ? String(error.stderr).trim() : "";
    throw new Error(`${command} failed${detail ? `: ${detail}` : ""}`);
  }
}

function tryRun(command, args) {
  try {
    run(command, args, { quiet: true });
    return true;
  } catch {
    return false;
  }
}

function cachePaths(version) {
  return {
    zip: join(cacheDir, `${version}.zip`),
    image: join(cacheDir, `${version}.dsk`),
  };
}

async function download(url, destination) {
  console.log(`disk: downloading ${url}`);
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) {
    throw new Error(
      `download failed: HTTP ${response.status} ${response.statusText}`,
    );
  }
  writeFileSync(destination, Buffer.from(await response.arrayBuffer()));
}

function extractZip(zipPath, destination) {
  rmSync(destination, { recursive: true, force: true });
  mkdirSync(destination, { recursive: true });
  if (process.platform === "win32") {
    run("tar", ["-xf", zipPath, "-C", destination]);
  } else {
    run("unzip", ["-o", zipPath, "-d", destination], { quiet: true });
  }
}

async function fetchDisk(version, force) {
  const spec = diskSpec(version);
  const paths = cachePaths(version);
  mkdirSync(cacheDir, { recursive: true });

  if (!force && existsSync(paths.image)) {
    try {
      verifyHash(paths.image, spec.imageSha256, `${version} disk image`);
      console.log(`disk: using cached ${paths.image}`);
      return paths.image;
    } catch (error) {
      console.warn(`disk: ${error.message}`);
      rmSync(paths.image, { force: true });
    }
  }

  if (force || !existsSync(paths.zip)) {
    await download(spec.url, paths.zip);
  }
  verifyHash(paths.zip, spec.zipSha256, `${version} zip`);

  const extracted = join(cacheDir, `${version}.extract`);
  extractZip(paths.zip, extracted);
  const member = join(extracted, spec.member);
  if (!existsSync(member)) {
    throw new Error(`zip did not contain ${spec.member}`);
  }
  verifyHash(member, spec.imageSha256, `${version} disk image`);
  rmSync(paths.image, { force: true });
  renameSync(member, paths.image);
  rmSync(extracted, { recursive: true, force: true });
  console.log(`disk: prepared ${paths.image}`);
  return paths.image;
}

function ensureHfsDirectories(hfsFile) {
  const parts = hfsFile.split(":").filter(Boolean);
  let current = "";
  for (const part of parts.slice(0, -1)) {
    current += `:${part}`;
    tryRun("hmkdir", [current]);
  }
}

function applyOverlay(overlayDirectory) {
  const overlayFile = join(overlayDirectory, "overlay.json");
  if (!existsSync(overlayFile)) {
    return;
  }

  const overlay = JSON.parse(readFileSync(overlayFile, "utf8"));
  for (const target of overlay.deletes ?? []) {
    if (typeof target !== "string" || !target.startsWith(":")) {
      throw new Error(`${overlayDirectory}: invalid delete target ${target}`);
    }
    tryRun("hdel", [target]);
  }

  for (const file of overlay.files ?? []) {
    if (typeof file.source !== "string" || typeof file.target !== "string") {
      throw new Error(
        `${overlayDirectory}: each file needs a source and target`,
      );
    }
    if (!file.target.startsWith(":")) {
      throw new Error(`${overlayDirectory}: invalid HFS target ${file.target}`);
    }

    const source = resolve(overlayDirectory, file.source);
    const relativeSource = relative(overlayDirectory, source);
    if (isAbsolute(relativeSource) || relativeSource.startsWith("..")) {
      throw new Error(
        `${overlayDirectory}: source escapes overlay: ${file.source}`,
      );
    }
    if (!existsSync(source) || !statSync(source).isFile()) {
      throw new Error(
        `${overlayDirectory}: missing overlay source ${file.source}`,
      );
    }

    ensureHfsDirectories(file.target);
    tryRun("hdel", [file.target]);
    run("hcopy", ["-m", source, file.target], { quiet: true });
    console.log(`disk: overlay ${file.target}`);
  }
}

function verifyMounted(version) {
  const panels = run("hls", [":System Folder:Control Panels"], { quiet: true });
  if (!panels.includes("TCP/IP")) {
    throw new Error("assembled image has no TCP/IP control panel");
  }
  const preferences = run("hls", [":System Folder:Preferences"], {
    quiet: true,
  });
  if (!preferences.includes("TCP/IP Preferences")) {
    throw new Error("assembled image has no TCP/IP Preferences");
  }
  const browser = run("hls", ["-l", ":Internet:Netscape Navigator Gold 3.04"], {
    quiet: true,
  });
  if (
    !browser.includes("APPL/MOSS") ||
    !browser.includes("Netscape Navigator Gold 3.04")
  ) {
    throw new Error("assembled image has no Netscape Navigator Gold 3.04");
  }
  const plugins = run(
    "hls",
    [":Internet:Netscape Navigator Gold 3.04:Plug-ins"],
    {
      quiet: true,
    },
  );
  if (!plugins.includes("LiveAudio")) {
    throw new Error("assembled image has no Netscape plug-ins");
  }
  const applications = run(
    "hls",
    ["-l", ":Applications:StuffIt Expander 5.5"],
    { quiet: true },
  );
  if (
    !applications.includes("APPL/SITx") ||
    !applications.includes("StuffIt Expander")
  ) {
    throw new Error("assembled image has no StuffIt Expander app");
  }
  const root = run("hls", [":"], { quiet: true });
  if (!root.includes("Welcome to macintosh.js")) {
    throw new Error("assembled image has no welcome readme");
  }
  console.log(
    `disk: verified ${version} TCP/IP installation, browser, and default software`,
  );
}

function verifyDisk(image, version = manifest.default) {
  if (!existsSync(image)) {
    throw new Error(`disk image not found: ${image}`);
  }
  tryRun("humount");
  run("hmount", [image], { quiet: true });
  try {
    verifyMounted(version);
  } finally {
    tryRun("humount");
  }
}

async function assembleDisk(version, output, force) {
  const base = await fetchDisk(version, force);
  mkdirSync(dirname(output), { recursive: true });
  copyFileSync(base, output);
  console.log(`disk: assembling ${version} into ${output}`);

  tryRun("humount");
  run("hmount", [output], { quiet: true });
  try {
    applyOverlay(join(root, "tools", "disk-overlays", "common"));
    applyOverlay(join(root, "tools", "disk-overlays", version));
    verifyMounted(version);
  } finally {
    tryRun("humount");
  }

  console.log(`disk: wrote ${output} (${statSync(output).size} bytes)`);
  return output;
}

function listDisks() {
  for (const [version, spec] of Object.entries(manifest.disks)) {
    const marker = version === manifest.default ? "*" : " ";
    console.log(`${marker} ${version}  ${spec.description}`);
  }
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  switch (options.command) {
    case "list":
      listDisks();
      break;
    case "fetch":
      await fetchDisk(options.version, options.force);
      break;
    case "assemble":
      await assembleDisk(options.version, options.output, options.force);
      break;
    case "verify":
      verifyDisk(options.disk, options.version);
      break;
    case "help":
      usage();
      break;
    default:
      throw new Error(`unknown command: ${options.command}`);
  }
}

main().catch((error) => {
  console.error(`disk: ${error.message}`);
  process.exitCode = 1;
});
