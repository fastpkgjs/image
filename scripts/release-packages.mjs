import fs from 'node:fs';
import os from 'node:os';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);

function arg(name, fallback) {
  const index = args.indexOf(`--${name}`);
  return index === -1 ? fallback : args[index + 1];
}

const rootPackage = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
const version = arg('version', process.env.VERSION || rootPackage.version);
const artifacts = path.resolve(root, arg('artifacts', 'artifacts'));
const output = path.resolve(root, arg('output', 'dist/npm'));

if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error(`invalid release version: ${version}`);
}

const targets = [
  {
    package: '@fastpkg/image-win32-x64',
    directory: 'image-win32-x64',
    binary: 'image.win32-x64-msvc.node',
    os: ['win32'],
    cpu: ['x64'],
  },
  {
    package: '@fastpkg/image-win32-arm64',
    directory: 'image-win32-arm64',
    binary: 'image.win32-arm64-msvc.node',
    os: ['win32'],
    cpu: ['arm64'],
  },
  {
    package: '@fastpkg/image-darwin-x64',
    directory: 'image-darwin-x64',
    binary: 'image.darwin-x64.node',
    os: ['darwin'],
    cpu: ['x64'],
  },
  {
    package: '@fastpkg/image-darwin-arm64',
    directory: 'image-darwin-arm64',
    binary: 'image.darwin-arm64.node',
    os: ['darwin'],
    cpu: ['arm64'],
  },
  {
    package: '@fastpkg/image-linux-x64-gnu',
    directory: 'image-linux-x64-gnu',
    binary: 'image.linux-x64-gnu.node',
    os: ['linux'],
    cpu: ['x64'],
    libc: ['glibc'],
  },
  {
    package: '@fastpkg/image-linux-arm64-gnu',
    directory: 'image-linux-arm64-gnu',
    binary: 'image.linux-arm64-gnu.node',
    os: ['linux'],
    cpu: ['arm64'],
    libc: ['glibc'],
  },
  {
    package: '@fastpkg/image-linux-x64-musl',
    directory: 'image-linux-x64-musl',
    binary: 'image.linux-x64-musl.node',
    os: ['linux'],
    cpu: ['x64'],
    libc: ['musl'],
  },
  {
    package: '@fastpkg/image-linux-arm64-musl',
    directory: 'image-linux-arm64-musl',
    binary: 'image.linux-arm64-musl.node',
    os: ['linux'],
    cpu: ['arm64'],
    libc: ['musl'],
  },
];

if (!fs.existsSync(artifacts)) {
  throw new Error(`artifacts directory not found: ${artifacts}`);
}

fs.rmSync(output, { recursive: true, force: true });
fs.mkdirSync(output, { recursive: true });

const optionalDependencies = {};
for (const target of targets) {
  const source = path.join(artifacts, target.binary);
  if (!fs.existsSync(source)) {
    throw new Error(`missing native binary: ${source}`);
  }
  const directory = path.join(output, target.directory);
  fs.mkdirSync(directory, { recursive: true });
  fs.copyFileSync(source, path.join(directory, target.binary));
  const manifest = {
    name: target.package,
    version,
    description: `Native binary for @fastpkg/image on ${target.package.slice('@fastpkg/image-'.length)}`,
    license: rootPackage.license,
    repository: rootPackage.repository,
    main: target.binary,
    files: ['*.node'],
    os: target.os,
    cpu: target.cpu,
    ...(target.libc ? { libc: target.libc } : {}),
    engines: rootPackage.engines,
    publishConfig: { access: 'public' },
  };
  fs.writeFileSync(path.join(directory, 'package.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  optionalDependencies[target.package] = version;
  console.log(`staged ${target.package}@${version}`);
}

const mainDirectory = path.join(output, 'image');
const mainPackageDirectory = path.join(mainDirectory, 'packages', 'image');
fs.mkdirSync(mainPackageDirectory, { recursive: true });
for (const file of ['index.js', 'index.cjs', 'index.d.ts', 'index.d.cts', 'lib.cjs']) {
  fs.copyFileSync(
    path.join(root, 'packages', 'image', file),
    path.join(mainPackageDirectory, file),
  );
}
fs.copyFileSync(path.join(root, 'README.md'), path.join(mainDirectory, 'README.md'));
fs.copyFileSync(path.join(root, 'LICENSE'), path.join(mainDirectory, 'LICENSE'));
fs.copyFileSync(path.join(root, 'THIRD_PARTY_NOTICES.md'), path.join(mainDirectory, 'THIRD_PARTY_NOTICES.md'));

const mainManifest = {
  ...rootPackage,
  version,
  files: [
    'packages/image/index.js',
    'packages/image/index.cjs',
    'packages/image/index.d.ts',
    'packages/image/index.d.cts',
    'packages/image/lib.cjs',
    'README.md',
    'LICENSE',
    'THIRD_PARTY_NOTICES.md',
  ],
  optionalDependencies,
  publishConfig: { access: 'public' },
};
delete mainManifest.devDependencies;
delete mainManifest.napi;
delete mainManifest.scripts;
fs.writeFileSync(
  path.join(mainDirectory, 'package.json'),
  `${JSON.stringify(mainManifest, null, 2)}\n`,
);
console.log(`staged ${rootPackage.name}@${version}`);

// ---------------------------------------------------------------------------
// Self-check: fail the release if the staged output is inconsistent.
// ---------------------------------------------------------------------------
const readJson = (file) => JSON.parse(fs.readFileSync(file, 'utf8'));
const problems = [];
const check = (condition, message) => {
  if (!condition) problems.push(message);
};

const loader = fs.readFileSync(path.join(root, 'packages', 'image', 'lib.cjs'), 'utf8');
const loaderBlock = loader.match(/const PLATFORM_PACKAGES = \{([\s\S]*?)\};/);
check(loaderBlock, 'could not find PLATFORM_PACKAGES in packages/image/lib.cjs');
const loaderPackages = new Set(
  loaderBlock ? [...loaderBlock[1].matchAll(/'(@fastpkg\/[^']+)'/g)].map((m) => m[1]) : [],
);
const stagedPackages = new Set(targets.map((target) => target.package));
for (const name of stagedPackages) check(loaderPackages.has(name), `lib.cjs PLATFORM_PACKAGES lacks ${name}`);
for (const name of loaderPackages) check(stagedPackages.has(name), `lib.cjs references unstaged package ${name}`);

const stagedMain = readJson(path.join(mainDirectory, 'package.json'));
check(stagedMain.version === version, `main version ${stagedMain.version} != ${version}`);
const optionalNames = Object.keys(stagedMain.optionalDependencies || {}).sort();
check(
  JSON.stringify(optionalNames) === JSON.stringify([...stagedPackages].sort()),
  `main optionalDependencies ${optionalNames.join(',')} do not match staged platform packages`,
);
for (const [name, range] of Object.entries(stagedMain.optionalDependencies || {})) {
  check(range === version, `optionalDependency ${name} pinned to ${range}, expected ${version}`);
}
// `exports` must keep the package.json subpath and a leading `types` condition (backed by a file
// that ships in the package) for both `import` and `require`.
const stagedExports = stagedMain.exports || {};
check(stagedExports['./package.json'] === './package.json', 'main exports lacks "./package.json": "./package.json"');
for (const condition of ['import', 'require']) {
  const entry = (stagedExports['.'] || {})[condition];
  check(entry && Object.keys(entry)[0] === 'types', `exports["."].${condition} must list "types" first`);
  for (const target of Object.values(entry || {})) {
    check(fs.existsSync(path.join(mainDirectory, target)), `exports["."].${condition} target ${target} is not staged`);
  }
}
check(
  stagedMain.types && fs.existsSync(path.join(mainDirectory, stagedMain.types)),
  `main "types" ${stagedMain.types} is not staged`,
);
const napiTargets = (rootPackage.napi && rootPackage.napi.targets) || [];
check(
  napiTargets.length === targets.length,
  `package.json napi.targets has ${napiTargets.length} entries, release has ${targets.length}`,
);

for (const target of targets) {
  const directory = path.join(output, target.directory);
  const manifestPath = path.join(directory, 'package.json');
  check(fs.existsSync(manifestPath), `${target.directory}: package.json missing`);
  if (!fs.existsSync(manifestPath)) continue;
  const manifest = readJson(manifestPath);
  check(manifest.name === target.package, `${target.directory}: name ${manifest.name}`);
  check(manifest.version === version, `${target.directory}: version ${manifest.version} != ${version}`);
  check(manifest.main === target.binary, `${target.directory}: main ${manifest.main}`);
  check(fs.existsSync(path.join(directory, manifest.main)), `${target.directory}: binary ${manifest.main} missing`);
  check(
    JSON.stringify(manifest.libc || null) === JSON.stringify(target.libc || null),
    `${target.directory}: libc field mismatch`,
  );
  // The loader looks up `${platform}-${arch}[-libc]`; make sure the key maps to this package.
  const libcSuffix = target.libc ? (target.libc[0] === 'musl' ? '-musl' : '-gnu') : '';
  const key = `${target.os[0]}-${target.cpu[0]}${libcSuffix}`;
  check(
    new RegExp(`'${key}':\\s*'${target.package}'`).test(loader),
    `lib.cjs maps ${key} to a different package than ${target.package}`,
  );
}

const npmBin = process.platform === 'win32' ? 'npm.cmd' : 'npm';
const pack = spawnSync(npmBin, ['pack', '--dry-run', '--json', '--ignore-scripts'], {
  cwd: mainDirectory,
  encoding: 'utf8',
  env: { ...process.env, npm_config_cache: process.env.npm_config_cache || path.join(os.tmpdir(), 'fastpkg-npm-cache') },
  shell: process.platform === 'win32',
});
if (pack.status !== 0) {
  problems.push(`npm pack --dry-run failed: ${pack.stderr || pack.stdout}`);
} else {
  try {
    const files = JSON.parse(pack.stdout)[0].files.map((entry) => entry.path);
    for (const required of [
      'packages/image/index.js',
      'packages/image/index.cjs',
      'packages/image/index.d.ts',
      'packages/image/index.d.cts',
      'packages/image/lib.cjs',
      'README.md',
      'LICENSE',
      'THIRD_PARTY_NOTICES.md',
      'package.json',
    ]) {
      check(files.includes(required), `npm pack of main package is missing ${required}`);
    }
    check(!files.some((file) => file.endsWith('.node')), 'main package unexpectedly contains a .node binary');
  } catch (error) {
    problems.push(`could not parse npm pack output: ${error.message}`);
  }
}

if (problems.length) {
  console.error('release self-check failed:');
  for (const problem of problems) console.error(`  - ${problem}`);
  process.exit(1);
}
console.log(`self-check ok: ${targets.length} platform packages + main package at ${version}`);
