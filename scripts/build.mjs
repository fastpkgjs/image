import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const debug = process.env.FASTPKG_DEBUG === '1' || process.argv.includes('--debug');

if (process.platform === 'linux') {
  process.env.RUSTFLAGS = [process.env.RUSTFLAGS, '-C link-arg=-Wl,-Bsymbolic']
    .filter(Boolean)
    .join(' ');
}

if (process.platform === 'win32') {
  const args = ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', path.join(root, 'scripts', 'build.ps1')];
  if (debug) args.push('-Debug');
  const result = spawnSync('powershell', args, { cwd: root, stdio: 'inherit' });
  process.exit(result.status ?? 1);
}

const napiArgs = [
  'napi',
  'build',
  '--platform',
  ...(debug ? [] : ['--release']),
  '--no-js',
  '--dts',
  'native.d.ts',
  '--manifest-path',
  'crates/fastpkg-napi/Cargo.toml',
  '--output-dir',
  'packages/image',
];
const result = spawnSync('npx', napiArgs, { cwd: root, stdio: 'inherit' });
process.exit(result.status ?? 1);

