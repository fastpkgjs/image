import fs from 'node:fs';
import { performance } from 'node:perf_hooks';
import sharp from 'sharp';
import image from '../packages/image/index.js';

const small = 'fixtures/photo.jpg';
const large = 'fixtures/large.jpg';

async function ensureLarge() {
  if (fs.existsSync(large)) return;
  const width = 4000;
  const height = 3000;
  const data = Buffer.allocUnsafe(width * height * 3);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 3;
      data[i] = (x * 255 / width) | 0;
      data[i + 1] = (y * 255 / height) | 0;
      data[i + 2] = ((x + y) * 255 / (width + height)) | 0;
    }
  }
  await sharp(data, { raw: { width, height, channels: 3 } })
    .jpeg({ quality: 88 })
    .toFile(large);
}

const cacheDir = 'bench/.cache';

// Deterministic photographic-ish image: multi-octave smooth noise plus fine
// grain, so JPEG/PNG ratios resemble a real photograph rather than a gradient.
async function makePhoto(width, height) {
  let seed = 0x9e3779b9;
  const rand = () => {
    seed ^= seed << 13; seed >>>= 0;
    seed ^= seed >>> 17;
    seed ^= seed << 5; seed >>>= 0;
    return seed / 4294967296;
  };
  const octave = async (w, h, amp) => {
    const raw = Buffer.allocUnsafe(w * h * 3);
    for (let i = 0; i < raw.length; i++) raw[i] = (rand() * 255) | 0;
    const out = await sharp(raw, { raw: { width: w, height: h, channels: 3 } })
      .resize(width, height, { kernel: 'cubic' })
      .raw()
      .toBuffer();
    return { out, amp };
  };
  const layers = [
    await octave(Math.ceil(width / 400), Math.ceil(height / 400), 0.55),
    await octave(Math.ceil(width / 100), Math.ceil(height / 100), 0.30),
    await octave(Math.ceil(width / 20), Math.ceil(height / 20), 0.12),
  ];
  const data = Buffer.allocUnsafe(width * height * 3);
  for (let i = 0; i < data.length; i++) {
    let v = 0;
    for (const l of layers) v += (l.out[i] - 127.5) * l.amp;
    v += (rand() - 0.5) * 3;
    data[i] = Math.max(0, Math.min(255, 128 + v * 1.8));
  }
  return { data, width, height };
}

async function ensureRealistic() {
  fs.mkdirSync(cacheDir, { recursive: true });
  const jpg = `${cacheDir}/photo-4000.jpg`;
  const png = `${cacheDir}/photo-2000.png`;
  const pngAlpha = `${cacheDir}/photo-2000-alpha.png`;
  if (fs.existsSync(jpg) && fs.existsSync(png) && fs.existsSync(pngAlpha)) return { jpg, png, pngAlpha };
  const big = await makePhoto(4000, 3000);
  await sharp(big.data, { raw: { width: 4000, height: 3000, channels: 3 } })
    .jpeg({ quality: 85 })
    .toFile(jpg);
  const mid = await sharp(big.data, { raw: { width: 4000, height: 3000, channels: 3 } })
    .resize(2000, 1500)
    .blur(1.2)
    .raw()
    .toBuffer();
  await sharp(mid, { raw: { width: 2000, height: 1500, channels: 3 } }).png({ adaptiveFiltering: true }).toFile(png);
  const alpha = Buffer.allocUnsafe(2000 * 1500);
  for (let y = 0; y < 1500; y++) {
    for (let x = 0; x < 2000; x++) {
      const d = Math.hypot(x - 1000, y - 750) / 900;
      alpha[y * 2000 + x] = Math.max(0, Math.min(255, (1.2 - d) * 255 * 1.5));
    }
  }
  const rgba = await sharp(mid, { raw: { width: 2000, height: 1500, channels: 3 } })
    .joinChannel(alpha, { raw: { width: 2000, height: 1500, channels: 1 } })
    .png({ adaptiveFiltering: true })
    .toBuffer();
  fs.writeFileSync(pngAlpha, rgba);
  return { jpg, png, pngAlpha };
}

// Screenshot-like graphic (flat fills, anti-aliased text, one translucent panel) as an RGBA PNG.
async function ensureGraphic() {
  const file = `${cacheDir}/graphic-1200.png`;
  if (fs.existsSync(file)) return file;
  fs.mkdirSync(cacheDir, { recursive: true });
  const rows = Array.from({ length: 24 }, (_, i) =>
    `<text x="32" y="${110 + i * 30}" font-family="sans-serif" font-size="17" fill="#222">Row ${i}: the quick brown fox jumps over the lazy dog ${i * 37}</text>`).join('');
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="860"><rect width="1200" height="860" fill="#fff"/><rect width="1200" height="64" fill="#2b5797"/>${rows}<rect x="820" y="100" width="320" height="220" rx="12" fill="#e8f0fe" fill-opacity="0.6" stroke="#999"/><circle cx="980" cy="560" r="110" fill="#d33"/></svg>`;
  await sharp(Buffer.from(svg)).png().toFile(file);
  return file;
}

async function measure(fn, iterations, warmup = 3) {
  for (let i = 0; i < warmup; i++) await fn();
  const samples = [];
  for (let i = 0; i < iterations; i++) {
    const start = performance.now();
    await fn();
    samples.push(performance.now() - start);
  }
  samples.sort((a, b) => a - b);
  const median = samples[Math.floor(samples.length / 2)];
  const p95 = samples[Math.min(samples.length - 1, Math.floor(samples.length * 0.95))];
  return { median, p95, min: samples[0], max: samples[samples.length - 1] };
}

await ensureLarge();
const real = await ensureRealistic();
const graphicPng = await ensureGraphic();

const scenarios = [
  {
    name: 'metadata 800x600',
    iterations: 40,
    fastpkg: () => image(small).metadata(),
    sharp: () => sharp(small).metadata(),
  },
  {
    name: 'metadata 4000x3000',
    iterations: 30,
    fastpkg: () => image(large).metadata(),
    sharp: () => sharp(large).metadata(),
  },
  {
    name: 'resize 1200 + jpeg q85',
    iterations: 10,
    fastpkg: () => image(large).resize({ width: 1200 }).jpeg({ quality: 85 }).toBuffer(),
    sharp: () => sharp(large).resize({ width: 1200 }).jpeg({ quality: 85 }).toBuffer(),
  },
  {
    name: 'resize 1200 + png',
    iterations: 6,
    fastpkg: () => image(large).resize({ width: 1200 }).png({ compressionLevel: 6 }).toBuffer(),
    sharp: () => sharp(large).resize({ width: 1200 }).png({ compressionLevel: 6 }).toBuffer(),
  },
  {
    name: 'resize 1200 + webp lossless',
    iterations: 6,
    fastpkg: () => image(large).resize({ width: 1200 }).webp({ lossless: true }).toBuffer(),
    sharp: () => sharp(large).resize({ width: 1200 }).webp({ lossless: true }).toBuffer(),
  },
  {
    name: 'graphic png -> webp lossless',
    iterations: 10,
    fastpkg: () => image(graphicPng).webp({ lossless: true }).toBuffer(),
    sharp: () => sharp(graphicPng).webp({ lossless: true }).toBuffer(),
  },
  {
    name: 'blur + sharpen + jpeg',
    iterations: 6,
    fastpkg: () => image(small).blur(2).sharpen({ sigma: 1 }).jpeg({ quality: 85 }).toBuffer(),
    sharp: () => sharp(small).blur(2).sharpen({ sigma: 1 }).jpeg({ quality: 85 }).toBuffer(),
  },
  {
    name: 'stats 4000x3000',
    iterations: 5,
    fastpkg: () => image(large).stats(),
    sharp: () => sharp(large).stats(),
  },
  {
    name: 'photo jpeg -> resize 1200 jpeg',
    iterations: 10,
    fastpkg: () => image(real.jpg).resize({ width: 1200 }).jpeg({ quality: 85 }).toBuffer(),
    sharp: () => sharp(real.jpg).resize({ width: 1200 }).jpeg({ quality: 85 }).toBuffer(),
  },
  {
    name: 'photo jpeg -> thumb 320 jpeg',
    iterations: 15,
    fastpkg: () => image(real.jpg).resize({ width: 320 }).jpeg({ quality: 80 }).toBuffer(),
    sharp: () => sharp(real.jpg).resize({ width: 320 }).jpeg({ quality: 80 }).toBuffer(),
  },
  {
    name: 'photo jpeg -> resize 1200 webp q80',
    iterations: 6,
    fastpkg: () => image(real.jpg).resize({ width: 1200 }).webp({ quality: 80 }).toBuffer(),
    sharp: () => sharp(real.jpg).resize({ width: 1200 }).webp({ quality: 80 }).toBuffer(),
  },
  {
    name: 'photo jpeg -> jpeg q85 (no resize)',
    iterations: 5,
    fastpkg: () => image(real.jpg).jpeg({ quality: 85 }).toBuffer(),
    sharp: () => sharp(real.jpg).jpeg({ quality: 85 }).toBuffer(),
  },
  {
    // Opt-in banded encode; only differs from the row above with BENCH_CONCURRENCY > 1
    // (with one thread it takes the normal path).
    name: 'photo jpeg -> jpeg q85 parallel (opt-in)',
    iterations: 5,
    fastpkg: () => image(real.jpg).jpeg({ quality: 85, parallel: true }).toBuffer(),
    sharp: () => sharp(real.jpg).jpeg({ quality: 85 }).toBuffer(),
  },
  {
    name: 'photo png 2000 -> png',
    iterations: 5,
    fastpkg: () => image(real.png).png({ compressionLevel: 6 }).toBuffer(),
    sharp: () => sharp(real.png).png({ compressionLevel: 6 }).toBuffer(),
  },
  {
    name: 'photo png 2000 -> resize 800 png',
    iterations: 6,
    fastpkg: () => image(real.png).resize({ width: 800 }).png({ compressionLevel: 6 }).toBuffer(),
    sharp: () => sharp(real.png).resize({ width: 800 }).png({ compressionLevel: 6 }).toBuffer(),
  },
  {
    name: 'alpha png 2000 -> png',
    iterations: 5,
    fastpkg: () => image(real.pngAlpha).png({ compressionLevel: 6 }).toBuffer(),
    sharp: () => sharp(real.pngAlpha).png({ compressionLevel: 6 }).toBuffer(),
  },
  {
    name: 'alpha png 2000 -> resize 800 png',
    iterations: 6,
    fastpkg: () => image(real.pngAlpha).resize({ width: 800 }).png({ compressionLevel: 6 }).toBuffer(),
    sharp: () => sharp(real.pngAlpha).resize({ width: 800 }).png({ compressionLevel: 6 }).toBuffer(),
  },
  {
    name: '8 concurrent resizes',
    iterations: 3,
    fastpkg: () => Promise.all(Array.from({ length: 8 }, () => image(small).resize(320).jpeg().toBuffer())),
    sharp: () => Promise.all(Array.from({ length: 8 }, () => sharp(small).resize(320).jpeg().toBuffer())),
  },
];

const results = [];
const oldConcurrency = image.concurrency();

sharp.cache(false);
// BENCH_CONCURRENCY=N runs both libraries with N threads (default 1, as in the README table).
const benchConcurrency = Math.max(1, Number(process.env.BENCH_CONCURRENCY) || 1);
sharp.concurrency(benchConcurrency);
image.concurrency(benchConcurrency);

for (const scenario of scenarios) {
  const fastpkg = await measure(scenario.fastpkg, scenario.iterations);
  const sharpResult = await measure(scenario.sharp, scenario.iterations);
  const ratio = sharpResult.median / fastpkg.median;
  results.push({
    name: scenario.name,
    fastpkg,
    sharp: sharpResult,
    speedup: ratio,
  });
  console.log(
    `${scenario.name.padEnd(30)} fastpkg ${fastpkg.median.toFixed(2).padStart(8)}ms  sharp ${sharpResult.median
      .toFixed(2)
      .padStart(8)}ms  x${ratio.toFixed(2)}`
  );
}

image.concurrency(oldConcurrency);

const report = {
  generatedAt: new Date().toISOString(),
  node: process.version,
  platform: `${process.platform}-${process.arch}`,
  cpu: process.env.PROCESSOR_IDENTIFIER || 'unknown',
  concurrency: benchConcurrency,
  sharp: sharp.versions,
  fastpkg: image.versions,
  results,
};
fs.writeFileSync('bench/results.json', JSON.stringify(report, null, 2));
console.log('\nWrote bench/results.json');
