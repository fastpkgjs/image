// Extensions and ESM named imports that sharp's own typings do not have. Checked against
// @fastpkg/image only.
import sharp, {
  Sharp,
  Gradient,
  gradient,
  concurrency,
  type Metadata,
  type OutputInfo,
  type ResizeOptions,
  type JpegOptions,
  type PngOptions,
  type WebpOptions,
  type AvifOptions,
  type Region,
  type OverlayOptions,
  type SharpOptions,
  type Stats,
  type FormatEnum,
  type Color,
} from '@fastpkg/image';

const instance: Sharp = sharp('a.png');
const same: sharp.Sharp = instance;
const g: Gradient = gradient('linear').addStop(0, '#000').addStop(1, '#fff');
const g2: sharp.Gradient = new sharp.Gradient('radial');
concurrency(2);
void [same, g, g2];

export async function extras(): Promise<void> {
  const meta: Metadata = await sharp('a.png').probe();
  const info: OutputInfo | undefined = undefined;
  const set: Array<ResizeOptions | JpegOptions | PngOptions | WebpOptions | AvifOptions | Region | OverlayOptions | SharpOptions | Stats | FormatEnum | Color | undefined> = [];
  const outputs = await sharp('a.png').responsive([100, 200], { formats: ['webp', 'jpeg'], quality: 70 });
  const first: { width: number; format: string; data: Buffer } = outputs[0];
  await sharp('a.png')
    .replaceColor('#f00', '#0f0', { tolerance: 10 })
    .duotone('#000', '#fff')
    .gradientMap(g)
    .curves({ rgb: [[0, 0], [255, 255]] })
    .levels({ gamma: 1.1 })
    .alphaFromLuma()
    .toBuffer();
  void [meta, info, set, first];
}
