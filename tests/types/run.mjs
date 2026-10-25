// Typings test: compiles the same sample sources against @fastpkg/image and against sharp's own
// typings (only the module specifier differs) under every supported TypeScript module mode.
//
//   node tests/types/run.mjs [--keep] [--only <config>]
//
// Each configuration gets a scratch project under tests/types/.work/ with
// node_modules/@fastpkg/image symlinked to the repository root, so package.json `exports`
// (the `types` conditions for import and require) is exercised exactly as a consumer sees it.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..', '..');
const work = path.join(here, '.work');
const tsc = path.join(root, 'node_modules', 'typescript', 'bin', 'tsc');
const args = process.argv.slice(2);
const only = args.includes('--only') ? args[args.indexOf('--only') + 1] : null;

// `shared` samples are also compiled against sharp with the specifier swapped.
// usage-named.ts / usage-ns.ts are generated from samples/usage.body.ts: the first imports the
// types by name (works for both packages in every mode), the second uses `sharp.Metadata`-style
// namespace access, which sharp only supports where `export =` semantics apply (CommonJS).
const SHARED = { esm: ['usage-named.ts', 'esm-namespace.mts'], cjsRequire: ['cjs-require.cts'] };
const USAGE_TYPES = ['Metadata', 'OutputInfo', 'Sharp', 'ResizeOptions', 'JpegOptions', 'PngOptions', 'WebpOptions', 'AvifOptions', 'Region', 'Color', 'OverlayOptions', 'SharpOptions', 'FormatEnum', 'Stats', 'SharpCounters'];
const usageHeader = (kind) => kind === 'named'
  ? `import sharp, { ${USAGE_TYPES.map((t) => `type ${t}`).join(', ')} } from '@fastpkg/image';\n\n`
  : `import sharp from '@fastpkg/image';\n\n${USAGE_TYPES.map((t) => `type ${t} = sharp.${t};`).join('\n')}\n\n`;
const FASTPKG_ONLY = ['fastpkg-only.ts'];

const NS_ONLY = [...FASTPKG_ONLY, 'usage-ns.ts'];
// Cross-package assignability (needs both packages in one program, so fastpkg pass only).
const COMPAT = ['compat.mts'];
const configs = [
  // ESM files: sharp's ESM typings have no `sharp.Metadata` namespace, so the shared sample
  // imports types by name; the namespace form is additionally checked for @fastpkg/image.
  { name: 'nodenext-esm', pkgType: 'module', options: { module: 'nodenext', moduleResolution: 'nodenext' }, shared: [...SHARED.esm, ...SHARED.cjsRequire], only: [...NS_ONLY, ...COMPAT] },
  { name: 'node16-esm', pkgType: 'module', options: { module: 'node16', moduleResolution: 'node16' }, shared: [...SHARED.esm, ...SHARED.cjsRequire], only: NS_ONLY },
  // CommonJS files (package type "commonjs"): require condition, `export =` typings.
  { name: 'nodenext-cjs', pkgType: 'commonjs', options: { module: 'nodenext', moduleResolution: 'nodenext' }, shared: ['usage-ns.ts', 'usage-named.ts', ...SHARED.cjsRequire], only: FASTPKG_ONLY },
  { name: 'node16-cjs', pkgType: 'commonjs', options: { module: 'node16', moduleResolution: 'node16' }, shared: ['usage-ns.ts', 'usage-named.ts', ...SHARED.cjsRequire], only: FASTPKG_ONLY },
  { name: 'bundler-interop', pkgType: 'module', options: { module: 'esnext', moduleResolution: 'bundler', esModuleInterop: true }, shared: ['usage-named.ts', 'esm-namespace.mts'], only: NS_ONLY },
  { name: 'bundler-nointerop', pkgType: 'module', options: { module: 'esnext', moduleResolution: 'bundler', esModuleInterop: false }, shared: ['usage-named.ts', 'esm-namespace.mts'], only: NS_ONLY },
  { name: 'commonjs-node10-interop', sharp: false, pkgType: 'commonjs', options: { module: 'commonjs', moduleResolution: 'node10', esModuleInterop: true, ignoreDeprecations: '6.0' }, shared: ['usage-ns.ts', 'usage-named.ts', ...SHARED.cjsRequire], only: FASTPKG_ONLY },
  // node10 resolution ignores `exports`; sharp's top-level "types" is an ESM .d.mts, so sharp itself
  // fails here (sharp: false) while @fastpkg/image's points at the `export =` .d.cts.
  // `import x = require()` works without esModuleInterop for both packages.
  { name: 'commonjs-node10-nointerop', sharp: false, pkgType: 'commonjs', options: { module: 'commonjs', moduleResolution: 'node10', esModuleInterop: false, ignoreDeprecations: '6.0' }, shared: SHARED.cjsRequire, only: [] },
  { name: 'node16-cjs-nointerop', pkgType: 'commonjs', options: { module: 'node16', moduleResolution: 'node16', esModuleInterop: false }, shared: SHARED.cjsRequire, only: [] },
];

const baseOptions = {
  target: 'ES2022',
  strict: true,
  noEmit: true,
  skipLibCheck: false,
  types: ['node'],
  lib: ['ES2022'],
};

function stage(config, target) {
  const dir = path.join(work, `${config.name}-${target}`);
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(path.join(dir, 'node_modules', '@fastpkg'), { recursive: true });
  fs.symlinkSync(root, path.join(dir, 'node_modules', '@fastpkg', 'image'), 'junction');
  fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ type: config.pkgType, private: true }));
  const files = [];
  const copy = (name, rewrite) => {
    const generated = name.match(/^usage-(named|ns)\.ts$/);
    let text = generated
      ? usageHeader(generated[1]) + fs.readFileSync(path.join(here, 'samples', 'usage.body.ts'), 'utf8')
      : fs.readFileSync(path.join(here, 'samples', name), 'utf8');
    if (rewrite) text = text.replaceAll("'@fastpkg/image'", "'sharp'");
    fs.writeFileSync(path.join(dir, name), text);
    files.push(name);
  };
  for (const name of config.shared) copy(name, target === 'sharp');
  if (target === 'fastpkg') for (const name of config.only) copy(name, false);
  const options = { ...baseOptions, ...config.options };
  if (options.ignoreDeprecations && !tscMajor6) delete options.ignoreDeprecations;
  fs.writeFileSync(path.join(dir, 'tsconfig.json'), JSON.stringify({ compilerOptions: options, files }, null, 2));
  return dir;
}

const tscMajor6 = JSON.parse(fs.readFileSync(path.join(root, 'node_modules', 'typescript', 'package.json'), 'utf8')).version.startsWith('6');

function compile(dir) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [tsc, '-p', dir, '--pretty', 'false'], { cwd: dir });
    let out = '';
    child.stdout.on('data', (d) => (out += d));
    child.stderr.on('data', (d) => (out += d));
    child.on('close', (code) => resolve({ code, out }));
  });
}

fs.mkdirSync(work, { recursive: true });
const jobs = [];
for (const config of configs) {
  if (only && config.name !== only) continue;
  for (const target of ['fastpkg', 'sharp']) {
    if (target === 'sharp' && (config.sharp === false || config.shared.length === 0)) continue;
    jobs.push({ label: `${config.name} / ${target}`, dir: stage(config, target) });
  }
}

// A few at a time: tsc is CPU bound and the machine is shared.
const results = [];
const queue = [...jobs];
await Promise.all(
  Array.from({ length: 3 }, async () => {
    while (queue.length) {
      const job = queue.shift();
      results.push({ ...job, ...(await compile(job.dir)) });
    }
  }),
);
results.sort((a, b) => a.label.localeCompare(b.label));
let failed = 0;
for (const r of results) {
  console.log(`${r.code === 0 ? 'ok  ' : 'FAIL'} ${r.label}`);
  if (r.code !== 0) {
    failed++;
    console.log(r.out.split('\n').map((l) => `       ${l}`).join('\n'));
  }
}
if (!args.includes('--keep')) fs.rmSync(work, { recursive: true, force: true });
console.log(failed ? `${failed} typings job(s) failed` : `typings ok (${results.length} jobs)`);
process.exit(failed ? 1 : 0);
