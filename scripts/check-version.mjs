// Verifies that a release tag (e.g. v0.2.0) matches package.json and the Cargo
// workspace version. Usage: node scripts/check-version.mjs [v0.2.0]
// Without an argument it only checks that package.json and Cargo.toml agree.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const pkgVersion = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8')).version;
const cargo = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
const section = cargo.match(/^\[workspace\.package\]([\s\S]*?)(?=^\[|$(?![\s\S]))/m);
const cargoVersion = section && section[1].match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const tag = process.argv[2];
const tagVersion = tag ? tag.replace(/^v/, '') : pkgVersion;

console.log(`tag=${tag ?? '(none)'} package.json=${pkgVersion} Cargo.toml workspace=${cargoVersion}`);
const errors = [];
if (!cargoVersion) errors.push('could not read [workspace.package] version from Cargo.toml');
if (pkgVersion !== tagVersion) errors.push(`package.json version ${pkgVersion} != tag ${tagVersion}`);
if (cargoVersion !== tagVersion) errors.push(`Cargo.toml workspace version ${cargoVersion} != tag ${tagVersion}`);
const lock = fs.readFileSync(path.join(root, 'Cargo.lock'), 'utf8');
for (const match of lock.matchAll(/name = "(fastpkg-[^"]+)"\nversion = "([^"]+)"/g)) {
  if (match[2] !== tagVersion) errors.push(`Cargo.lock: ${match[1]} is ${match[2]}, expected ${tagVersion}`);
}
if (errors.length) {
  for (const error of errors) console.error(`error: ${error}`);
  process.exit(1);
}
console.log('versions consistent');
