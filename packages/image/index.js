import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const image = require('./lib.cjs');

export default image;
export const Sharp = image.Sharp;
export const cache = image.cache;
export const concurrency = image.concurrency;
export const counters = image.counters;
export const simd = image.simd;
export const format = image.format;
export const versions = image.versions;
export const interpolators = image.interpolators;
export const queue = image.queue;
export const Gradient = image.Gradient;
export const gradient = image.gradient;
export const linearGradient = image.linearGradient;
export const radialGradient = image.radialGradient;
