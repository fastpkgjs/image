// failOn: differential tests against sharp on deterministically corrupted files.
//   npm test                  a few hundred cases, time-bounded
//   FAILON_FULL=1 npm test    the whole corpus (several thousand cases)
// The corpus generator lives in tests/_failon-corpus.mjs, where it also runs as a script:
//   node tests/_failon-corpus.mjs [format=png,gif] [perBase=N] [show=N] [dump=dir]
import test, { after } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Readable } from 'node:stream';
import { sharp, engine, make } from './_util.mjs';
import { baseFiles, mutationsFor, runCorpus, judge, LEVELS } from './_failon-corpus.mjs';

const FULL = process.env.FAILON_FULL === '1';
const bases = await baseFiles();
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'fastpkg-failon-'));
after(() => fs.rmSync(tmp, { recursive: true, force: true }));

// Known differences from sharp, kept out of the zero-mismatch assertions and listed as todos.
const GAPS = [
  {
    name: 'AVIF: a damaged AV1 payload is concealed by rav1d, libheif rejects it',
    match: (m) => m.format === 'avif' && m.kind === 'accept-vs-reject',
  },
  {
    name: 'AVIF: a damaged iloc/ispe box is rejected here, libheif still decodes the image',
    match: (m) => m.format === 'avif' && m.kind === 'reject-vs-accept',
  },
  {
    name: 'WebP: some damaged lossless bitstreams decode in libwebp here and fail in libvips',
    match: (m) => m.format === 'webp' && m.kind === 'accept-vs-reject' && /lossless/.test(m.base),
  },
  {
    name: "GIF failOn 'warning': junk after a cut frame that parses as another frame is an error in libvips",
    match: (m) => m.format === 'gif' && m.failOn === 'warning' && m.kind === 'accept-vs-reject',
  },
  {
    name: "JPEG: a progressive file with an inconsistent scan script can fail in libvips even at failOn 'none'",
    match: (m) => m.format === 'jpeg' && m.kind === 'accept-vs-reject' && /progressive/.test(m.base),
  },
  {
    name: 'PNG: IHDR sizes that need more than the decoder buffer limit are rejected here, libvips allocates them',
    match: (m) => m.format === 'png' && m.kind === 'reject-vs-accept' && /limits are exceeded/.test(m.engine),
  },
];
const isGap = (m) => GAPS.some((gap) => gap.match(m));
const describe = (m) => `${m.format}/${m.failOn} ${m.kind}: ${m.base}: ${m.mutation} (sharp: ${String(m.sharp).split('\n')[0]}; engine: ${String(m.engine).split('\n')[0]})`;
const failures = (result) => result.mismatches.filter((m) => m.kind !== 'pixels' && !isGap(m)).map(describe);

test('failOn corpus: a deterministic subset agrees with sharp', async () => {
  const result = await runCorpus({ bases, perBase: FULL ? Infinity : 4, budgetMs: FULL ? Infinity : 60000 });
  assert.ok(FULL || result.totals.cases >= 300, `only ${result.totals.cases} cases ran`);
  const dangerous = result.mismatches.filter((m) => m.kind === 'accept-vs-reject' && !isGap(m)).map(describe);
  assert.deepEqual(dangerous, [], 'inputs sharp rejects but fastpkg accepts');
  assert.deepEqual(failures(result), [], 'any other accept/reject or dimension disagreement');
});

// One group per root cause. Every mutation matching `ids` is judged at every level.
const GROUPS = [
  ['PNG: bad chunk CRC (critical and ancillary)', 'png', /^crc /],
  ['PNG: data, chunks or IDAT after IEND, missing IEND', 'png', /IEND/],
  ['PNG: chunk length and type damage', 'png', /^(len|type case|delete|duplicate)/],
  ['PNG: damaged, short or long zlib stream', 'png', /^IDAT|^data flip/],
  ['PNG: truncation and byte damage', 'png', /^(trunc|flip|zero|delete @|insert|trailing)/],
  ['JPEG: missing EOI, extra bytes, fill bytes', 'jpeg', /EOI|before SOI|trailing/],
  ['JPEG: damaged entropy data (bad Huffman codes)', 'jpeg', /^entropy/],
  ['JPEG: segment damage and truncation', 'jpeg', /^(delete seg|len|truncate in seg|trunc|flip|zero)/],
  ['WebP: RIFF and chunk sizes', 'webp', /riff size|chunk|odd pad|garbage after riff/],
  ['WebP: truncation and byte damage', 'webp', /^(trunc|flip|zero|delete @|insert|trailing)/],
  ['GIF: truncation, missing trailer, trailing data', 'gif', /^(trunc|drop trailer|garbage after|two trailers|cut mid)/],
  ['GIF: header, descriptor and LZW damage', 'gif', /^(header byte|drop image|bad LZW|bad sub-block|flip|zero|delete @|insert)/],
  ['TIFF: IFD damage and truncation', 'tiff', /./],
  ['AVIF: truncation and trailing garbage', 'avif', /^(trunc|trailing)/],
];
for (const [name, format, ids] of GROUPS) {
  test(`failOn corpus: ${name}`, async () => {
    const out = [];
    for (const base of bases.filter((b) => b.format === format)) {
      const muts = mutationsFor(base).filter((m) => ids.test(m.id));
      // The whole group in full runs; every sixth mutation in the default run.
      for (const m of FULL ? muts : muts.filter((_, i) => i % 6 === 0)) {
        for (const failOn of LEVELS) {
          const verdict = await judge(m.buf, failOn);
          if (verdict && verdict.kind !== 'pixels') {
            const mismatch = { ...verdict, failOn, base: base.name, format, mutation: m.id };
            if (!isGap(mismatch)) out.push(describe(mismatch));
          }
        }
      }
    }
    assert.deepEqual(out, []);
  });
}

test('failOn: the default level is warning', async () => {
  const jpeg = bases.find((b) => b.name === 'jpeg q80').buf;
  const damaged = Buffer.concat([jpeg.subarray(0, jpeg.length >> 1)]);
  for (const lib of [sharp, engine]) {
    await assert.rejects(lib(damaged).raw().toBuffer(), undefined, lib === sharp ? 'sharp' : 'fastpkg');
    await lib(damaged, { failOn: 'none' }).raw().toBuffer();
  }
});

test('failOn: invalid values throw as in sharp, for buffer, file and stream inputs', async () => {
  const png = await make('png', 8, 8);
  const file = path.join(tmp, 'ok.png');
  fs.writeFileSync(file, png);
  const inputs = {
    buffer: (lib, failOn) => lib(png, { failOn }),
    file: (lib, failOn) => lib(file, { failOn }),
    stream: (lib, failOn) => {
      const instance = lib({ failOn });
      Readable.from([png]).pipe(instance);
      return instance;
    },
  };
  for (const failOn of ['bogus', 'Warning', '', 5, true]) {
    for (const [kind, build] of Object.entries(inputs)) {
      const results = [];
      for (const lib of [sharp, engine]) {
        try {
          const instance = build(lib, failOn);
          instance.on?.('error', () => {});
          await instance.toBuffer();
          results.push('ok');
        } catch (error) {
          results.push(/failOn/.test(error.message) ? 'invalid' : `other: ${error.message}`);
        }
      }
      assert.deepEqual(results, ['invalid', 'invalid'], `${kind} failOn=${JSON.stringify(failOn)}`);
    }
  }
});

test('failOn: buffer, file and stream inputs honour the same level', async () => {
  const jpeg = bases.find((b) => b.name === 'jpeg q80').buf;
  const damaged = jpeg.subarray(0, jpeg.length - 40);
  const file = path.join(tmp, 'damaged.jpg');
  fs.writeFileSync(file, damaged);
  const viaStream = async (lib, failOn) => {
    const instance = lib({ failOn });
    const done = instance.toBuffer();
    Readable.from([damaged]).pipe(instance);
    return done;
  };
  const verdict = async (run) => {
    try {
      await run();
      return 'ok';
    } catch {
      return 'rejected';
    }
  };
  for (const failOn of LEVELS) {
    const seen = [];
    for (const lib of [sharp, engine]) {
      seen.push(
        await verdict(() => lib(damaged, { failOn }).toBuffer()),
        await verdict(() => lib(file, { failOn }).toBuffer()),
        await verdict(() => viaStream(lib, failOn)),
      );
    }
    assert.deepEqual(seen.slice(3), seen.slice(0, 3), `failOn=${failOn}: fastpkg [buffer, file, stream] vs sharp`);
  }
});

// Documented gaps: each is a case where fastpkg still differs from sharp.
for (const gap of GAPS) test.todo(gap.name);
