#!/usr/bin/env node
// Collect Tauri's bundle output into one release artifact: a .dmg on macOS,
// a .zip of the bundle directory everywhere else.

import { execFileSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const packageJson = JSON.parse(
  readFileSync(join(root, "package.json"), "utf8"),
);

function usage() {
  console.log(`Usage:
  node tools/collect-artifacts.mjs --disk-version <version> --target <triple> [--out <dir>]`);
}

function parseArgs(argv) {
  const options = {
    diskVersion: undefined,
    target: undefined,
    output: join(root, "artifacts"),
  };
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    if (arg === "--disk-version") {
      options.diskVersion = argv[++index];
    } else if (arg.startsWith("--disk-version=")) {
      options.diskVersion = arg.slice("--disk-version=".length);
    } else if (arg === "--target") {
      options.target = argv[++index];
    } else if (arg.startsWith("--target=")) {
      options.target = arg.slice("--target=".length);
    } else if (arg === "--out") {
      options.output = resolve(argv[++index]);
    } else if (arg.startsWith("--out=")) {
      options.output = resolve(arg.slice("--out=".length));
    } else if (arg === "--help" || arg === "-h") {
      usage();
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  if (!options.diskVersion) {
    throw new Error("--disk-version is required");
  }
  if (!options.target) {
    throw new Error("--target is required");
  }
  return options;
}

function bundleDirectory(target) {
  const candidates = [
    join(root, "src-tauri", "target", target, "release", "bundle"),
    join(root, "src-tauri", "target", "release", "bundle"),
  ];
  const bundle = candidates.find((candidate) => existsSync(candidate));
  if (!bundle) {
    throw new Error(`no bundle directory for ${target}; run tauri build first`);
  }
  return bundle;
}

function findDmg(directory) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      const found = findDmg(path);
      if (found) {
        return found;
      }
    } else if (entry.name.endsWith(".dmg")) {
      return path;
    }
  }
  return undefined;
}

function zipBundle(bundle, destination) {
  if (process.platform === "win32") {
    execFileSync(
      "powershell",
      [
        "-NoProfile",
        "-Command",
        `Compress-Archive -Path "${join(bundle, "*")}" -DestinationPath "${destination}" -Force`,
      ],
      { stdio: "inherit" },
    );
  } else {
    execFileSync("zip", ["-r", "-q", destination, "."], {
      cwd: bundle,
      stdio: "inherit",
    });
  }
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const bundle = bundleDirectory(options.target);
  const prefix = `macintosh.js-${packageJson.version}-${options.diskVersion}-${options.target}`;
  mkdirSync(options.output, { recursive: true });

  if (process.platform === "darwin") {
    const dmg = findDmg(bundle);
    if (!dmg) {
      throw new Error(`no .dmg found in ${bundle}; build with --bundles dmg`);
    }
    const destination = join(options.output, `${prefix}.dmg`);
    cpSync(dmg, destination);
    console.log(`artifact: ${destination}`);
    return;
  }

  const destination = join(options.output, `${prefix}.zip`);
  zipBundle(bundle, destination);
  console.log(`artifact: ${destination}`);
}

try {
  main();
} catch (error) {
  console.error(`artifacts: ${error.message}`);
  process.exitCode = 1;
}
