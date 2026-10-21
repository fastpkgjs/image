import fs from 'node:fs';
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
    description: `Native binary for @fastpkg/image on ${target.os[0]}-${target.cpu[0]}`,
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
for (const file of ['index.js', 'index.cjs', 'index.d.ts', 'lib.cjs']) {
  fs.copyFileSync(
    path.join(root, 'packages', 'image', file),
    path.join(mainPackageDirectory, file),
  );
}
fs.copyFileSync(path.join(root, 'README.md'), path.join(mainDirectory, 'README.md'));
fs.copyFileSync(path.join(root, 'LICENSE'), path.join(mainDirectory, 'LICENSE'));

const mainManifest = {
  ...rootPackage,
  version,
  files: [
    'packages/image/index.js',
    'packages/image/index.cjs',
    'packages/image/index.d.ts',
    'packages/image/lib.cjs',
    'README.md',
    'LICENSE',
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
