// Structural compatibility with sharp's own typings, in the direction that matters when code
// written against sharp is handed an @fastpkg/image object (and the reverse for plain data types).
import fp, { type Sharp as FpSharp, type Metadata as FpMetadata, type OutputInfo as FpOutputInfo, type ResizeOptions as FpResize, type SharpOptions as FpSharpOptions, type OverlayOptions as FpOverlay, type Stats as FpStats } from '@fastpkg/image';
import sh, { type Sharp as ShSharp, type Metadata as ShMetadata, type OutputInfo as ShOutputInfo, type ResizeOptions as ShResize, type SharpOptions as ShSharpOptions, type OverlayOptions as ShOverlay, type Stats as ShStats } from 'sharp';

// fastpkg instance usable wherever a sharp instance is expected
export const instance: ShSharp = {} as FpSharp;
// plain data types are interchangeable both ways
export const m1: ShMetadata = {} as FpMetadata;
export const m2: FpMetadata = {} as ShMetadata;
export const o1: ShOutputInfo = {} as FpOutputInfo;
export const o2: FpOutputInfo = {} as ShOutputInfo;
export const r1: ShResize = {} as FpResize;
export const r2: FpResize = {} as ShResize;
export const s1: ShSharpOptions = {} as FpSharpOptions;
export const s2: FpSharpOptions = {} as ShSharpOptions;
export const v1: ShOverlay = {} as FpOverlay;
export const v2: FpOverlay = {} as ShOverlay;
export const t1: ShStats = {} as FpStats;
export const t2: FpStats = {} as ShStats;
// results of calls are interchangeable
export const call: ShSharp = fp('a.png');
