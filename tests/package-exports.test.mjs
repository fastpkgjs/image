import assert from 'node:assert/strict';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
// Resolve through the package's own name so `exports` is exercised (self-reference).
const require = createRequire(path.join(root, 'tests', 'x.cjs'));

test('package.json is exported (require and import)', async () => {
  assert.equal(require('@fastpkg/image/package.json').name, '@fastpkg/image');
  const viaImport = await import('@fastpkg/image/package.json', { with: { type: 'json' } });
  assert.equal(viaImport.default.version, pkg.version);
});

test('exports: types come first and exist for both import and require', () => {
  const dot = pkg.exports['.'];
  for (const condition of ['import', 'require']) {
    const keys = Object.keys(dot[condition]);
    assert.equal(keys[0], 'types', `${condition}: types must be the first condition`);
    for (const target of Object.values(dot[condition])) {
      assert.ok(fs.existsSync(path.join(root, target)), `${condition}: ${target} is missing`);
    }
  }
  assert.ok(fs.existsSync(path.join(root, pkg.types)));
  assert.equal(pkg.exports['./package.json'], './package.json');
  for (const file of ['index.d.ts', 'index.d.cts']) {
    assert.ok(pkg.files.includes(`packages/image/${file}`), `files lacks ${file}`);
  }
  assert.ok(pkg.files.includes('THIRD_PARTY_NOTICES.md'));
});

test('require() and import give the same module, default and named exports', async () => {
  const cjs = require('@fastpkg/image');
  const esm = await import('@fastpkg/image');
  assert.equal(typeof cjs, 'function');
  assert.equal(cjs.default, cjs);
  assert.equal(esm.default, cjs);
  assert.equal(esm.Sharp, cjs.Sharp);
  for (const name of ['cache', 'concurrency', 'counters', 'simd', 'format', 'versions', 'interpolators', 'gravity', 'strategy', 'position', 'kernel', 'fit', 'blend', 'bool', 'colourspace', 'colorspace', 'align', 'queue', 'Gradient', 'gradient', 'linearGradient', 'radialGradient', 'block', 'unblock']) {
    assert.ok(name in cjs, `require lacks ${name}`);
    assert.equal(esm[name], cjs[name], `import { ${name} } differs from require().${name}`);
  }
});

test('every value declared in the typings exists at runtime', async () => {
  const cjs = require('@fastpkg/image');
  const declared = [...fs.readFileSync(path.join(root, 'packages', 'image', 'index.d.ts'), 'utf8').matchAll(/^export declare const (\w+)/gm)].map((m) => m[1]);
  assert.ok(declared.length > 20);
  for (const name of [...declared, 'Sharp', 'Gradient']) assert.ok(name in cjs, `typings declare ${name} but the runtime lacks it`);
});
