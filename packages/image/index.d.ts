// ESM entry types. The declarations live in index.d.cts (sharp's `export =` layout, used for
// `require`); this file re-exposes them as a default export plus named exports so that
// `import sharp from '@fastpkg/image'`, `import { Sharp, Metadata } from '@fastpkg/image'` and
// `type M = sharp.Metadata` all work under module node16/nodenext/bundler.
import sharp from './index.cjs';

export default sharp;

// Both a value and a type: re-exported through import aliases so every meaning is kept.
import Sharp = sharp.Sharp;
import Gradient = sharp.Gradient;
export { Sharp, Gradient };

export declare const align: typeof sharp.align;
export declare const blend: typeof sharp.blend;
export declare const block: typeof sharp.block;
export declare const bool: typeof sharp.bool;
export declare const cache: typeof sharp.cache;
export declare const colorspace: typeof sharp.colorspace;
export declare const colourspace: typeof sharp.colourspace;
export declare const concurrency: typeof sharp.concurrency;
export declare const counters: typeof sharp.counters;
export declare const fit: typeof sharp.fit;
export declare const format: typeof sharp.format;
export declare const gradient: typeof sharp.gradient;
export declare const gravity: typeof sharp.gravity;
export declare const interpolators: typeof sharp.interpolators;
export declare const kernel: typeof sharp.kernel;
export declare const linearGradient: typeof sharp.linearGradient;
export declare const position: typeof sharp.position;
export declare const queue: typeof sharp.queue;
export declare const radialGradient: typeof sharp.radialGradient;
export declare const simd: typeof sharp.simd;
export declare const strategy: typeof sharp.strategy;
export declare const unblock: typeof sharp.unblock;
export declare const versions: typeof sharp.versions;

export type {
  AffineOptions,
  AlphaFromLumaOptions,
  AnimationOptions,
  ApplyLutOptions,
  AvailableFormatInfo,
  AvifOptions,
  Blend,
  BlurOptions,
  BoolEnum,
  CacheOptions,
  CacheResult,
  ChannelStats,
  Channels,
  ClaheOptions,
  Color,
  ColorInput,
  Colour,
  ColourBalanceOptions,
  ColourBalanceTriple,
  ColourspaceEnum,
  CommentsMetadata,
  Create,
  CreateChannels,
  CreateRaw,
  CreateText,
  CurvePoints,
  CurvesOptions,
  DepthEnum,
  DuotoneOptions,
  Exif,
  ExifDir,
  ExtendOptions,
  ExtendWith,
  FailOnOptions,
  FillOptions,
  FitEnum,
  FlattenOptions,
  FormatEnum,
  GainMapMetadata,
  GifOptions,
  GradientConstructor,
  GradientLike,
  GradientOptions,
  GradientSpec,
  GradientStop,
  Gravity,
  GravityEnum,
  HeifCompression,
  HeifOptions,
  HeifTune,
  HorizontalAlignment,
  Interpolators,
  Join,
  Jp2InputOptions,
  Jp2Options,
  JpegOptions,
  JxlOptions,
  Kernel,
  KernelEnum,
  LevelMetadata,
  LevelsOptions,
  Matrix2x2,
  Matrix3x3,
  Matrix4x4,
  MediaType,
  Metadata,
  NegateOptions,
  Noise,
  NormaliseOptions,
  OpenSlideInputOptions,
  OutputInfo,
  OutputOptions,
  OverlayOptions,
  PaletteSwapOptions,
  PdfInputOptions,
  PngOptions,
  Precision,
  PresetEnum,
  Raw,
  RawOptions,
  RecolorOptions,
  Region,
  ReplaceColorOptions,
  ResizeOptions,
  ResponsiveOptions,
  ResponsiveOutput,
  RotateOptions,
  SharpConstructor,
  SharpCounters,
  SharpInput,
  SharpOptions,
  SharpenOptions,
  Stats,
  StrategyEnum,
  SvgInputOptions,
  TextAlign,
  TextWrap,
  ThresholdOptions,
  TiffInputOptions,
  TiffOptions,
  TileContainer,
  TileLayout,
  TileOptions,
  TimeoutOptions,
  TrimOptions,
  Unit,
  VerticalAlignment,
  WebpOptions,
  WhiteBalanceOptions,
  WithIccProfileOptions,
  WriteableMetadata,
} from './index.cjs';
