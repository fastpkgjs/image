// Child process for parallel-jpeg.test.mjs: the rayon pool size is fixed per process, so the
// thread count under test is set here with image.concurrency(n) before any work runs.
import { engine, sharp, pattern, prng } from './_util.mjs';

const [threads, width, height, channels, quality, subsampling] = process.argv.slice(2);
engine.concurrency(Number(threads));
const w = Number(width);
const h = Number(height);
const c = Number(channels);
const raw = pattern(w, h, c);
const rnd = prng(w * 31 + h);
for (let i = 0; i < raw.length; i++) raw[i] = Math.max(0, Math.min(255, raw[i] + Math.round((rnd() - 0.5) * 14)));
const input = { raw: { width: w, height: h, channels: c } };
const options = { quality: Number(quality), chromaSubsampling: subsampling };
const par = await engine(raw, input).jpeg({ ...options, parallel: true }).toBuffer();
const ref = await engine(raw, input).jpeg({ ...options, optimiseCoding: false }).toBuffer();
const decode = async (buf) => (await sharp(buf).raw().toBuffer({ resolveWithObject: true }));
const a = await decode(par);
const b = await decode(ref);
let restarts = 0;
for (let i = 0; i + 1 < par.length; i++) if (par[i] === 0xff && par[i + 1] >= 0xd0 && par[i + 1] <= 0xd7) restarts++;
console.log(
  JSON.stringify({
    concurrency: engine.concurrency(),
    bands: restarts + 1,
    dri: par.includes(Buffer.from([0xff, 0xdd, 0x00, 0x04])),
    sameInfo: a.info.width === b.info.width && a.info.height === b.info.height && a.info.channels === b.info.channels,
    identical: a.data.equals(b.data),
    parSize: par.length,
    refSize: ref.length,
    sameBytesAsDefault: par.equals(await engine(raw, input).jpeg(options).toBuffer()),
  }),
);
