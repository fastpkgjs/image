type M = Metadata;
type Info = OutputInfo;
type Instance = Sharp;

const resize: ResizeOptions = {
  width: 320,
  height: 200,
  fit: sharp.fit.cover,
  position: sharp.gravity.north,
  kernel: sharp.kernel.lanczos3,
  withoutEnlargement: true,
};
const jpeg: JpegOptions = { quality: 80, mozjpeg: true, progressive: true };
const png: PngOptions = { compressionLevel: 6, palette: true, effort: 4 };
const webp: WebpOptions = { quality: 70, lossless: false, effort: 4, alphaQuality: 90 };
const avif: AvifOptions = { quality: 50, effort: 4 };
const region: Region = { left: 0, top: 0, width: 10, height: 10 };
const color: Color = { r: 1, g: 2, b: 3, alpha: 0.5 };
const overlay: OverlayOptions = { input: Buffer.alloc(0), gravity: sharp.gravity.southeast, blend: 'over', top: 1, left: 2 };
const inputOptions: SharpOptions = {
  failOn: 'none',
  limitInputPixels: 1e8,
  animated: false,
  raw: { width: 2, height: 2, channels: 4 },
};
const create: SharpOptions = { create: { width: 2, height: 2, channels: 4, background: color } };
const fmt: FormatEnum = sharp.format;
const stats: Promise<Stats> = sharp('in.png').stats();

export async function run(input: Buffer | string): Promise<[M, Info, Instance]> {
  const pipeline: Sharp = sharp(input, inputOptions)
    .rotate()
    .resize(resize)
    .extract(region)
    .extend({ top: 2, bottom: 2, left: 2, right: 2, background: color })
    .flatten({ background: '#fff' })
    .composite([overlay])
    .sharpen({ sigma: 1 })
    .blur(0.5)
    .modulate({ brightness: 1.1, saturation: 0.9 })
    .gamma(2.2)
    .toColourspace('srgb')
    .withMetadata()
    .jpeg(jpeg);
  const { data, info } = await pipeline.clone().toBuffer({ resolveWithObject: true });
  const buffer: Buffer = await pipeline.clone().png(png).toBuffer();
  await pipeline.clone().webp(webp).toFile('out.webp');
  await pipeline.clone().avif(avif).toFile('out.avif');
  const meta: Metadata = await sharp(data).metadata();
  const width: number | undefined = meta.width;
  void [buffer, width, create, fmt, stats];
  return [meta, info, pipeline];
}

sharp.cache(false);
sharp.concurrency(1);
sharp.simd(true);
const counters: SharpCounters = sharp.counters();
void counters;
const stream: NodeJS.ReadWriteStream = sharp().resize(10);
void stream;
