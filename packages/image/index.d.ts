/// <reference types="node" />

import { Duplex, Readable } from 'node:stream';

export type Input = string | Buffer | Uint8Array | ArrayBuffer | NodeJS.ReadableStream;
export type Fit = 'cover' | 'contain' | 'fill' | 'inside' | 'outside';
export type Kernel =
  | 'nearest'
  | 'linear'
  | 'bilinear'
  | 'cubic'
  | 'mitchell'
  | 'lanczos2'
  | 'lanczos3'
  | 'mks2013'
  | 'mks2021';
export type Format =
  | 'jpeg'
  | 'jpg'
  | 'png'
  | 'webp'
  | 'avif'
  | 'jxl'
  | 'gif'
  | 'qoi'
  | 'raw'
  | 'svg';

export interface InputOptions {
  raw?: { width: number; height: number; channels: number; depth?: 'uchar' | 'ushort'; premultiplied?: boolean };
  create?: { width: number; height: number; channels?: number; background?: string; noise?: number };
  text?: {
    text: string;
    font?: string;
    fontfile?: string;
    width?: number;
    height?: number;
    align?: 'left' | 'center' | 'right';
    dpi?: number;
    rgba?: boolean;
    spacing?: number;
    wrap?: string;
  };
  page?: number;
  pages?: number;
  animated?: boolean;
  density?: number;
  limitInputPixels?: number;
  unlimited?: boolean;
  autoOrient?: boolean;
  failOn?: 'none' | 'truncated' | 'error' | 'warning';
  sequentialRead?: boolean;
  ignoreIcc?: boolean;
}

export interface ResizeOptions {
  width?: number | null;
  height?: number | null;
  fit?: Fit;
  position?: string | { left?: number; top?: number };
  background?: string;
  kernel?: Kernel;
  withoutEnlargement?: boolean;
  withoutReduction?: boolean;
  fastShrinkOnLoad?: boolean;
}

export interface OutputOptions {
  quality?: number;
  lossless?: boolean;
  effort?: number;
  progressive?: boolean;
  compressionLevel?: number;
  palette?: boolean;
  colors?: number;
  dither?: number;
  loop?: number;
  delay?: number | number[];
  pageHeight?: number;
  depth?: string;
  bitdepth?: number;
  chromaSubsampling?: string;
  mozjpeg?: boolean;
  trellisQuantisation?: boolean;
  overshootDeringing?: boolean;
  optimiseScans?: boolean;
  optimiseCoding?: boolean;
  smartSubsample?: boolean;
  nearLossless?: boolean;
  preset?: string;
  timeout?: number;
  resolveWithObject?: boolean;
}

export interface OutputInfo {
  format: Format;
  width: number;
  height: number;
  channels: number;
  size: number;
  space: string;
  depth: string;
  hasAlpha: boolean;
  premultiplied: boolean;
}

export interface Metadata extends OutputInfo {
  pages: number;
  pageHeight: number;
  orientation?: number;
  density?: number;
  hasProfile: boolean;
  hasExif: boolean;
  hasXmp: boolean;
  exif?: Buffer;
  icc?: Buffer;
  xmp?: Buffer;
  comments?: string;
}

export interface Stats {
  channels: Array<{
    min: number;
    max: number;
    sum: number;
    mean: number;
    stdev: number;
    minX: number;
    minY: number;
    maxX: number;
    maxY: number;
  }>;
  isOpaque: boolean;
  entropy: number;
  sharpness: number;
  dominant?: { r: number; g: number; b: number; alpha: number };
}

export type ColorInput = string | { r?: number; g?: number; b?: number; alpha?: number; a?: number };
export type CurvePoints = Array<[number, number]> | number[];

export interface GradientStop {
  offset: number;
  color: ColorInput;
}

export interface GradientSpec {
  type?: 'linear' | 'radial';
  angle?: number;
  center?: [number, number];
  radius?: number;
  spread?: 'pad' | 'repeat' | 'reflect';
  stops: GradientStop[] | Array<[number, ColorInput]> | ColorInput[];
}

export interface ReplaceColorOptions {
  tolerance?: number;
  softness?: number;
  amount?: number;
  preserveOpacity?: boolean;
}

export interface RecolorOptions {
  amount?: number;
  preserveOpacity?: boolean;
}

export interface GradientOptions {
  amount?: number;
  preserveOpacity?: boolean;
}

export interface ApplyLutOptions {
  strength?: number;
  interpolation?: 'trilinear' | 'nearest';
}

export interface CurvesOptions {
  rgb?: CurvePoints;
  master?: CurvePoints;
  r?: CurvePoints;
  red?: CurvePoints;
  g?: CurvePoints;
  green?: CurvePoints;
  b?: CurvePoints;
  blue?: CurvePoints;
  points?: CurvePoints;
  channel?: 'rgb' | 'master' | 'r' | 'red' | 'g' | 'green' | 'b' | 'blue';
}

export interface LevelsOptions {
  inputBlack?: number | number[];
  inputWhite?: number | number[];
  gamma?: number | number[];
  outputBlack?: number | number[];
  outputWhite?: number | number[];
}

export interface WhiteBalanceOptions {
  temperature?: number;
  tint?: number;
}

export interface ColourBalanceOptions {
  shadows?: number[] | { r?: number; g?: number; b?: number };
  midtones?: number[] | { r?: number; g?: number; b?: number };
  highlights?: number[] | { r?: number; g?: number; b?: number };
}

export interface FillOptions {
  amount?: number;
  preserveOpacity?: boolean;
}

export interface PaletteSwapOptions {
  dither?: number;
  preserveOpacity?: boolean;
}

export interface AlphaFromLumaOptions {
  channel?: 'luma' | 'r' | 'red' | 'g' | 'green' | 'b' | 'blue' | 'a' | 'alpha';
  invert?: boolean;
  multiply?: boolean;
  scale?: number;
  offset?: number;
}

export interface GradientConstructor {
  new (type?: 'linear' | 'radial'): Gradient;
  prototype: Gradient;
}

export class Gradient {
  constructor(type?: 'linear' | 'radial');
  type(value: 'linear' | 'radial'): this;
  linear(angle?: number): this;
  radial(cx?: number, cy?: number, radius?: number): this;
  angle(value: number): this;
  center(x: number, y: number): this;
  radius(value: number): this;
  spread(value: 'pad' | 'repeat' | 'reflect'): this;
  addStop(offset: number, color: ColorInput): this;
  stop(offset: number, color: ColorInput): this;
  clone(): Gradient;
  toJSON(): GradientSpec;
}

export interface CompositeOptions {
  input: Input;
  blend?: string;
  gravity?: string;
  top?: number;
  left?: number;
  tile?: boolean;
  density?: number;
  premultiplied?: boolean;
}

export interface ConvolveKernel {
  width: number;
  height: number;
  kernel: number[];
  scale?: number;
  offset?: number;
}

export class Sharp extends Duplex {
  constructor(input?: Input | InputOptions, options?: InputOptions);
  clone(): Sharp;

  resize(width?: number | null | ResizeOptions, height?: number | null, options?: ResizeOptions): this;
  extend(options: { width?: number; height?: number; background?: string; position?: string; extendWith?: 'background' | 'copy' | 'repeat' | 'mirror' }): this;
  extract(options: { left: number; top: number; width: number; height: number }): this;
  trim(options?: { background?: string; threshold?: number; lineArt?: boolean }): this;
  rotate(angle?: number, options?: { background?: string }): this;
  autoOrient(): this;
  flip(flag?: boolean): this;
  flop(flag?: boolean): this;
  affine(matrix: number[] | number[][], options?: { idx?: number; idy?: number; odx?: number; ody?: number; background?: string; interpolator?: string }): this;
  sharpen(options?: number | { sigma?: number; m1?: number; m2?: number; x1?: number; y2?: number; y3?: number }): this;
  median(size?: number): this;
  blur(sigma?: number | { sigma: number }): this;
  dilate(width?: number): this;
  erode(width?: number): this;
  flatten(options?: { background?: string }): this;
  unflatten(): this;
  gamma(gamma?: number, gammaOut?: number): this;
  negate(options?: { alpha?: boolean }): this;
  normalise(options?: { lower?: number; upper?: number }): this;
  normalize(options?: { lower?: number; upper?: number }): this;
  clahe(options: { width: number; height: number; maxSlope?: number }): this;
  convolve(kernel: ConvolveKernel): this;
  threshold(threshold?: number | { threshold?: number; greyscale?: boolean; grayscale?: boolean }, options?: { greyscale?: boolean; grayscale?: boolean }): this;
  boolean(operand: Input, operator: 'and' | 'or' | 'eor', options?: { raw?: InputOptions['raw'] }): this;
  linear(a?: number | number[], b?: number | number[]): this;
  recomb(matrix: number[][]): this;
  modulate(options?: { brightness?: number; saturation?: number; hue?: number; lightness?: number }): this;
  tint(color: string): this;
  greyscale(): this;
  grayscale(): this;
  removeAlpha(): this;
  ensureAlpha(alpha?: number): this;
  extractChannel(channel: number | 'red' | 'green' | 'blue' | 'alpha'): this;
  joinChannel(input: Input, options?: { raw?: InputOptions['raw'] }): this;
  bandbool(operator: 'and' | 'or' | 'eor'): this;
  composite(images: CompositeOptions | CompositeOptions[]): this;

  replaceColor(from: ColorInput, to: ColorInput, options?: ReplaceColorOptions): this;
  recolor(color: ColorInput, options?: RecolorOptions): this;
  duotone(shadows: ColorInput, highlights: ColorInput, options?: RecolorOptions): this;
  duotone(options: { shadows: ColorInput; highlights: ColorInput; amount?: number; preserveOpacity?: boolean }): this;
  gradientMap(gradient: Gradient | GradientSpec | ColorInput[] | Array<[number, ColorInput]>, options?: GradientOptions): this;
  gradientRecolor(gradient: Gradient | GradientSpec | ColorInput[] | Array<[number, ColorInput]>, options?: GradientOptions): this;
  gradientFill(gradient: Gradient | GradientSpec | ColorInput[] | Array<[number, ColorInput]>, options?: GradientOptions): this;
  applyLut(lut: string | Buffer, options?: ApplyLutOptions): this;
  curves(value: CurvesOptions | CurvePoints): this;
  levels(options?: LevelsOptions): this;
  whiteBalance(options?: WhiteBalanceOptions): this;
  colourBalance(options?: ColourBalanceOptions): this;
  colorBalance(options?: ColourBalanceOptions): this;
  posterize(levels?: number | { levels?: number }): this;
  solarize(options?: number | { threshold?: number }): this;
  fill(colorOrGradient: ColorInput | Gradient | GradientSpec, options?: FillOptions): this;
  paletteSwap(palette: ColorInput[] | { palette: ColorInput[] }, options?: PaletteSwapOptions): this;
  alphaFromLuma(options?: AlphaFromLumaOptions): this;
  toFormat(format: Format | string, options?: OutputOptions): this;
  jpeg(options?: OutputOptions): this;
  png(options?: OutputOptions): this;
  webp(options?: OutputOptions): this;
  gif(options?: OutputOptions): this;
  avif(options?: OutputOptions): this;
  jxl(options?: OutputOptions): this;
  qoi(options?: OutputOptions): this;
  raw(options?: OutputOptions): this;
  toColourspace(space: string): this;
  toColorspace(space: string): this;
  pipelineColourspace(space: string): this;
  pipelineColorspace(space: string): this;
  tile(options?: { size?: number; overlap?: number; background?: string; container?: 'fs' | 'zip'; layout?: 'dz' | 'iiif' | 'iiif3' | 'zoomify' | 'google'; skipBlanks?: number; depth?: string; id?: string }): this;
  heif(options?: OutputOptions): never;
  tiff(options?: OutputOptions): never;
  jp2(options?: OutputOptions): never;

  keepMetadata(): this;
  withMetadata(options?: Record<string, unknown>): this;
  keepExif(): this;
  withExif(exif: string | Record<string, unknown>): this;
  withExifMerge(exif: string | Record<string, unknown>): this;
  keepIccProfile(): this;
  withIccProfile(profile: string): this;
  keepXmp(): this;
  withXmp(xmp: string): this;
  keepGainMap(): this;
  withGainMap(gainmap?: unknown): this;
  withDensity(dpi: number): this;
  timeout(options: number | { seconds: number }): this;

  metadata(): Promise<Metadata>;
  stats(): Promise<Stats>;
  probe(): Promise<Metadata>;
  dominant(): Promise<Stats['dominant']>;
  toBuffer(options?: OutputOptions & { resolveWithObject?: false }): Promise<Buffer>;
  toBuffer(options: OutputOptions & { resolveWithObject: true }): Promise<{ data: Buffer; info: OutputInfo }>;
  toBuffer(callback: (error: Error | null, data: Buffer, info: OutputInfo) => void): Promise<undefined>;
  toBuffer(options: OutputOptions, callback: (error: Error | null, data: Buffer, info: OutputInfo) => void): Promise<undefined>;
  toUint8Array(options?: OutputOptions): Promise<Uint8Array>;
  toFile(path: string, options?: OutputOptions): Promise<OutputInfo>;
  toFile(path: string, callback: (error: Error | null, info: OutputInfo) => void): Promise<undefined>;
  toStream(): Readable;
  responsive(widths: number[], options?: OutputOptions & { formats?: Format[] }): Promise<Array<{ width: number; format: Format; data: Buffer }>>;
}

declare function image(input?: Input | InputOptions, options?: InputOptions): Sharp;

declare namespace image {
  export { Sharp, Input, InputOptions, ResizeOptions, OutputOptions, OutputInfo, Metadata, Stats, CompositeOptions, ConvolveKernel };
  export const Gradient: GradientConstructor;
  export const cache: (options?: Record<string, unknown>) => Record<string, number>;
  export const concurrency: (value?: number) => number;
  export const counters: () => { queue: number; process: number };
  export const simd: (value?: boolean) => boolean;
  export const format: Record<string, Record<string, boolean>>;
  export const versions: Record<string, string>;
  export const interpolators: Record<string, string>;
  export const queue: NodeJS.EventEmitter;
  export function gradient(type?: 'linear' | 'radial'): Gradient;
  export function linearGradient(angle?: number): Gradient;
  export function radialGradient(cx?: number, cy?: number, radius?: number): Gradient;
  export function block(options?: Record<string, unknown>): void;
  export function unblock(options?: Record<string, unknown>): void;
}

export default image;
export const cache: typeof image.cache;
export const concurrency: typeof image.concurrency;
export const counters: typeof image.counters;
export const simd: typeof image.simd;
export const format: typeof image.format;
export const versions: typeof image.versions;
export const interpolators: typeof image.interpolators;
export const queue: typeof image.queue;
export const gradient: typeof image.gradient;
export const linearGradient: typeof image.linearGradient;
export const radialGradient: typeof image.radialGradient;
