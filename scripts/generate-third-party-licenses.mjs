#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repository = resolve(scriptDirectory, "..");
const outputPath = join(repository, "THIRD_PARTY_LICENSES.txt");
const mode = process.argv[2];

if (mode !== "--write" && mode !== "--check") {
  fail("usage: node scripts/generate-third-party-licenses.mjs --write|--check");
}

const normalize = (text) => text.replaceAll("\r\n", "\n").trimEnd();
const read = (path) => normalize(readFileSync(path, "utf8"));
const compare = (left, right) => (left < right ? -1 : left > right ? 1 : 0);

function fail(message) {
  console.error(message);
  process.exit(1);
}

function rustLicenses() {
  const executable = process.env.CARGO_ABOUT || "cargo-about";
  const result = spawnSync(
    executable,
    [
      "generate",
      "--config",
      "about.toml",
      "--workspace",
      "--locked",
      "--offline",
      "--fail",
      "--format",
      "json",
    ],
    {
      cwd: repository,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    },
  );

  if (result.status !== 0) {
    if (result.error?.code === "ENOENT") {
      fail("cargo-about is required; install the pinned version 0.9.1");
    }
    process.stderr.write(result.stderr || "cargo-about failed\n");
    process.exit(result.status ?? 1);
  }

  const report = JSON.parse(result.stdout);
  const sections = report.licenses.flatMap((license) => {
    const packages = license.used_by
      .map((usage) => `${usage.crate.name} ${usage.crate.version}`)
      .filter((name) => !name.startsWith("yawm-"))
      .sort(compare);
    if (packages.length === 0) return [];
    return [
      `${license.name} (${license.id})\n${packages.map((name) => `- ${name}`).join("\n")}\n\n${normalize(
        license.text,
      )}`,
    ];
  });

  return `RUST DEPENDENCIES\n=================\n\n${sections.join(
    "\n\n-------------------------------------------------------------------------------\n\n",
  )}`;
}

function npmLicenses() {
  const desktop = join(repository, "apps/desktop");
  const lock = JSON.parse(readFileSync(join(desktop, "package-lock.json"), "utf8"));
  const overrides = JSON.parse(
    readFileSync(join(repository, "licenses/npm-overrides.json"), "utf8"),
  );
  const packages = [];

  for (const [relativePath, metadata] of Object.entries(lock.packages)) {
    if (!relativePath.startsWith("node_modules/") || metadata.dev === true) continue;

    const directory = join(desktop, relativePath);
    let manifest;
    try {
      manifest = JSON.parse(readFileSync(join(directory, "package.json"), "utf8"));
    } catch {
      fail(`npm package ${relativePath} is not installed; run npm ci in apps/desktop`);
    }

    const key = `${manifest.name}@${manifest.version}`;
    const override = overrides.packages[key];
    let licenseFiles;

    if (override) {
      licenseFiles = [resolve(repository, override)];
    } else {
      licenseFiles = readdirSync(directory)
        .filter((name) => /^(licen[cs]e|copying|notice)(?:$|[._-])/i.test(name))
        .sort(compare)
        .map((name) => join(directory, name));
    }

    if (licenseFiles.length === 0) {
      fail(`npm package ${key} has no packaged license text or reviewed override`);
    }

    const repositoryUrl =
      typeof manifest.repository === "string"
        ? manifest.repository
        : manifest.repository?.url;
    packages.push({
      key,
      license: metadata.license || manifest.license || "UNDECLARED",
      repository: repositoryUrl,
      licenseFiles,
    });
  }

  packages.sort((left, right) => compare(left.key, right.key));
  const groups = new Map();
  for (const entry of packages) {
    const texts = entry.licenseFiles.map((path) => read(path)).join("\n\n");
    const groupKey = `${entry.license}\0${texts}`;
    const group = groups.get(groupKey) || {
      license: entry.license,
      packages: [],
      texts,
    };
    group.packages.push({ name: entry.key, source: entry.repository });
    groups.set(groupKey, group);
  }

  for (const entry of overrides.manual) {
    const texts = read(resolve(repository, entry.file));
    const groupKey = `${entry.license}\0${texts}`;
    const group = groups.get(groupKey) || {
      license: entry.license,
      packages: [],
      texts,
    };
    group.packages.push({ name: entry.name, source: entry.source });
    groups.set(groupKey, group);
  }

  const sections = [...groups.values()]
    .sort((left, right) => compare(left.packages[0].name, right.packages[0].name))
    .map((group) => {
      const usedBy = group.packages
        .map(({ name, source }) => `- ${name}${source ? ` (${source})` : ""}`)
        .join("\n");
      return `License: ${group.license}\nUsed by:\n${usedBy}\n\n${group.texts}`;
    });

  return `NPM AND VENDORED FRONTEND DEPENDENCIES\n======================================\n\n${sections.join(
    "\n\n-------------------------------------------------------------------------------\n\n",
  )}`;
}

const generated = `${`THIRD-PARTY LICENSES
====================

This file lists license terms for third-party code distributed with yawm.
It is generated from Cargo.lock, apps/desktop/package-lock.json, and the
reviewed overrides under licenses/. It does not change yawm's own license.

The Rust section covers the Linux, Windows, and universal macOS release target
graphs configured in about.toml.

`}${rustLicenses()}

===============================================================================

${npmLicenses()}
`;

if (mode === "--write") {
  writeFileSync(outputPath, generated);
  console.log(`wrote ${outputPath}`);
} else {
  let existing;
  try {
    existing = readFileSync(outputPath, "utf8");
  } catch {
    fail("THIRD_PARTY_LICENSES.txt is missing; regenerate it with --write");
  }
  if (existing !== generated) {
    fail("THIRD_PARTY_LICENSES.txt is stale; regenerate it with --write");
  }
  console.log("THIRD_PARTY_LICENSES.txt is current");
}
