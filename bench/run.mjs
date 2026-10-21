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
    name: '8 concurrent resizes',
    iterations: 3,
    fastpkg: () => Promise.all(Array.from({ length: 8 }, () => image(small).resize(320).jpeg().toBuffer())),
    sharp: () => Promise.all(Array.from({ length: 8 }, () => sharp(small).resize(320).jpeg().toBuffer())),
  },
];

const results = [];
const oldConcurrency = image.concurrency();

await ensureLarge();
sharp.cache(false);
sharp.concurrency(1);
image.concurrency(1);

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
  concurrency: 1,
  sharp: sharp.versions,
  fastpkg: image.versions,
  results,
};
fs.writeFileSync('bench/results.json', JSON.stringify(report, null, 2));
console.log('\nWrote bench/results.json');
