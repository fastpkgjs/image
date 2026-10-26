'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { Duplex, Readable, PassThrough } = require('node:stream');
const { EventEmitter } = require('node:events');
const zlib = require('node:zlib');

const PLATFORM_PACKAGES = {
  'win32-x64': '@fastpkg/image-win32-x64',
  'win32-arm64': '@fastpkg/image-win32-arm64',
  'darwin-x64': '@fastpkg/image-darwin-x64',
  'darwin-arm64': '@fastpkg/image-darwin-arm64',
  'linux-x64-gnu': '@fastpkg/image-linux-x64-gnu',
  'linux-arm64-gnu': '@fastpkg/image-linux-arm64-gnu',
  'linux-x64-musl': '@fastpkg/image-linux-x64-musl',
  'linux-arm64-musl': '@fastpkg/image-linux-arm64-musl',
};

let muslCache;
function isMusl() {
  if (process.platform !== 'linux') return false;
  if (muslCache !== undefined) return muslCache;
  muslCache = detectMusl();
  return muslCache;
}

function detectMusl() {
  // glibc builds of Node report the runtime glibc version; musl builds do not.
  try {
    if (process.report && typeof process.report.getReport === 'function') {
      const previous = process.report.excludeNetwork;
      process.report.excludeNetwork = true;
      try {
        const header = process.report.getReport().header;
        if (header && header.glibcVersionRuntime) return false;
      } finally {
        process.report.excludeNetwork = previous;
      }
    }
  } catch {
    // fall through to filesystem probes
  }
  try {
    for (const dir of ['/lib', '/usr/lib']) {
      if (fs.readdirSync(dir).some((name) => name.startsWith('ld-musl-'))) return true;
    }
  } catch {
    // ignore
  }
  try {
    return fs.readFileSync('/usr/bin/ldd', 'utf8').includes('musl');
  } catch {
    return false;
  }
}

function platformKey() {
  const base = `${process.platform}-${process.arch}`;
  return process.platform === 'linux' ? `${base}-${isMusl() ? 'musl' : 'gnu'}` : base;
}

function describePlatform() {
  const libc = process.platform === 'linux' ? ` (${isMusl() ? 'musl' : 'glibc'})` : '';
  return `${process.platform}-${process.arch}${libc}`;
}

function firstLine(error) {
  return String((error && error.message) || error).split('\n')[0];
}

function loadNative() {
  const dir = __dirname;
  const key = platformKey();
  const platformPackage = PLATFORM_PACKAGES[key];
  const failures = [];
  if (platformPackage) {
    try {
      return require(platformPackage);
    } catch (error) {
      failures.push(`${platformPackage}: ${firstLine(error)}`);
      if (process.env.FASTPKG_DEBUG) console.error(`[@fastpkg/image] failed to load ${platformPackage}`, error);
    }
  }
  let candidates = [];
  try {
    candidates = fs
      .readdirSync(dir)
      .filter((name) => name.startsWith('image') && name.endsWith('.node'));
  } catch {
    // directory unreadable; report below
  }
  const ordered = candidates.sort((a, b) => {
    const platform = `image.${key}`;
    return Number(b.startsWith(platform)) - Number(a.startsWith(platform));
  });
  for (const name of ordered) {
    try {
      return require(path.join(dir, name));
    } catch (error) {
      failures.push(`${name}: ${firstLine(error)}`);
      if (process.env.FASTPKG_DEBUG) console.error(`[@fastpkg/image] failed to load ${name}`, error);
    }
  }
  const lines = [`@fastpkg/image could not load its native addon on ${describePlatform()}, Node ${process.version}.`];
  if (!platformPackage) {
    lines.push(
      `No prebuilt binary exists for ${key}. Supported: ${Object.keys(PLATFORM_PACKAGES).join(', ')}.`
    );
  } else {
    lines.push(`Expected the optional dependency ${platformPackage}.`);
    lines.push(
      'If it is missing, reinstall with optional dependencies enabled (`npm install --include=optional`, ' +
        'and do not use `--no-optional` / `--omit=optional`). When copying node_modules between machines or ' +
        'libc variants, reinstall instead of copying, and note that lockfiles created with an older npm may ' +
        'need `rm -rf node_modules package-lock.json && npm install`.'
    );
  }
  if (failures.length) lines.push(`Load errors:\n  ${failures.join('\n  ')}`);
  lines.push('When working from a git checkout, run `npm run build:native` from the repository root.');
  lines.push('Set FASTPKG_DEBUG=1 for full errors. Report problems at https://github.com/fastpkgjs/image/issues');
  throw new Error(lines.join('\n'));
}

const native = loadNative();
const EMPTY = Buffer.alloc(0);
const queue = new EventEmitter();
let queueDepth = 0;

const FORMAT_ALIASES = {
  jpg: 'jpeg',
  jpe: 'jpeg',
  apng: 'png',
  'image/jpeg': 'jpeg',
  'image/png': 'png',
  'image/webp': 'webp',
  'image/avif': 'avif',
  'image/jxl': 'jxl',
  'image/gif': 'gif',
  'image/qoi': 'qoi',
};

function normalizeFormat(format) {
  if (typeof format !== 'string') return format;
  return FORMAT_ALIASES[format.toLowerCase()] || format.toLowerCase();
}

// Output extensions that select an encoder; anything else falls back to the input format (sharp).
// tif/tiff/heic/heif/jp2 are listed so they reach the encoder and fail with a clear message.
const OUTPUT_EXTENSIONS = new Set(['jpeg', 'jpg', 'png', 'webp', 'gif', 'avif', 'jxl', 'qoi', 'raw', 'tif', 'tiff', 'heic', 'heif', 'jp2']);

function sameFile(a, b) {
  const resolve = (p) => {
    try {
      return fs.realpathSync(p);
    } catch {
      return path.resolve(p);
    }
  };
  return resolve(a) === resolve(b);
}

function isStream(value) {
  return value && typeof value.pipe === 'function' && typeof value.on === 'function';
}

function toBufferLike(input) {
  if (Buffer.isBuffer(input)) return input;
  if (input instanceof Uint8Array) {
    return Buffer.from(input.buffer, input.byteOffset, input.byteLength);
  }
  if (input instanceof ArrayBuffer) return Buffer.from(input);
  if (ArrayBuffer.isView(input)) {
    return Buffer.from(input.buffer, input.byteOffset, input.byteLength);
  }
  return null;
}

function readInput(input) {
  if (typeof input === 'string') {
    if (input.length === 0) throw new Error("Unsupported input '' of type string");
    return input;
  }
  const buffer = toBufferLike(input);
  if (buffer) return buffer; // an empty Buffer is reported when the pipeline runs
  let shown;
  try {
    shown = String(input);
  } catch {
    shown = typeof input;
  }
  throw new Error(`Unsupported input '${shown}' of type ${typeof input}`);
}

function normalizeInputOptions(options = {}) {
  const out = {};
  const mapping = {
    page: 'page',
    pages: 'pages',
    animated: 'animated',
    density: 'density',
    limitInputPixels: 'limitInputPixels',
    unlimited: 'unlimited',
    autoOrient: 'autoOrient',
    failOn: 'failOn',
    sequentialRead: 'sequentialRead',
    ignoreIcc: 'ignoreIcc',
  };
  for (const [from, to] of Object.entries(mapping)) {
    if (is.defined(options[from])) out[to] = options[from];
  }
  if (typeof out.density === 'number') out.density = Math.max(1, Math.round(out.density));
  // sharp: limitInputPixels false|0 disables the limit, true keeps the default.
  if (out.limitInputPixels === false || out.limitInputPixels === 0) {
    delete out.limitInputPixels;
    out.unlimited = true;
  } else if (out.limitInputPixels === true) {
    delete out.limitInputPixels;
  }
  const stripNull = (o) => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== null && v !== undefined));
  // sharp: pages -1 means "all pages"; the engine decodes the first frame only.
  if (out.pages === -1) delete out.pages;
  if (options.raw) out.raw = stripNull(options.raw);
  if (options.create) {
    out.create = stripNull(options.create);
    if (out.create.background !== undefined) out.create.background = colorValue(out.create.background);
  }
  if (options.text) out.text = stripNull(options.text);
  return out;
}

// sharp accepts CSS-style strings or `{ r, g, b, alpha }` objects wherever a colour is expected.
function colorValue(value, fallback = '#000000') {
  if (value === undefined || value === null) return fallback;
  const c = parseColour(value);
  const hex = (n) => n.toString(16).padStart(2, '0');
  return `#${hex(c.r)}${hex(c.g)}${hex(c.b)}${hex(c.a)}`;
}

// ---------------------------------------------------------------------------------------------
// Argument validation (mirrors sharp's synchronous checks and messages).
// ---------------------------------------------------------------------------------------------
const is = {
  defined: (v) => v !== undefined && v !== null,
  object: (v) => typeof v === 'object',
  plainObject: (v) => Object.prototype.toString.call(v) === '[object Object]',
  bool: (v) => typeof v === 'boolean',
  number: (v) => typeof v === 'number' && Number.isFinite(v),
  integer: (v) => Number.isInteger(v),
  string: (v) => typeof v === 'string' && v.length > 0,
  inRange: (v, min, max) => v >= min && v <= max,
  inArray: (v, list) => list.includes(v),
};

function invalid(name, expected, actual) {
  let actualString;
  try {
    actualString = String(actual).slice(0, 100);
  } catch {
    // Symbol-keyed or null-prototype values cannot be stringified; sharp ignores that too.
  }
  return new TypeError(`Expected ${expected} for ${name} but received ${actualString} of type ${typeof actual}`);
}

function checkBool(name, value) {
  if (!is.bool(value)) throw invalid(name, 'boolean', value);
  return value;
}

// Colours: sharp accepts an object or a CSS colour string of 3-200 characters.
function checkColour(value, name = 'background') {
  if (!(is.object(value) && value !== null) && !(typeof value === 'string' && value.length >= 3 && value.length <= 200)) {
    throw invalid(name, 'object or string', value);
  }
  try {
    parseColour(value);
  } catch (error) {
    if (error instanceof Error && error.message.startsWith('Unable to parse')) error.message += ` for ${name}`;
    throw error;
  }
  return value;
}

const GRAVITY_NAMES = ['centre', 'north', 'east', 'south', 'west', 'northeast', 'southeast', 'southwest', 'northwest'];
const FITS = ['contain', 'cover', 'fill', 'inside', 'outside'];
const KERNELS = ['nearest', 'linear', 'cubic', 'mitchell', 'lanczos2', 'lanczos3', 'mks2013', 'mks2021'];
const EXTEND_WITH = ['background', 'copy', 'repeat', 'mirror'];
const GRAVITY = { center: 0, centre: 0, north: 1, east: 2, south: 3, west: 4, northeast: 5, southeast: 6, southwest: 7, northwest: 8 };
const POSITION = { top: 1, right: 2, bottom: 3, left: 4, 'right top': 5, 'right bottom': 6, 'left bottom': 7, 'left top': 8 };
const STRATEGY = { entropy: 16, attention: 17 };
const BLENDS = {
  clear: 'clear', source: 'source', over: 'over', in: 'in', out: 'out', atop: 'atop', dest: 'dest',
  'dest-over': 'dest-over', 'dest-in': 'dest-in', 'dest-out': 'dest-out', 'dest-atop': 'dest-atop', xor: 'xor',
  add: 'add', saturate: 'saturate', multiply: 'multiply', screen: 'screen', overlay: 'overlay', darken: 'darken',
  lighten: 'lighten', 'colour-dodge': 'colour-dodge', 'color-dodge': 'colour-dodge', 'colour-burn': 'colour-burn',
  'color-burn': 'colour-burn', 'hard-light': 'hard-light', 'soft-light': 'soft-light', difference: 'difference',
  exclusion: 'exclusion',
};
const ALIGN = { left: 'low', top: 'low', low: 'low', center: 'centre', centre: 'centre', right: 'high', bottom: 'high', high: 'high' };
const OUTPUT_FORMATS = new Set(['heic', 'heif', 'avif', 'jpeg', 'jpg', 'jpe', 'tile', 'dz', 'png', 'raw', 'tiff', 'tif', 'webp', 'gif', 'jp2', 'jpx', 'j2k', 'j2c', 'jxl']);

function checkPosition(value) {
  const pos = is.integer(value) ? value : STRATEGY[value] || POSITION[value] || GRAVITY[value];
  if (is.integer(pos) && (is.inRange(pos, 0, 8) || is.inRange(pos, 16, 17))) return value;
  throw invalid('position', 'valid position/gravity/strategy', value);
}

function checkIntRange(name, value, min, max, prefix = '') {
  if (!(is.integer(value) && is.inRange(value, min, max))) {
    throw invalid(name, `${prefix}integer between ${min} and ${max}`, value);
  }
  return value;
}

function checkNumRange(name, value, min, max) {
  if (!(is.number(value) && is.inRange(value, min, max))) {
    throw invalid(name, `number between ${min} and ${max}`, value);
  }
  return value;
}

// Validates the per-input options object (constructor options or composite input options).
function validateInputOptions(o) {
  if (o === undefined || o === null) return;
  if (!is.object(o)) throw new Error(`Invalid input options ${o}`);
  if (is.defined(o.failOn) && !(is.string(o.failOn) && is.inArray(o.failOn, ['none', 'truncated', 'error', 'warning']))) {
    throw invalid('failOn', 'one of: none, truncated, error, warning', o.failOn);
  }
  if (is.defined(o.autoOrient)) checkBool('autoOrient', o.autoOrient);
  if (is.defined(o.density) && !(is.number(o.density) && is.inRange(o.density, 1, 100000))) {
    throw invalid('density', 'number between 1 and 100000', o.density);
  }
  if (is.defined(o.ignoreIcc)) checkBool('ignoreIcc', o.ignoreIcc);
  if (is.defined(o.limitInputPixels) && !is.bool(o.limitInputPixels)) {
    if (!(is.integer(o.limitInputPixels) && is.inRange(o.limitInputPixels, 0, Number.MAX_SAFE_INTEGER))) {
      throw invalid('limitInputPixels', 'positive integer', o.limitInputPixels);
    }
  }
  if (is.defined(o.limitInputChannels) && !is.bool(o.limitInputChannels)) {
    if (!(is.integer(o.limitInputChannels) && is.inRange(o.limitInputChannels, 0, Number.MAX_SAFE_INTEGER))) {
      throw invalid('limitInputChannels', 'positive integer', o.limitInputChannels);
    }
  }
  if (is.defined(o.unlimited)) checkBool('unlimited', o.unlimited);
  if (is.defined(o.sequentialRead)) checkBool('sequentialRead', o.sequentialRead);
  if (is.defined(o.raw)) {
    const r = o.raw;
    if (
      !(is.object(r) &&
        is.integer(r.width) && is.inRange(r.width, 1, 100000000) &&
        is.integer(r.height) && is.inRange(r.height, 1, 100000000) &&
        is.integer(r.channels) && is.inRange(r.channels, 1, 4))
    ) {
      throw new Error('Expected width, height and channels for raw pixel input');
    }
    if (is.defined(r.premultiplied)) checkBool('raw.premultiplied', r.premultiplied);
    if (is.defined(r.pageHeight)) {
      if (!(is.integer(r.pageHeight) && r.pageHeight > 0 && r.pageHeight <= r.height)) {
        throw invalid('raw.pageHeight', 'positive integer', r.pageHeight);
      }
      if (r.height % r.pageHeight !== 0) {
        throw new Error(`Expected raw.height ${r.height} to be a multiple of raw.pageHeight ${r.pageHeight}`);
      }
    }
  }
  if (is.defined(o.animated)) checkBool('animated', o.animated);
  if (is.defined(o.pages)) checkIntRange('pages', o.pages, -1, 100000);
  if (is.defined(o.page)) checkIntRange('page', o.page, 0, 100000);
  if (is.object(o.svg) && o.svg !== null) {
    if (is.defined(o.svg.stylesheet) && typeof o.svg.stylesheet !== 'string') throw invalid('svg.stylesheet', 'string', o.svg.stylesheet);
    if (is.defined(o.svg.highBitdepth)) checkBool('svg.highBitdepth', o.svg.highBitdepth);
  }
  if (is.defined(o.create)) {
    const c = o.create;
    if (
      !(is.object(c) &&
        is.integer(c.width) && is.inRange(c.width, 1, 100000000) &&
        is.integer(c.height) && is.inRange(c.height, 1, 100000000) &&
        is.integer(c.channels))
    ) {
      throw new Error('Expected valid width, height and channels to create a new input image');
    }
    if (is.defined(c.pageHeight)) {
      if (!(is.integer(c.pageHeight) && c.pageHeight > 0 && c.pageHeight <= c.height)) {
        throw invalid('create.pageHeight', 'positive integer', c.pageHeight);
      }
      if (c.height % c.pageHeight !== 0) {
        throw new Error(`Expected create.height ${c.height} to be a multiple of create.pageHeight ${c.pageHeight}`);
      }
    }
    if (is.defined(c.noise)) {
      if (!is.object(c.noise)) throw new Error('Expected noise to be an object');
      if (c.noise.type !== 'gaussian') throw new Error('Only gaussian noise is supported at the moment');
      if (!is.inRange(c.channels, 1, 4)) throw invalid('create.channels', 'number between 1 and 4', c.channels);
      if (is.defined(c.noise.mean) && !(is.number(c.noise.mean) && is.inRange(c.noise.mean, 0, 10000))) {
        throw invalid('create.noise.mean', 'number between 0 and 10000', c.noise.mean);
      }
      if (is.defined(c.noise.sigma) && !(is.number(c.noise.sigma) && is.inRange(c.noise.sigma, 0, 10000))) {
        throw invalid('create.noise.sigma', 'number between 0 and 10000', c.noise.sigma);
      }
    } else if (is.defined(c.background)) {
      if (!is.inRange(c.channels, 3, 4)) throw invalid('create.channels', 'number between 3 and 4', c.channels);
      checkColour(c.background, 'background');
    } else {
      throw new Error('Expected valid noise or background to create a new input image');
    }
  }
  if (is.defined(o.text)) {
    const t = o.text;
    if (!(is.object(t) && is.string(t.text))) throw new Error('Expected a valid string to create an image with text.');
    if (is.defined(t.height) && is.defined(t.dpi)) throw new Error('Expected only one of dpi or height');
    if (is.defined(t.font) && !is.string(t.font)) throw invalid('text.font', 'string', t.font);
    if (is.defined(t.fontfile) && !is.string(t.fontfile)) throw invalid('text.fontfile', 'string', t.fontfile);
    if (is.defined(t.width)) checkIntRange('text.width', t.width, 1, 1000000);
    if (is.defined(t.height)) checkIntRange('text.height', t.height, 1, 1000000);
    if (is.defined(t.align) && !(is.string(t.align) && is.string(ALIGN[t.align]))) throw invalid('text.align', 'valid alignment', t.align);
    if (is.defined(t.justify) && !is.bool(t.justify)) throw invalid('text.justify', 'boolean', t.justify);
    if (is.defined(t.dpi)) checkIntRange('text.dpi', t.dpi, 1, 1000000);
    if (is.defined(t.rgba) && !is.bool(t.rgba)) throw invalid('text.rgba', 'bool', t.rgba);
    if (is.defined(t.spacing)) checkIntRange('text.spacing', t.spacing, -1000000, 1000000);
    if (is.defined(t.wrap) && !(is.string(t.wrap) && is.inArray(t.wrap, ['word', 'char', 'word-char', 'none']))) {
      throw invalid('text.wrap', 'one of: word, char, word-char, none', t.wrap);
    }
  }
}

function checkAnimation(o) {
  if (!is.object(o) || o === null) return;
  if (is.defined(o.loop)) checkIntRange('loop', o.loop, 0, 65535);
  if (is.defined(o.delay)) {
    const d = o.delay;
    const ok = is.integer(d)
      ? is.inRange(d, 0, 65535)
      : Array.isArray(d) && is.inRange(d.length, 1, 1000000) && d.every(is.integer) && d.every((v) => is.inRange(v, 0, 65535));
    if (!ok) throw invalid('delay', 'integer or an array of integers between 0 and 65535', d);
  }
}

// Output-format option validation; returns nothing, throws sharp-style errors.
function nullOptions(o, prop) {
  // sharp dereferences the options object, so null fails with a TypeError.
  if (o === null) throw new TypeError(`Cannot read properties of null (reading '${prop}')`);
}

function validateOutputOptions(format, o) {
  nullOptions(o, { jpeg: 'quality', png: 'progressive', webp: 'quality', gif: 'reuse', raw: 'depth' }[format] || 'quality');
  const obj = is.object(o) && o !== null;
  switch (format) {
    case 'jpeg':
      if (!obj) return;
      if (is.defined(o.quality)) checkIntRange('quality', o.quality, 1, 100);
      if (is.defined(o.progressive)) checkBool('jpegProgressive', o.progressive);
      if (is.defined(o.chromaSubsampling) && !(is.string(o.chromaSubsampling) && is.inArray(o.chromaSubsampling, ['4:2:0', '4:4:4']))) {
        throw invalid('chromaSubsampling', 'one of: 4:2:0, 4:4:4', o.chromaSubsampling);
      }
      for (const [names, key] of [
        [['optimizeCoding', 'optimiseCoding'], 'jpegOptimiseCoding'],
        [['trellisQuantization', 'trellisQuantisation'], 'jpegTrellisQuantisation'],
        [['overshootDeringing'], 'jpegOvershootDeringing'],
        [['optimizeScans', 'optimiseScans'], 'jpegOptimiseScans'],
      ]) {
        const v = names.map((n) => o[n]).find((x) => is.bool(x)) ?? o[names[names.length - 1]];
        if (is.defined(v)) checkBool(key, v);
      }
      if (is.defined(o.mozjpeg)) {
        if (!is.bool(o.mozjpeg)) throw invalid('mozjpeg', 'boolean', o.mozjpeg);
      }
      if (is.defined(o.parallel)) {
        checkBool('jpegParallel', o.parallel);
        // Banded encoding only produces plain baseline streams; asking for a progressive or
        // scan-optimised file as well is a contradiction, not something to ignore silently.
        if (o.parallel && (o.progressive === true || o.mozjpeg === true || o.optimiseScans === true || o.optimizeScans === true)) {
          throw new TypeError('parallel cannot be combined with progressive/mozjpeg encoding');
        }
      }
      {
        const q = is.number(o.quantizationTable) ? o.quantizationTable : o.quantisationTable;
        if (is.defined(q) && !(is.integer(q) && is.inRange(q, 0, 8))) throw invalid('quantisationTable', 'integer between 0 and 8', q);
      }
      return;
    case 'png': {
      if (!obj) return;
      if (is.defined(o.progressive)) checkBool('pngProgressive', o.progressive);
      if (is.defined(o.compressionLevel)) checkIntRange('compressionLevel', o.compressionLevel, 0, 9);
      if (is.defined(o.adaptiveFiltering)) checkBool('pngAdaptiveFiltering', o.adaptiveFiltering);
      const colours = o.colours || o.colors;
      if (is.defined(colours)) checkIntRange('colours', colours, 2, 256);
      let palette = false;
      if (is.defined(o.palette)) palette = checkBool('pngPalette', o.palette);
      else if ([o.quality, o.effort, o.colours, o.colors, o.dither].some(is.defined)) palette = true;
      if (palette) {
        if (is.defined(o.quality)) checkIntRange('quality', o.quality, 0, 100);
        if (is.defined(o.effort)) checkIntRange('effort', o.effort, 1, 10);
        if (is.defined(o.dither)) checkNumRange('dither', o.dither, 0, 1);
      }
      return;
    }
    case 'webp':
      if (obj) {
        if (is.defined(o.quality)) checkIntRange('quality', o.quality, 1, 100);
        if (is.defined(o.alphaQuality)) checkIntRange('alphaQuality', o.alphaQuality, 0, 100);
        for (const key of ['lossless', 'nearLossless', 'smartSubsample', 'smartDeblock', 'minSize', 'mixed', 'exact']) {
          if (is.defined(o[key])) checkBool(`webp${key[0].toUpperCase()}${key.slice(1)}`, o[key]);
        }
        if (is.defined(o.preset) && !(is.string(o.preset) && is.inArray(o.preset, ['default', 'photo', 'picture', 'drawing', 'icon', 'text']))) {
          throw invalid('preset', 'one of: default, photo, picture, drawing, icon, text', o.preset);
        }
        if (is.defined(o.effort)) checkIntRange('effort', o.effort, 0, 6);
      }
      checkAnimation(o);
      return;
    case 'gif':
      if (obj) {
        if (is.defined(o.reuse)) checkBool('gifReuse', o.reuse);
        if (is.defined(o.progressive)) checkBool('gifProgressive', o.progressive);
        const colours = o.colours || o.colors;
        if (is.defined(colours)) checkIntRange('colours', colours, 2, 256);
        if (is.defined(o.effort)) checkIntRange('effort', o.effort, 1, 10);
        if (is.defined(o.dither)) checkNumRange('dither', o.dither, 0, 1);
        if (is.defined(o.interFrameMaxError)) checkNumRange('interFrameMaxError', o.interFrameMaxError, 0, 32);
        if (is.defined(o.interPaletteMaxError)) checkNumRange('interPaletteMaxError', o.interPaletteMaxError, 0, 256);
        if (is.defined(o.keepDuplicateFrames)) checkBool('keepDuplicateFrames', o.keepDuplicateFrames);
      }
      checkAnimation(o);
      return;
    case 'avif':
    case 'heif':
      if (!obj) return;
      if (is.defined(o.quality)) checkIntRange('quality', o.quality, 1, 100);
      if (is.defined(o.lossless) && !is.bool(o.lossless)) throw invalid('lossless', 'boolean', o.lossless);
      if (is.defined(o.effort)) checkIntRange('effort', o.effort, 0, 9);
      if (is.defined(o.chromaSubsampling) && !(is.string(o.chromaSubsampling) && is.inArray(o.chromaSubsampling, ['4:2:0', '4:4:4']))) {
        throw invalid('chromaSubsampling', 'one of: 4:2:0, 4:4:4', o.chromaSubsampling);
      }
      if (is.defined(o.bitdepth) && !(is.integer(o.bitdepth) && is.inArray(o.bitdepth, [8, 10, 12]))) {
        throw invalid('bitdepth', '8, 10 or 12', o.bitdepth);
      }
      if (is.defined(o.tune) && !(is.string(o.tune) && is.inArray(o.tune, ['auto', 'iq', 'psnr', 'ssim']))) {
        throw invalid('tune', 'one of: auto, iq, psnr, ssim', o.tune);
      }
      return;
    case 'jxl':
      if (obj) {
        if (is.defined(o.quality)) checkIntRange('quality', o.quality, 1, 100);
        else if (is.defined(o.distance)) checkNumRange('distance', o.distance, 0, 15);
        if (is.defined(o.decodingTier)) checkIntRange('decodingTier', o.decodingTier, 0, 4);
        if (is.defined(o.lossless) && !is.bool(o.lossless)) throw invalid('lossless', 'boolean', o.lossless);
        if (is.defined(o.effort)) checkIntRange('effort', o.effort, 1, 9);
      }
      checkAnimation(o);
      return;
    case 'raw':
      if (obj && is.defined(o.depth)) {
        const depths = ['char', 'uchar', 'short', 'ushort', 'int', 'uint', 'float', 'complex', 'double', 'dpcomplex'];
        if (!(is.string(o.depth) && is.inArray(o.depth, depths))) {
          throw invalid('depth', `one of: ${depths.join(', ')}`, o.depth);
        }
      }
      return;
    default:
  }
}


// CSS colour parsing (strings, { r, g, b, alpha } objects), equivalent to what sharp accepts.
const COLOUR_NAMES = new Map(
  'aliceblue:f0f8ff,antiquewhite:faebd7,aqua:00ffff,aquamarine:7fffd4,azure:f0ffff,beige:f5f5dc,bisque:ffe4c4,black:000000,blanchedalmond:ffebcd,blue:0000ff,blueviolet:8a2be2,brown:a52a2a,burlywood:deb887,cadetblue:5f9ea0,chartreuse:7fff00,chocolate:d2691e,coral:ff7f50,cornflowerblue:6495ed,cornsilk:fff8dc,crimson:dc143c,cyan:00ffff,darkblue:00008b,darkcyan:008b8b,darkgoldenrod:b8860b,darkgray:a9a9a9,darkgreen:006400,darkgrey:a9a9a9,darkkhaki:bdb76b,darkmagenta:8b008b,darkolivegreen:556b2f,darkorange:ff8c00,darkorchid:9932cc,darkred:8b0000,darksalmon:e9967a,darkseagreen:8fbc8f,darkslateblue:483d8b,darkslategray:2f4f4f,darkslategrey:2f4f4f,darkturquoise:00ced1,darkviolet:9400d3,deeppink:ff1493,deepskyblue:00bfff,dimgray:696969,dimgrey:696969,dodgerblue:1e90ff,firebrick:b22222,floralwhite:fffaf0,forestgreen:228b22,fuchsia:ff00ff,gainsboro:dcdcdc,ghostwhite:f8f8ff,gold:ffd700,goldenrod:daa520,gray:808080,green:008000,greenyellow:adff2f,grey:808080,honeydew:f0fff0,hotpink:ff69b4,indianred:cd5c5c,indigo:4b0082,ivory:fffff0,khaki:f0e68c,lavender:e6e6fa,lavenderblush:fff0f5,lawngreen:7cfc00,lemonchiffon:fffacd,lightblue:add8e6,lightcoral:f08080,lightcyan:e0ffff,lightgoldenrodyellow:fafad2,lightgray:d3d3d3,lightgreen:90ee90,lightgrey:d3d3d3,lightpink:ffb6c1,lightsalmon:ffa07a,lightseagreen:20b2aa,lightskyblue:87cefa,lightslategray:778899,lightslategrey:778899,lightsteelblue:b0c4de,lightyellow:ffffe0,lime:00ff00,limegreen:32cd32,linen:faf0e6,magenta:ff00ff,maroon:800000,mediumaquamarine:66cdaa,mediumblue:0000cd,mediumorchid:ba55d3,mediumpurple:9370db,mediumseagreen:3cb371,mediumslateblue:7b68ee,mediumspringgreen:00fa9a,mediumturquoise:48d1cc,mediumvioletred:c71585,midnightblue:191970,mintcream:f5fffa,mistyrose:ffe4e1,moccasin:ffe4b5,navajowhite:ffdead,navy:000080,oldlace:fdf5e6,olive:808000,olivedrab:6b8e23,orange:ffa500,orangered:ff4500,orchid:da70d6,palegoldenrod:eee8aa,palegreen:98fb98,paleturquoise:afeeee,palevioletred:db7093,papayawhip:ffefd5,peachpuff:ffdab9,peru:cd853f,pink:ffc0cb,plum:dda0dd,powderblue:b0e0e6,purple:800080,rebeccapurple:663399,red:ff0000,rosybrown:bc8f8f,royalblue:4169e1,saddlebrown:8b4513,salmon:fa8072,sandybrown:f4a460,seagreen:2e8b57,seashell:fff5ee,sienna:a0522d,silver:c0c0c0,skyblue:87ceeb,slateblue:6a5acd,slategray:708090,slategrey:708090,snow:fffafa,springgreen:00ff7f,steelblue:4682b4,tan:d2b48c,teal:008080,thistle:d8bfd8,tomato:ff6347,turquoise:40e0d0,violet:ee82ee,wheat:f5deb3,white:ffffff,whitesmoke:f5f5f5,yellow:ffff00,yellowgreen:9acd32'.split(',').map((entry) => {
    const [name, hex] = entry.split(':');
    return [name, [parseInt(hex.slice(0, 2), 16), parseInt(hex.slice(2, 4), 16), parseInt(hex.slice(4, 6), 16)]];
  })
);
const NUM = '[+-]?(?:\\d*\\.)?\\d+(?:e[+-]?\\d+)?';
const RGB_RE = new RegExp(`^rgba?\\(\\s*(${NUM})(%?)\\s*[,\\s]\\s*(${NUM})(%?)\\s*[,\\s]\\s*(${NUM})(%?)\\s*(?:[,/]\\s*(${NUM})(%?))?\\s*\\)$`, 'i');
const HSL_RE = new RegExp(`^(hsla?|hwb)\\(\\s*(${NUM})(?:deg)?\\s*[,\\s]\\s*(${NUM})%\\s*[,\\s]\\s*(${NUM})%\\s*(?:[,/]\\s*(${NUM})(%?))?\\s*\\)$`, 'i');
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

function hslToRgb(h, s, l) {
  h = (((h % 360) + 360) % 360) / 360;
  s = clamp(s, 0, 100) / 100;
  l = clamp(l, 0, 100) / 100;
  if (s === 0) return [l * 255, l * 255, l * 255];
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  const f = (t) => {
    t = (t + 1) % 1;
    if (t < 1 / 6) return p + (q - p) * 6 * t;
    if (t < 1 / 2) return q;
    if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
    return p;
  };
  return [f(h + 1 / 3) * 255, f(h) * 255, f(h - 1 / 3) * 255];
}

function hwbToRgb(h, w, b) {
  w = clamp(w, 0, 100) / 100;
  b = clamp(b, 0, 100) / 100;
  if (w + b >= 1) {
    const g = (w / (w + b)) * 255;
    return [g, g, g];
  }
  return hslToRgb(h, 100, 50).map((c) => (c / 255) * (1 - w - b) * 255 + w * 255);
}

// Returns { r, g, b, a } with integer channels 0-255.
function parseColour(value) {
  let rgb;
  let alpha = 1;
  if (typeof value === 'string') {
    const unable = () => new Error(`Unable to parse color from string: ${value}`);
    if (value.length < 3 || value.length > 200) throw invalid('background', 'object or string', value);
    const lower = value.toLowerCase();
    let m;
    if (lower[0] === '#') {
      if (!/^#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/.test(lower)) throw unable();
      let hex = lower.slice(1);
      if (hex.length <= 4) hex = [...hex].map((c) => c + c).join('');
      rgb = [0, 2, 4].map((o) => parseInt(hex.slice(o, o + 2), 16));
      if (hex.length === 8) alpha = parseInt(hex.slice(6, 8), 16) / 255;
    } else if (COLOUR_NAMES.has(lower)) {
      rgb = COLOUR_NAMES.get(lower);
    } else if (lower === 'transparent') {
      rgb = [0, 0, 0];
      alpha = 0;
    } else if ((m = RGB_RE.exec(value))) {
      rgb = [1, 3, 5].map((i) => (m[i + 1] ? (parseFloat(m[i]) / 100) * 255 : parseFloat(m[i])));
      if (m[7] !== undefined) alpha = m[8] ? parseFloat(m[7]) / 100 : parseFloat(m[7]);
    } else if ((m = HSL_RE.exec(value))) {
      const a = [parseFloat(m[2]), parseFloat(m[3]), parseFloat(m[4])];
      rgb = m[1].toLowerCase() === 'hwb' ? hwbToRgb(...a) : hslToRgb(...a);
      if (m[5] !== undefined) alpha = m[6] ? parseFloat(m[5]) / 100 : parseFloat(m[5]);
    } else {
      throw unable();
    }
  } else if (is.object(value) && value !== null && !Array.isArray(value)) {
    const unable = () => {
      let shown;
      try {
        shown = JSON.stringify(value);
      } catch {
        shown = String(value);
      }
      return new Error(`Unable to parse color from object: ${shown}`);
    };
    if (is.number(value.r) && is.number(value.g) && is.number(value.b)) {
      rgb = [value.r, value.g, value.b];
      if (value.alpha !== undefined) {
        if (!is.number(value.alpha)) throw unable();
        alpha = value.alpha;
      }
    } else if (is.number(value.h) && is.number(value.s) && is.number(value.l)) {
      rgb = hslToRgb(value.h, value.s, value.l);
      if (value.alpha !== undefined) {
        if (!is.number(value.alpha)) throw unable();
        alpha = value.alpha;
      }
    } else {
      throw unable();
    }
  } else if (is.object(value) && value !== null) {
    throw new Error(`Unable to parse color from object: ${JSON.stringify(value)}`);
  } else {
    throw invalid('background', 'object or string', value);
  }
  return {
    r: Math.round(clamp(rgb[0], 0, 255)),
    g: Math.round(clamp(rgb[1], 0, 255)),
    b: Math.round(clamp(rgb[2], 0, 255)),
    a: Math.round(clamp(alpha, 0, 1) * 255),
  };
}

// napi-rs prefixes messages with the status word ("InvalidArg, ..."); a Rust panic surfaces as a
// "task panicked" join error. Present both as ordinary Error messages.
const NATIVE_STATUS = /^(?:InvalidArg|GenericFailure|Unknown|Cancelled|StringExpected|NumberExpected|BooleanExpected|FunctionExpected|PendingException|GenerateError|ObjectExpected|ArrayBufferExpected|DetachableArraybufferExpected|WouldDeadlock|EscapeCalledTwice|HandleScopeMismatch|CallbackScopeMismatch|QueueFull|Closing|BigintExpected|DateExpected|ArrayExpected|ArrayBufferTypeError), /;
function cleanNativeError(error) {
  if (!(error instanceof Error) || typeof error.message !== 'string') return error;
  let message = error.message;
  for (let i = 0; i < 3 && NATIVE_STATUS.test(message); i++) message = message.replace(NATIVE_STATUS, '');
  if (/task \d+ panicked|panicked at|task panicked/.test(message)) {
    message = `Internal error while processing image: ${message.replace(/^.*?panicked(?: with message)?:?\s*/s, '').split('\n')[0].slice(0, 200) || 'worker panic'}`;
  }
  if (message !== error.message) error.message = message;
  return error;
}


// Built-in ICC profiles for withIccProfile('srgb' | 'p3'): minimal ICC v4 matrix/TRC display
// profiles generated on demand (colorants adapted to the D50 profile connection space, sRGB
// parametric transfer curve, which Display P3 shares).
const BUILTIN_PROFILES = {
  srgb: { name: 'sRGB', primaries: [[0.43607, 0.22249, 0.01392], [0.38515, 0.71687, 0.09708], [0.14307, 0.06061, 0.7141]] },
  p3: { name: 'Display P3', primaries: [[0.51512, 0.2412, -0.00105], [0.29198, 0.69225, 0.04189], [0.1571, 0.06657, 0.78407]] },
};
const builtinProfileCache = new Map();

function builtinIccProfile(key) {
  const spec = BUILTIN_PROFILES[key];
  if (!spec) return undefined;
  if (builtinProfileCache.has(key)) return builtinProfileCache.get(key);
  const s15 = (v) => {
    const b = Buffer.alloc(4);
    b.writeInt32BE(Math.round(v * 65536));
    return b;
  };
  const pad4 = (buf) => (buf.length % 4 ? Buffer.concat([buf, Buffer.alloc(4 - (buf.length % 4))]) : buf);
  const mluc = (text) => {
    const utf16 = Buffer.from(text, 'utf16le').swap16();
    const head = Buffer.alloc(28);
    head.write('mluc', 0, 'ascii');
    head.writeUInt32BE(1, 8); // one record
    head.writeUInt32BE(12, 12); // record size
    head.write('enUS', 16, 'ascii');
    head.writeUInt32BE(utf16.length, 20);
    head.writeUInt32BE(28, 24); // string offset
    return pad4(Buffer.concat([head, utf16]));
  };
  const xyz = (v) => Buffer.concat([Buffer.from('XYZ \0\0\0\0', 'binary'), s15(v[0]), s15(v[1]), s15(v[2])]);
  const para = Buffer.concat([
    Buffer.from('para\0\0\0\0', 'binary'),
    Buffer.from([0, 3, 0, 0]), // function type 3, reserved
    ...[2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045].map(s15),
  ]);
  const tags = [
    ['desc', mluc(spec.name)],
    ['cprt', mluc('No copyright, use freely')],
    ['wtpt', xyz([0.9642, 1, 0.8249])],
    ['rXYZ', xyz(spec.primaries[0])],
    ['gXYZ', xyz(spec.primaries[1])],
    ['bXYZ', xyz(spec.primaries[2])],
    ['rTRC', para],
    ['gTRC', null],
    ['bTRC', null],
  ];
  const table = Buffer.alloc(4 + tags.length * 12);
  table.writeUInt32BE(tags.length, 0);
  let offset = 128 + table.length;
  const bodies = [];
  const placed = new Map();
  tags.forEach(([sig, data], i) => {
    // gTRC / bTRC share rTRC's data, as the spec allows.
    let at;
    let size;
    if (data === null) {
      ({ at, size } = placed.get('rTRC'));
    } else {
      size = data.length;
      at = offset;
      bodies.push(data);
      offset += data.length;
      placed.set(sig, { at, size });
    }
    table.write(sig, 4 + i * 12, 'ascii');
    table.writeUInt32BE(at, 8 + i * 12);
    table.writeUInt32BE(size, 12 + i * 12);
  });
  const header = Buffer.alloc(128);
  header.writeUInt32BE(offset, 0);
  header.writeUInt32BE(0x04300000, 8); // v4.3
  header.write('mntr', 12, 'ascii');
  header.write('RGB ', 16, 'ascii');
  header.write('XYZ ', 20, 'ascii');
  header.writeUInt16BE(2024, 24);
  header.writeUInt16BE(1, 26);
  header.writeUInt16BE(1, 28);
  header.write('acsp', 36, 'ascii');
  s15(0.9642).copy(header, 68);
  s15(1).copy(header, 72);
  s15(0.8249).copy(header, 76);
  const profile = Buffer.concat([header, table, ...bodies]);
  builtinProfileCache.set(key, profile);
  return profile;
}

// ---------------------------------------------------------------------------------------------
// Operation order
//
// sharp does not run operations in call order: libvips' pipeline (src/pipeline.cc) has a fixed
// sequence of stages, and call order only matters for the few cases handled by `rotateBefore`
// (see normalizeOperations). The spec's operation array is therefore re-ordered into sharp's
// stage order before it reaches the native layer. Operations sharp has no stage for (the engine's
// own extensions, gamma which spans two stages, ...) act as barriers: nothing moves across them.
// ---------------------------------------------------------------------------------------------
const STAGE_FIXED = {
  autoOrient: 10,
  trim: 20,
  flatten: 40,
  greyscale: 41,
  resize: 50,
  joinChannel: 70,
  affine: 100,
  extend: 110,
  median: 120,
  threshold: 121,
  dilate: 122,
  erode: 123,
  blur: 124,
  unflatten: 125,
  convolve: 126,
  recomb: 127,
  modulate: 128,
  sharpen: 129,
  composite: 140,
  linear: 151,
  normalise: 152,
  clahe: 153,
  boolean: 154,
  bandbool: 155,
  tint: 156,
  removeAlpha: 157,
  ensureAlpha: 158,
  extractChannel: 160,
  negate: 170,
};

function isQuarterTurn(op) {
  return Number.isFinite(op.angle) && op.angle % 90 === 0;
}

function normalizeOperations(operations) {
  if (operations.length < 2) return operations;
  // Replay the option bag sharp builds while the methods are called (resize.js / operation.js).
  let rotation = false;
  let resizeExpected = false;
  let widthPre = false;
  let widthPost = false;
  let rotateBefore = false;
  const extractStage = new Map();
  for (const op of operations) {
    switch (op.op) {
      case 'rotate':
        rotation = Number.isFinite(op.angle) && op.angle % 360 !== 0;
        break;
      case 'resize':
        resizeExpected = (op.width !== undefined && op.width !== null) || (op.height !== undefined && op.height !== null);
        if (rotation && resizeExpected) rotateBefore = true;
        break;
      case 'extract':
        if (resizeExpected || widthPre) {
          widthPost = true;
          extractStage.set(op, 90);
        } else {
          widthPre = true;
          extractStage.set(op, 30);
        }
        if (rotation && !resizeExpected && (!widthPre || !widthPost)) rotateBefore = true;
        break;
      case 'trim':
        if (rotation) rotateBefore = true;
        break;
      default:
    }
  }
  const stageOf = (op) => {
    switch (op.op) {
      case 'flip':
        return rotateBefore ? 11 : 60;
      case 'flop':
        return rotateBefore ? 12 : 61;
      case 'rotate':
        if (isQuarterTurn(op)) return rotateBefore ? 13 : 62;
        return rotateBefore ? 14 : 80;
      case 'extract':
        return extractStage.get(op);
      default:
        return STAGE_FIXED[op.op] ?? null;
    }
  };
  const out = [];
  let segment = [];
  const flush = () => {
    segment.sort((a, b) => a.stage - b.stage || a.index - b.index);
    for (const entry of segment) out.push(entry.op);
    segment = [];
  };
  operations.forEach((op, index) => {
    const stage = stageOf(op);
    if (stage === null || stage === undefined) {
      flush();
      out.push(op);
    } else {
      segment.push({ op, stage, index });
    }
  });
  flush();
  // sharp crops / pads (`cover` / `contain`) only after the flips and quarter turns that follow
  // the resample, so those must run in between.
  const resizeAt = out.findIndex((op) => op.op === 'resize');
  if (resizeAt !== -1) {
    const resize = out[resizeAt];
    const fit = resize.fit || 'cover';
    const hasTarget = (resize.width !== undefined && resize.width !== null) || (resize.height !== undefined && resize.height !== null);
    let end = resizeAt + 1;
    while (end < out.length && [60, 61, 62].includes(stageOf(out[end]))) end += 1;
    if (end > resizeAt + 1 && hasTarget && (fit === 'cover' || fit === 'contain')) {
      out[resizeAt] = { ...resize, deferCanvas: true };
      out.splice(end, 0, {
        op: 'canvas',
        width: resize.width,
        height: resize.height,
        fit,
        ...(resize.position !== undefined ? { position: resize.position } : {}),
        ...(resize.background !== undefined ? { background: resize.background } : {}),
      });
    }
  }
  return out;
}

function setOperation(instance, op) {
  // sharp treats null like undefined for optional values.
  for (const key of Object.keys(op)) if (op[key] === null) delete op[key];
  instance._spec.operations.push(op);
  return instance;
}

function mapResizeOptions(options = {}) {
  const out = {};
  const keys = [
    'fit',
    'position',
    'background',
    'kernel',
    'withoutEnlargement',
    'withoutReduction',
    'fastShrinkOnLoad',
  ];
  for (const key of keys) {
    if (is.defined(options[key])) out[key] = options[key];
  }
  if (out.background !== undefined) out.background = colorValue(out.background);
  if (out.position !== null && out.position !== undefined) {
    out.position = positionName(out.position);
  }
  return out;
}

function positionName(position) {
  // sharp.gravity constants (0-8). sharp.strategy.entropy/attention (16/17) are content-aware
  // crops that this engine approximates with the centre.
  if (typeof position === 'number') {
    if (position === 16 || position === 17) return 'centre';
    if (Number.isInteger(position) && position >= 0 && position <= 8) return GRAVITY_NAMES[position];
    throw new Error(`Expected valid position/gravity/strategy for position but received ${position} of type number`);
  }
  if (position === 'entropy' || position === 'attention') return 'centre';
  if (!position || typeof position !== 'object') return position;
  if (position.left !== undefined && position.top !== undefined) return 'top-left';
  if (position.left !== undefined) return 'left';
  if (position.top !== undefined) return 'top';
  return 'centre';
}

function mapOutputOptions(options = {}, format) {
  const out = {};
  const keys = [
    'quality',
    'lossless',
    'effort',
    'progressive',
    'chromaSubsampling',
    'compressionLevel',
    'palette',
    'dither',
    'delay',
    'pageHeight',
    'depth',
    'mozjpeg',
    'trellisQuantisation',
    'overshootDeringing',
    'optimiseScans',
    'optimiseCoding',
    'parallel',
    'smartSubsample',
    'nearLossless',
    'exact',
    'alphaQuality',
    'preset',
    'bitdepth',
    'timeout',
  ];
  for (const key of keys) {
    if (is.defined(options[key])) out[key] = options[key];
  }
  // US/UK spellings and sharp's `colours`.
  const alias = (to, ...froms) => {
    for (const from of froms) if (options[from] !== undefined && is.bool(options[from])) out[to] = options[from];
  };
  const colours = options.colours || options.colors;
  if (is.number(colours)) out.colors = colours;
  alias('optimiseCoding', 'optimizeCoding');
  alias('trellisQuantisation', 'trellisQuantization');
  alias('optimiseScans', 'optimizeScans');
  if (options.optimizeScans === true || options.optimiseScans === true) out.progressive = true;
  if (options.mozjpeg === true) {
    // sharp: mozjpeg implies trellis quantisation, overshoot deringing, scan optimisation and progressive output.
    out.trellisQuantisation = true;
    out.overshootDeringing = true;
    out.optimiseScans = true;
    out.progressive = true;
  }
  if (is.defined(options.effort)) out.effortSet = true;
  if (is.defined(options.quality)) out.qualitySet = true;
  if (format === 'png') {
    // sharp: any of quality/effort/colours/dither switches palette output on unless `palette`
    // is given explicitly.
    if (!is.defined(options.palette) && [options.quality, options.effort, options.colours, options.colors, options.dither].some(is.defined)) {
      out.palette = true;
    }
    if (is.bool(options.adaptiveFiltering)) out.adaptiveFiltering = options.adaptiveFiltering;
  }
  if (is.defined(options.loop)) out.loopCount = options.loop;
  if (is.defined(options.delay)) out.delay = Array.isArray(options.delay) ? options.delay : [options.delay];
  return out;
}

function colorString(value) {
  if (typeof value === 'string') return value;
  if (value && typeof value === 'object') {
    const r = Number(value.r ?? value.red ?? 0) | 0;
    const g = Number(value.g ?? value.green ?? 0) | 0;
    const b = Number(value.b ?? value.blue ?? 0) | 0;
    const rawAlpha = value.alpha ?? value.a ?? 1;
    const alpha = Math.max(0, Math.min(255, rawAlpha <= 1 ? rawAlpha * 255 : rawAlpha));
    return `rgba(${r},${g},${b},${(alpha / 255).toFixed(6)})`;
  }
  throw new TypeError('expected a color string or {r,g,b,alpha} object');
}

function gradientStops(stops) {
  if (!Array.isArray(stops)) throw new TypeError('gradient stops must be an array');
  return stops.map((stop) => {
    if (Array.isArray(stop)) {
      return { offset: Number(stop[0]), color: colorString(stop[1]) };
    }
    if (stop && typeof stop === 'object') {
      return { offset: Number(stop.offset), color: colorString(stop.color) };
    }
    throw new TypeError('gradient stop must be [offset, color] or {offset, color}');
  });
}

function gradientSpec(value) {
  if (value instanceof Gradient) return value.toJSON();
  if (Array.isArray(value)) {
    const plainColors = value.every(
      (entry) => typeof entry === 'string' || (entry && typeof entry === 'object' && !Array.isArray(entry) && 'r' in entry)
    );
    const stops = plainColors
      ? value.map((color, index) => ({
          offset: value.length > 1 ? index / (value.length - 1) : 0,
          color: colorString(color),
        }))
      : gradientStops(value);
    return { type: 'linear', angle: 0, center: [0.5, 0.5], radius: 0.5, spread: 'pad', stops };
  }
  if (value && typeof value === 'object' && Array.isArray(value.stops)) {
    return {
      type: value.type || value.kind || 'linear',
      angle: Number(value.angle ?? 0),
      center: Array.isArray(value.center) ? value.center.map(Number) : [0.5, 0.5],
      radius: Number(value.radius ?? 0.5),
      spread: value.spread || 'pad',
      stops: gradientStops(value.stops),
    };
  }
  throw new TypeError('expected a Gradient, gradient spec, or array of stops');
}

function curvePoints(points) {
  if (!points) return [];
  if (!Array.isArray(points)) throw new TypeError('curve points must be an array');
  if (points.every((point) => typeof point === 'number')) {
    return points.map((y, index) => [index / Math.max(1, points.length - 1), y > 1 ? y / 255 : y]);
  }
  return points.map((point) => {
    if (!Array.isArray(point) || point.length < 2) {
      throw new TypeError('curve points must be [x, y] pairs');
    }
    let x = Number(point[0]);
    let y = Number(point[1]);
    if (x > 1 || y > 1) {
      x /= 255;
      y /= 255;
    }
    return [x, y];
  });
}

function curveSpec(value) {
  if (Array.isArray(value)) {
    return { master: curvePoints(value), red: [], green: [], blue: [] };
  }
  const options = value || {};
  if (options.points && options.channel) {
    const points = curvePoints(options.points);
    const channel = String(options.channel).toLowerCase();
    return {
      master: channel === 'rgb' || channel === 'master' ? points : [],
      red: channel === 'r' || channel === 'red' ? points : [],
      green: channel === 'g' || channel === 'green' ? points : [],
      blue: channel === 'b' || channel === 'blue' ? points : [],
    };
  }
  return {
    master: curvePoints(options.rgb || options.master || options.points),
    red: curvePoints(options.r || options.red),
    green: curvePoints(options.g || options.green),
    blue: curvePoints(options.b || options.blue),
  };
}

function channelShift(value) {
  if (!value) return [0, 0, 0];
  if (Array.isArray(value)) return [Number(value[0] || 0), Number(value[1] || 0), Number(value[2] || 0)];
  return [Number(value.r || 0), Number(value.g || 0), Number(value.b || 0)];
}

let crcTable;
function crc32(buffer) {
  if (!crcTable) {
    crcTable = new Uint32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      crcTable[n] = c >>> 0;
    }
  }
  let crc = 0xffffffff;
  for (let i = 0; i < buffer.length; i++) crc = crcTable[(crc ^ buffer[i]) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function dosDateTime(date = new Date()) {
  const time = ((date.getHours() & 31) << 11) | ((date.getMinutes() & 63) << 5) | ((date.getSeconds() / 2) & 31);
  const day = (((date.getFullYear() - 1980) & 127) << 9) | (((date.getMonth() + 1) & 15) << 5) | (date.getDate() & 31);
  return { time, day };
}

function zipBuffer(files) {
  const local = [];
  const central = [];
  const { time, day } = dosDateTime();
  let offset = 0;
  for (const [name, data] of files) {
    const nameBytes = Buffer.from(name.replace(/\\/g, '/'), 'utf8');
    const compressed = zlib.deflateRawSync(data, { level: 6 });
    const crc = crc32(data);
    const localHeader = Buffer.alloc(30);
    localHeader.writeUInt32LE(0x04034b50, 0);
    localHeader.writeUInt16LE(20, 4);
    localHeader.writeUInt16LE(0, 6);
    localHeader.writeUInt16LE(8, 8);
    localHeader.writeUInt16LE(time, 10);
    localHeader.writeUInt16LE(day, 12);
    localHeader.writeUInt32LE(crc, 14);
    localHeader.writeUInt32LE(compressed.length, 18);
    localHeader.writeUInt32LE(data.length, 22);
    localHeader.writeUInt16LE(nameBytes.length, 26);
    localHeader.writeUInt16LE(0, 28);
    local.push(localHeader, nameBytes, compressed);
    const centralHeader = Buffer.alloc(46);
    centralHeader.writeUInt32LE(0x02014b50, 0);
    centralHeader.writeUInt16LE(20, 4);
    centralHeader.writeUInt16LE(20, 6);
    centralHeader.writeUInt16LE(0, 8);
    centralHeader.writeUInt16LE(8, 10);
    centralHeader.writeUInt16LE(time, 12);
    centralHeader.writeUInt16LE(day, 14);
    centralHeader.writeUInt32LE(crc, 16);
    centralHeader.writeUInt32LE(compressed.length, 20);
    centralHeader.writeUInt32LE(data.length, 24);
    centralHeader.writeUInt16LE(nameBytes.length, 28);
    centralHeader.writeUInt16LE(0, 30);
    centralHeader.writeUInt16LE(0, 32);
    centralHeader.writeUInt16LE(0, 34);
    centralHeader.writeUInt16LE(0, 36);
    centralHeader.writeUInt32LE(0, 38);
    centralHeader.writeUInt32LE(offset, 42);
    central.push(centralHeader, nameBytes);
    offset += localHeader.length + nameBytes.length + compressed.length;
  }
  const centralSize = central.reduce((sum, part) => sum + part.length, 0);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(0, 4);
  end.writeUInt16LE(0, 6);
  end.writeUInt16LE(files.size, 8);
  end.writeUInt16LE(files.size, 10);
  end.writeUInt32LE(centralSize, 12);
  end.writeUInt32LE(offset, 16);
  end.writeUInt16LE(0, 20);
  return Buffer.concat([...local, ...central, end]);
}

function tilePath(layout, level, col, row, width, height, size, ext, tileIndex = 0) {
  switch (layout) {
    case 'google':
      return `${level}/${col}_${row}.${ext}`;
    case 'zoomify': {
      const group = Math.floor(tileIndex / 256);
      return `TileGroup${group}/${level}-${col}-${row}.${ext}`;
    }
    case 'iiif':
    case 'iiif3':
      return `${level}/${col}_${row}.${ext}`;
    default:
      return `${layout === 'dz' ? `${level}/` : `${level}/`}${col}_${row}.${ext}`;
  }
}

class Gradient {
  constructor(type = 'linear') {
    this._spec = {
      type,
      angle: 0,
      center: [0.5, 0.5],
      radius: 0.5,
      spread: 'pad',
      stops: [],
    };
  }

  type(value) {
    this._spec.type = value;
    return this;
  }

  linear(angle = 0) {
    this._spec.type = 'linear';
    this._spec.angle = Number(angle);
    return this;
  }

  radial(cx = 0.5, cy = 0.5, radius = 0.5) {
    this._spec.type = 'radial';
    this._spec.center = [Number(cx), Number(cy)];
    this._spec.radius = Number(radius);
    return this;
  }

  angle(value) {
    this._spec.angle = Number(value);
    return this;
  }

  center(x, y) {
    this._spec.center = [Number(x), Number(y)];
    return this;
  }

  radius(value) {
    this._spec.radius = Number(value);
    return this;
  }

  spread(value) {
    this._spec.spread = value;
    return this;
  }

  addStop(offset, color) {
    this._spec.stops.push({ offset: Number(offset), color: colorString(color) });
    return this;
  }

  stop(offset, color) {
    return this.addStop(offset, color);
  }

  clone() {
    const copy = new Gradient(this._spec.type);
    copy._spec = JSON.parse(JSON.stringify(this._spec));
    return copy;
  }

  toJSON() {
    return {
      ...this._spec,
      center: [...this._spec.center],
      stops: [...this._spec.stops]
        .map((stop) => ({ ...stop }))
        .sort((a, b) => a.offset - b.offset),
    };
  }
}

// Overlay descriptor: encoded bytes (path/Buffer/typed array), or a generated input
// ({ create } / { text }) or raw pixels ({ raw } option on a Buffer) described by `source`.
function parseCompositeInput(entry) {
  const value = entry;
  const input = value.input;
  const opts = {};
  for (const key of ['failOn', 'autoOrient', 'density', 'ignoreIcc', 'limitInputPixels', 'limitInputChannels', 'unlimited', 'sequentialRead', 'raw', 'animated', 'pages', 'page', 'svg']) {
    if (is.defined(value[key])) opts[key] = value[key];
  }
  let bytes;
  let source;
  if (is.plainObject(input) && !Object.keys(opts).length) {
    validateInputOptions(input);
    if (!(input.create || input.text)) throw new Error(`Unsupported input '${input}' of type ${typeof input}`);
    source = normalizeInputOptions(input);
    bytes = EMPTY;
  } else {
    validateInputOptions(opts);
    if (typeof input === 'string') {
      if (!input.length) throw new Error("Unsupported input '' of type string");
      bytes = fs.readFileSync(input);
    } else {
      bytes = toBufferLike(input);
      if (!bytes) {
        let shown;
        try {
          shown = String(input);
        } catch {
          shown = typeof input;
        }
        throw new Error(`Unsupported input '${shown}' of type ${typeof input}${Object.keys(opts).length ? ` when also providing options of type object` : ''}`);
      }
      if (bytes.length === 0) throw new Error('Input Buffer is empty');
    }
    if (Object.keys(opts).length) source = normalizeInputOptions(opts);
  }
  return { value, bytes, source };
}

class Sharp extends Duplex {
  constructor(input, options = {}) {
    if (arguments.length === 1 && !is.defined(input)) throw new Error('Invalid input');
    super({ readableObjectMode: false, writableObjectMode: false });
    nullOptions(options, 'failOn');
    if (!is.object(options) && options !== undefined) throw new Error(`Invalid input options ${options}`);
    validateInputOptions(options);
    this._inputFromStream = false;
    this._chunks = null;
    this._inputPromise = null;

    if (input && typeof input === 'object' && !Buffer.isBuffer(input) && !(input instanceof Uint8Array) && !(input instanceof ArrayBuffer) && !ArrayBuffer.isView(input) && !isStream(input)) {
      const source = { ...input };
      validateInputOptions(source);
      if (source.create || source.text) {
        this._input = EMPTY;
        this._spec = { input: { ...normalizeInputOptions(source), ...normalizeInputOptions(options) }, operations: [], output: {} };
        return;
      }
    }

    if (isStream(input)) {
      this._input = null;
      this._inputFromStream = true;
      this._chunks = [];
      this._inputPromise = new Promise((resolve, reject) => {
        this.once('finish', () => resolve(Buffer.concat(this._chunks)));
        this.once('error', reject);
      });
      input.once('error', (error) => this.destroy(error));
      input.pipe(this);
      this._spec = { input: normalizeInputOptions(options), operations: [], output: {} };
      return;
    }

    if (input === undefined) {
      this._input = null;
      this._chunks = [];
      // sharp semantics: terminal methods on a write-fed instance wait for the input to end.
      this._inputReady = new Promise((resolve) => {
        this._inputResolve = resolve;
      });
      this._spec = { input: normalizeInputOptions(options), operations: [], output: {} };
      return;
    }

    this._input = readInput(input);
    this._spec = { input: normalizeInputOptions(options), operations: [], output: {} };
  }

  _write(chunk, encoding, callback) {
    if (!this._chunks) this._chunks = [];
    this._chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk, encoding));
    callback();
  }

  _read() {}

  _final(callback) {
    if (this._inputFromStream) {
      callback();
      return;
    }
    const input = this._input !== null ? this._input : Buffer.concat(this._chunks || []);
    if (this._inputResolve) this._inputResolve(input);
    for (const clone of this._clones || []) {
      clone._chunks = [input];
      if (clone._inputResolve) clone._inputResolve(input);
      clone.end();
    }
    queueDepth += 1;
    queue.emit('change', queueDepth);
    native
      .process(input, this._snap())
      .then((result) => {
        queueDepth = Math.max(0, queueDepth - 1);
        queue.emit('change', queueDepth);
        this.push(result.data);
        this.push(null);
        callback();
      })
      .catch((error) => {
        queueDepth = Math.max(0, queueDepth - 1);
        queue.emit('change', queueDepth);
        callback(this._fail(error));
      });
  }

  // sharp snapshots its options when a terminal method is called; later chaining must not leak
  // into a job that is already pending. Operations are immutable once pushed, so a shallow copy
  // of the containers is enough.
  _snap() {
    return { ...this._spec, input: { ...this._spec.input }, operations: normalizeOperations([...this._spec.operations]), output: { ...this._spec.output } };
  }

  // The native layer reports a missing file as a bare io error; sharp names the path.
  _fail(error) {
    cleanNativeError(error);
    if (typeof this._input === 'string' && error && /No such file|os error [23]\b|cannot find the (file|path)|ENOENT/i.test(String(error.message)) && !String(error.message).includes(this._input)) {
      error.message = `Input file is missing: ${this._input}`;
    }
    return error;
  }

  async _resolvedInput() {
    if (this._input !== null) {
      if (this._input.length === 0 && typeof this._input !== 'string' && !(this._spec.input.create || this._spec.input.text)) {
        throw new Error('Input Buffer is empty');
      }
      return this._input;
    }
    if (this._inputPromise) return this._inputPromise;
    if (this._inputReady) return this._inputReady;
    if (this._chunks && this._chunks.length) return Buffer.concat(this._chunks);
    return EMPTY;
  }

  clone() {
    const copy = Object.create(Sharp.prototype);
    Object.assign(copy, this);
    // Object.assign copied the parent's stream/emitter internals by reference; give the clone its own.
    copy._events = undefined;
    copy._eventsCount = 0;
    copy._readableState = undefined;
    copy._writableState = undefined;
    Duplex.call(copy, { readableObjectMode: false, writableObjectMode: false });
    copy._spec = JSON.parse(JSON.stringify(this._spec));
    copy._chunks = null;
    copy._clones = null;
    copy._inputFromStream = false;
    // A clone of a stream-fed instance shares its input: it receives the data once the parent
    // has been fully written (the documented `pipeline.clone()` fan-out pattern).
    copy._inputPromise = this._inputFromStream ? this._inputPromise : null;
    copy._inputReady = undefined;
    copy._inputResolve = undefined;
    if (this._input === null && !this._inputFromStream) {
      copy._chunks = [];
      copy._inputReady = new Promise((resolve) => {
        copy._inputResolve = resolve;
      });
      (this._clones ||= []).push(copy);
    }
    return copy;
  }

  resize(width, height, options) {
    if (is.defined(width)) {
      if (is.object(width) && !is.defined(options)) {
        options = width;
        width = undefined;
        if (is.defined(height)) checkIntRange('height', height, 1, 100000000, 'positive ');
      } else {
        checkIntRange('width', width, 1, 100000000, 'positive ');
        if (is.defined(height)) checkIntRange('height', height, 1, 100000000, 'positive ');
      }
    } else if (is.defined(height)) {
      checkIntRange('height', height, 1, 100000000, 'positive ');
    }
    options = is.object(options) && options !== null ? options : {};
    if (is.defined(options.width)) checkIntRange('width', options.width, 1, 100000000, 'positive ');
    if (is.defined(options.height)) checkIntRange('height', options.height, 1, 100000000, 'positive ');
    if (is.defined(options.fit) && !FITS.includes(options.fit)) throw invalid('fit', 'valid fit', options.fit);
    if (is.defined(options.position)) checkPosition(options.position);
    if (is.defined(options.background)) checkColour(options.background);
    if (is.defined(options.kernel) && !KERNELS.includes(options.kernel)) throw invalid('kernel', 'valid kernel name', options.kernel);
    for (const key of ['withoutEnlargement', 'withoutReduction', 'fastShrinkOnLoad']) {
      if (is.defined(options[key])) checkBool(key, options[key]);
    }
    if (is.defined(options.width)) width = options.width;
    if (is.defined(options.height)) height = options.height;
    return setOperation(this, {
      op: 'resize',
      width: width === null ? undefined : width,
      height: height === null ? undefined : height,
      ...mapResizeOptions(options),
    });
  }

  extend(options) {
    // sharp accepts a single number (all edges) or { top, left, bottom, right }.
    if (is.integer(options)) {
      checkIntRange('extend', options, 1, 100000000);
      options = { top: options, left: options, bottom: options, right: options };
    } else if (!(is.object(options) && options !== null)) {
      throw invalid('extend', 'integer or object', options);
    }
    const edges = options;
    const edge = (value, name) => {
      if (!is.defined(value)) return undefined;
      return checkIntRange(name, value, 0, 100000000);
    };
    if (is.defined(edges.background)) checkColour(edges.background);
    if (is.defined(edges.extendWith) && !EXTEND_WITH.includes(edges.extendWith)) {
      throw invalid('extendWith', 'one of: background, copy, repeat, mirror', edges.extendWith);
    }
    return setOperation(this, {
      op: 'extend',
      width: is.defined(edges.width) ? checkIntRange('width', edges.width, 0, 100000000) : undefined,
      height: is.defined(edges.height) ? checkIntRange('height', edges.height, 0, 100000000) : undefined,
      top: edge(edges.top, 'top'),
      left: edge(edges.left, 'left'),
      bottom: edge(edges.bottom, 'bottom'),
      right: edge(edges.right, 'right'),
      background: colorValue(edges.background),
      position: positionName(edges.position) || 'centre',
      extendWith: edges.extendWith || 'background',
    });
  }

  extract(options) {
    if (!(is.object(options) && options !== null)) {
      throw invalid('options', 'object with integer left, top, width and height', options);
    }
    for (const name of ['left', 'top', 'width', 'height']) checkIntRange(name, options[name], 0, 100000000);
    return setOperation(this, {
      op: 'extract',
      left: options.left,
      top: options.top,
      width: options.width,
      height: options.height,
    });
  }

  trim(options) {
    let threshold = 10;
    let lineArt = false;
    let background;
    if (is.defined(options)) {
      if (!is.object(options)) throw invalid('trim', 'object', options);
      if (is.defined(options.background)) background = checkColour(options.background);
      if (is.defined(options.threshold)) {
        if (!(is.number(options.threshold) && options.threshold >= 0)) throw invalid('threshold', 'positive number', options.threshold);
        threshold = options.threshold;
      }
      if (is.defined(options.lineArt)) lineArt = checkBool('trimLineArt', options.lineArt);
      if (is.defined(options.margin)) checkIntRange('margin', options.margin, 0, 10000000);
    }
    return setOperation(this, {
      op: 'trim',
      background: background === undefined ? undefined : colorValue(background),
      threshold,
      lineArt,
      margin: is.defined(options) && is.defined(options.margin) ? options.margin : 0,
    });
  }

  rotate(angle, options = {}) {
    if (!is.defined(angle)) return this.autoOrient();
    if (!is.number(angle)) throw invalid('angle', 'numeric', angle);
    if (!(Number.isInteger(angle) && angle % 90 === 0) && is.object(options) && options !== null && options.background) {
      checkColour(options.background);
    }
    // sharp keeps a single rotation: the last rotate(angle) call wins.
    this._spec.operations = this._spec.operations.filter((op) => op.op !== 'rotate');
    return setOperation(this, {
      op: 'rotate',
      angle: Number(angle) || 0,
      background: colorValue(options.background),
    });
  }

  autoOrient() {
    return setOperation(this, { op: 'autoOrient' });
  }

  // flip()/flop() are boolean flags in sharp: calling them twice does not undo the flip.
  flip(flag = true) {
    this._spec.operations = this._spec.operations.filter((op) => op.op !== 'flip');
    return flag === false ? this : setOperation(this, { op: 'flip', flip: true });
  }

  flop(flag = true) {
    this._spec.operations = this._spec.operations.filter((op) => op.op !== 'flop');
    return flag === false ? this : setOperation(this, { op: 'flop', flop: true });
  }

  affine(matrix, options) {
    const shapeOk =
      Array.isArray(matrix) &&
      ((matrix.length === 4 && matrix.every(is.number)) ||
        (matrix.length === 2 && matrix.every((row) => Array.isArray(row) && row.length === 2)));
    const flat = shapeOk ? matrix.flat() : [];
    if (!(flat.length === 4 && flat.every(is.number))) throw invalid('matrix', '1x4 or 2x2 array', matrix);
    if (is.defined(options)) {
      if (!is.object(options)) throw invalid('options', 'object', options);
      if (is.defined(options.background)) checkColour(options.background);
      for (const key of ['idx', 'idy', 'odx', 'ody']) {
        if (is.defined(options[key]) && !is.number(options[key])) throw invalid(`options.${key}`, 'number', options[key]);
      }
      if (is.defined(options.interpolator) && !is.inArray(options.interpolator, ['nearest', 'bilinear', 'bicubic', 'lbb', 'nohalo', 'vsqbs'])) {
        throw invalid('options.interpolator', 'valid interpolator name', options.interpolator);
      }
    }
    options = options || {};
    return setOperation(this, {
      op: 'affine',
      matrix: matrix.flat ? matrix.flat() : matrix,
      idx: options.idx || 0,
      idy: options.idy || 0,
      odx: options.odx || 0,
      ody: options.ody || 0,
      background: colorValue(options.background),
      interpolator: options.interpolator,
    });
  }

  sharpen(options) {
    if (is.plainObject(options)) {
      if (!(is.number(options.sigma) && is.inRange(options.sigma, 0.000001, 10))) {
        throw invalid('options.sigma', 'number between 0.000001 and 10', options.sigma);
      }
      for (const key of ['m1', 'm2', 'x1', 'y2', 'y3']) {
        if (is.defined(options[key]) && !(is.number(options[key]) && is.inRange(options[key], 0, 1000000))) {
          throw invalid(`options.${key}`, 'number between 0 and 1000000', options[key]);
        }
      }
    } else {
      return setOperation(this, { op: 'sharpen', simple: true, sigma: 1 });
    }
    return setOperation(this, {
      op: 'sharpen',
      sigma: options.sigma === undefined ? 1 : options.sigma,
      m1: options.m1,
      m2: options.m2,
      x1: options.x1,
      y2: options.y2,
      y3: options.y3,
    });
  }

  median(size) {
    if (!is.defined(size)) size = 3;
    else checkIntRange('size', size, 1, 1000);
    return setOperation(this, { op: 'median', size });
  }

  blur(options) {
    let sigma;
    if (is.number(options)) {
      sigma = options;
    } else if (is.plainObject(options)) {
      if (!is.number(options.sigma)) throw invalid('options.sigma', 'number between 0.3 and 1000', undefined);
      sigma = options.sigma;
      if ('precision' in options && !['integer', 'float', 'approximate'].includes(options.precision)) {
        throw invalid('precision', 'one of: integer, float, approximate', options.precision);
      }
      if ('minAmplitude' in options) checkNumRange('minAmplitude', options.minAmplitude, 0.001, 1);
    }
    if (!is.defined(options) || (is.bool(options) && options)) {
      return setOperation(this, { op: 'blur', sigma: 1, mild: true });
    }
    if (is.bool(options)) return this; // blur(false): no blur
    if (!(is.number(sigma) && is.inRange(sigma, 0.3, 1000))) throw invalid('sigma', 'number between 0.3 and 1000', sigma);
    return setOperation(this, {
      op: 'blur',
      sigma,
      precision: is.plainObject(options) ? options.precision : undefined,
      minAmplitude: is.plainObject(options) ? options.minAmplitude : undefined,
    });
  }

  dilate(width) {
    if (!is.defined(width)) width = 1;
    else checkIntRange('width', width, 1, 65536);
    return setOperation(this, { op: 'dilate', width });
  }

  erode(width) {
    if (!is.defined(width)) width = 1;
    else checkIntRange('width', width, 1, 65536);
    return setOperation(this, { op: 'erode', width });
  }

  flatten(options) {
    nullOptions(options, 'background');
    if (is.bool(options) && !options) {
      return this; // flatten(false): no-op
    }
    if (is.object(options) && options !== null && is.defined(options.background)) checkColour(options.background);
    const background = is.object(options) && options !== null ? options.background : undefined;
    return setOperation(this, { op: 'flatten', background: colorValue(background) });
  }

  unflatten() {
    return setOperation(this, { op: 'unflatten' });
  }

  gamma(gamma, gammaOut) {
    if (!is.defined(gamma)) gamma = 2.2;
    else if (!(is.number(gamma) && is.inRange(gamma, 1, 3))) throw invalid('gamma', 'number between 1.0 and 3.0', gamma);
    if (is.defined(gammaOut) && !(is.number(gammaOut) && is.inRange(gammaOut, 1, 3))) {
      throw invalid('gammaOut', 'number between 1.0 and 3.0', gammaOut);
    }
    return setOperation(this, { op: 'gamma', gamma, gammaOut: is.defined(gammaOut) ? gammaOut : gamma });
  }

  negate(options) {
    // negate() is a boolean flag in sharp: calling it twice does not undo it.
    this._spec.operations = this._spec.operations.filter((op) => op.op !== 'negate');
    if (is.bool(options) && !options) return this;
    let alpha = true;
    if (is.plainObject(options) && 'alpha' in options) {
      if (!is.bool(options.alpha)) throw invalid('alpha', 'should be boolean value', options.alpha);
      alpha = options.alpha;
    }
    return setOperation(this, { op: 'negate', alpha });
  }

  normalise(options) {
    let lower = 1;
    let upper = 99;
    if (is.plainObject(options)) {
      if (is.defined(options.lower)) {
        if (!(is.number(options.lower) && is.inRange(options.lower, 0, 99))) throw invalid('lower', 'number between 0 and 99', options.lower);
        lower = options.lower;
      }
      if (is.defined(options.upper)) {
        if (!(is.number(options.upper) && is.inRange(options.upper, 1, 100))) throw invalid('upper', 'number between 1 and 100', options.upper);
        upper = options.upper;
      }
    }
    if (lower >= upper) throw invalid('range', 'lower to be less than upper', `${lower} >= ${upper}`);
    return setOperation(this, { op: 'normalise', lower, upper });
  }

  normalize(options) {
    return this.normalise(options);
  }

  clahe(options) {
    if (!is.plainObject(options)) throw invalid('options', 'plain object', options);
    checkIntRange('width', options.width, 1, 65536);
    checkIntRange('height', options.height, 1, 65536);
    if (is.defined(options.maxSlope)) checkIntRange('maxSlope', options.maxSlope, 0, 100);
    return setOperation(this, {
      op: 'clahe',
      width: options.width,
      height: options.height,
      maxSlope: options.maxSlope === undefined ? 3 : options.maxSlope,
    });
  }

  convolve(kernel) {
    if (
      !is.object(kernel) || kernel === null || !Array.isArray(kernel.kernel) ||
      !is.integer(kernel.width) || !is.integer(kernel.height) ||
      !is.inRange(kernel.width, 3, 1001) || !is.inRange(kernel.height, 3, 1001) ||
      kernel.height * kernel.width !== kernel.kernel.length ||
      !kernel.kernel.every(is.number)
    ) {
      throw new Error('Invalid convolution kernel');
    }
    // sharp defaults: scale = sum of the kernel (min 1), offset = 0 (integers only).
    let scale = is.integer(kernel.scale) ? kernel.scale : kernel.kernel.reduce((a, b) => a + b, 0);
    if (scale < 1) scale = 1;
    const offset = is.integer(kernel.offset) ? kernel.offset : 0;
    return setOperation(this, {
      op: 'convolve',
      width: kernel.width,
      height: kernel.height,
      kernel: Array.from(kernel.kernel),
      scale,
      offset,
    });
  }

  threshold(threshold, options) {
    nullOptions(options, 'greyscale');
    if (!is.defined(threshold)) threshold = 128;
    else if (is.bool(threshold)) threshold = threshold ? 128 : 0;
    else if (!(is.integer(threshold) && is.inRange(threshold, 0, 255))) {
      throw invalid('threshold', 'integer between 0 and 255', threshold);
    }
    const grayscale = !is.object(options) || options === null || options.greyscale === true || options.grayscale === true;
    return setOperation(this, { op: 'threshold', threshold, grayscale });
  }

  boolean(operand, operator, options = {}) {
    if (!(is.string(operator) && is.inArray(operator, ['and', 'or', 'eor']))) {
      throw invalid('operator', 'one of: and, or, eor', operator);
    }
    let bytes;
    if (typeof operand === 'string' && operand.length > 0) bytes = fs.readFileSync(operand);
    else bytes = toBufferLike(operand);
    if (!bytes) throw new Error(`Unsupported input '${operand}' of type ${typeof operand} for boolean operand`);
    if (bytes.length === 0) throw new Error('Input Buffer is empty');
    return setOperation(this, {
      op: 'boolean',
      operand: bytes.toString('base64'),
      operator,
      raw: options.raw,
    });
  }

  linear(a, b) {
    if (!is.defined(a) && is.number(b)) a = 1.0;
    else if (is.number(a) && !is.defined(b)) b = 0.0;
    const list = (name, v) => {
      if (!is.defined(v)) return [];
      if (is.number(v)) return [v];
      if (Array.isArray(v) && is.inRange(v.length, 1, 1000000) && v.every(is.number)) return v;
      throw invalid(name, 'number or array of numbers', v);
    };
    const la = list('a', a);
    const lb = list('b', b);
    if (la.length !== lb.length) throw new Error('Expected a and b to be arrays of the same length');
    return setOperation(this, { op: 'linear', a: is.defined(a) ? a : undefined, b: is.defined(b) ? b : undefined });
  }

  recomb(matrix) {
    if (!Array.isArray(matrix)) throw invalid('recomb inputMatrix', 'array', matrix);
    const dimensions = matrix.length;
    if (dimensions !== 3 && dimensions !== 4) throw invalid('recomb inputMatrix', '3x3 or 4x4 array', dimensions);
    if (!matrix.every((row) => Array.isArray(row) && row.length === dimensions)) {
      throw invalid('inputMatrix', `array of ${dimensions} arrays of length ${dimensions}`, matrix);
    }
    if (!matrix.flat().every(is.number)) throw invalid('inputMatrix', 'array of numbers', matrix.flat());
    return setOperation(this, { op: 'recomb', matrix });
  }

  modulate(options) {
    if (!is.plainObject(options)) throw invalid('options', 'plain object', options);
    if ('brightness' in options && !(is.number(options.brightness) && options.brightness >= 0)) {
      throw invalid('brightness', 'number above zero', options.brightness);
    }
    if ('saturation' in options && !(is.number(options.saturation) && options.saturation >= 0)) {
      throw invalid('saturation', 'number above zero', options.saturation);
    }
    if ('hue' in options && !is.integer(options.hue)) throw invalid('hue', 'number', options.hue);
    if ('lightness' in options && !is.number(options.lightness)) throw invalid('lightness', 'number', options.lightness);
    return setOperation(this, {
      op: 'modulate',
      brightness: options.brightness === undefined ? 1 : options.brightness,
      saturation: options.saturation === undefined ? 1 : options.saturation,
      hue: (options.hue || 0) % 360,
      lightness: options.lightness || 0,
    });
  }

  tint(color) {
    if (!is.defined(color)) return this;
    return setOperation(this, { op: 'tint', tint: colorValue(checkColour(color, 'tint')) });
  }

  greyscale(flag) {
    if (is.bool(flag) && !flag) return this; // sharp: greyscale(false) is a no-op
    return setOperation(this, { op: 'greyscale' });
  }

  grayscale(flag) {
    return this.greyscale(flag);
  }

  removeAlpha() {
    return setOperation(this, { op: 'removeAlpha' });
  }

  ensureAlpha(alpha) {
    if (!is.defined(alpha)) alpha = 1;
    else if (!(is.number(alpha) && is.inRange(alpha, 0, 1))) throw invalid('alpha', 'number between 0 and 1', alpha);
    return setOperation(this, { op: 'ensureAlpha', alpha });
  }

  extractChannel(channel) {
    const channelMap = { red: 0, green: 1, blue: 2, alpha: 3 };
    if (Object.prototype.hasOwnProperty.call(channelMap, channel)) channel = channelMap[channel];
    if (!(is.integer(channel) && is.inRange(channel, 0, 4))) {
      throw invalid('channel', 'integer or one of: red, green, blue, alpha', channel);
    }
    return setOperation(this, { op: 'extractChannel', channel });
  }

  joinChannel(input, options = {}) {
    const bytes = typeof input === 'string' ? fs.readFileSync(input) : toBufferLike(input);
    if (!bytes) throw new TypeError('joinChannel input must be a path or Buffer');
    return setOperation(this, {
      op: 'joinChannel',
      input: bytes.toString('base64'),
      raw: options.raw,
    });
  }

  bandbool(operator) {
    if (!(is.string(operator) && is.inArray(operator, ['and', 'or', 'eor']))) {
      throw invalid('boolOp', 'one of: and, or, eor', operator);
    }
    return setOperation(this, { op: 'bandbool', operator });
  }

  composite(images) {
    if (!Array.isArray(images)) throw invalid('images to composite', 'array', images);
    // Validate everything first so a failing entry leaves the pipeline untouched.
    const ops = images.map((entry) => {
      if (!(is.object(entry) && entry !== null)) throw invalid('image to composite', 'object', entry);
      const parsed = parseCompositeInput(entry);
      const value = entry;
      let blend = 'over';
      if (is.defined(value.blend)) {
        if (!(is.string(value.blend) && is.string(BLENDS[value.blend]))) throw invalid('blend', 'valid blend name', value.blend);
        blend = BLENDS[value.blend];
      }
      let tile = false;
      if (is.defined(value.tile)) tile = checkBool('tile', value.tile);
      for (const key of ['left', 'top']) {
        if (is.defined(value[key])) checkIntRange(key, value[key], -100000000, 100000000);
      }
      if (is.defined(value.top) !== is.defined(value.left)) throw new Error('Expected both left and top to be set');
      let gravity = 'centre';
      if (is.defined(value.gravity)) {
        if (is.integer(value.gravity) && is.inRange(value.gravity, 0, 8)) gravity = GRAVITY_NAMES[value.gravity];
        else if (is.string(value.gravity) && is.integer(GRAVITY[value.gravity])) gravity = value.gravity;
        else throw invalid('gravity', 'valid gravity', value.gravity);
      }
      let premultiplied = false;
      if (is.defined(value.premultiplied)) premultiplied = checkBool('premultiplied', value.premultiplied);
      return {
        op: 'composite',
        input: parsed.bytes.toString('base64'),
        ...(parsed.source ? { source: parsed.source } : {}),
        blend: blend.replace(/-(\w)/g, (_, c) => c.toUpperCase()),
        gravity,
        top: value.top,
        left: value.left,
        tile,
        density: value.density,
        premultiplied,
      };
    });
    // sharp semantics: the last composite() call replaces earlier ones.
    this._spec.operations = this._spec.operations.filter((op) => op.op !== 'composite');
    for (const op of ops) setOperation(this, op);
    return this;
  }

  replaceColor(from, to, options = {}) {
    return setOperation(this, {
      op: 'replaceColor',
      from: colorString(from),
      to: colorString(to),
      tolerance: options.tolerance === undefined ? 0.12 : options.tolerance,
      softness: options.softness === undefined ? 0.08 : options.softness,
      amount: options.amount === undefined ? 1 : options.amount,
      preserveOpacity: options.preserveOpacity !== false,
    });
  }

  recolor(color, options = {}) {
    return setOperation(this, {
      op: 'recolor',
      color: colorString(color),
      amount: options.amount === undefined ? 1 : options.amount,
      preserveOpacity: options.preserveOpacity !== false,
    });
  }

  duotone(shadows, highlights, options = {}) {
    if (shadows && typeof shadows === 'object' && !Array.isArray(shadows)) {
      options = highlights || {};
      highlights = shadows.highlights;
      shadows = shadows.shadows;
    }
    return setOperation(this, {
      op: 'duotone',
      shadows: colorString(shadows),
      highlights: colorString(highlights),
      amount: options.amount === undefined ? 1 : options.amount,
      preserveOpacity: options.preserveOpacity !== false,
    });
  }

  gradientMap(gradient, options = {}) {
    return setOperation(this, {
      op: 'gradientMap',
      gradient: gradientSpec(gradient),
      amount: options.amount === undefined ? 1 : options.amount,
      preserveOpacity: options.preserveOpacity !== false,
    });
  }

  gradientRecolor(gradient, options = {}) {
    return setOperation(this, {
      op: 'gradientRecolor',
      gradient: gradientSpec(gradient),
      amount: options.amount === undefined ? 1 : options.amount,
      preserveOpacity: options.preserveOpacity !== false,
    });
  }

  gradientFill(gradient, options = {}) {
    return setOperation(this, {
      op: 'gradientFill',
      gradient: gradientSpec(gradient),
      amount: options.amount === undefined ? 1 : options.amount,
      preserveOpacity: options.preserveOpacity !== false,
    });
  }

  applyLut(lut, options = {}) {
    let data;
    if (Buffer.isBuffer(lut)) {
      data = lut.toString('utf8');
    } else if (typeof lut === 'string') {
      data = fs.existsSync(lut) ? fs.readFileSync(lut, 'utf8') : lut;
    } else {
      throw new TypeError('applyLut expects a .cube path, Buffer, or LUT text');
    }
    return setOperation(this, {
      op: 'applyLut',
      data,
      strength: options.strength === undefined ? 1 : options.strength,
      interpolation: options.interpolation || 'trilinear',
    });
  }

  curves(value) {
    return setOperation(this, { op: 'curves', ...curveSpec(value) });
  }

  levels(options = {}) {
    return setOperation(this, {
      op: 'levels',
      inputBlack: options.inputBlack === undefined ? 0 : options.inputBlack,
      inputWhite: options.inputWhite === undefined ? 255 : options.inputWhite,
      gamma: options.gamma === undefined ? 1 : options.gamma,
      outputBlack: options.outputBlack === undefined ? 0 : options.outputBlack,
      outputWhite: options.outputWhite === undefined ? 255 : options.outputWhite,
    });
  }

  whiteBalance(options = {}) {
    return setOperation(this, {
      op: 'whiteBalance',
      temperature: options.temperature || 0,
      tint: options.tint || 0,
    });
  }

  colourBalance(options = {}) {
    return setOperation(this, {
      op: 'colourBalance',
      shadows: channelShift(options.shadows),
      midtones: channelShift(options.midtones),
      highlights: channelShift(options.highlights),
    });
  }

  colorBalance(options = {}) {
    return this.colourBalance(options);
  }

  posterize(levels = 8, options = {}) {
    if (levels && typeof levels === 'object') {
      options = levels;
      levels = options.levels === undefined ? 8 : options.levels;
    }
    return setOperation(this, { op: 'posterize', levels });
  }

  solarize(options = {}) {
    const threshold = typeof options === 'number' ? options : options.threshold;
    return setOperation(this, {
      op: 'solarize',
      threshold: threshold === undefined ? 128 : threshold,
    });
  }

  fill(colorOrGradient, options = {}) {
    const isGradient =
      colorOrGradient instanceof Gradient ||
      Array.isArray(colorOrGradient) ||
      (colorOrGradient && typeof colorOrGradient === 'object' && Array.isArray(colorOrGradient.stops));
    if (isGradient) {
      return this.gradientFill(colorOrGradient, options);
    }
    return setOperation(this, {
      op: 'fill',
      color: colorString(colorOrGradient),
      amount: options.amount === undefined ? 1 : options.amount,
      preserveOpacity: options.preserveOpacity !== false,
    });
  }

  paletteSwap(palette, options = {}) {
    const colors = Array.isArray(palette) ? palette : palette && palette.palette;
    if (!Array.isArray(colors)) throw new TypeError('paletteSwap requires an array of colors');
    return setOperation(this, {
      op: 'paletteSwap',
      palette: colors.map(colorString),
      dither: options.dither === undefined ? 0 : options.dither,
      preserveOpacity: options.preserveOpacity !== false,
    });
  }

  alphaFromLuma(options = {}) {
    return setOperation(this, {
      op: 'alphaFromLuma',
      channel: options.channel || 'luma',
      invert: options.invert === true,
      multiply: options.multiply === true,
      scale: options.scale === undefined ? 1 : options.scale,
      offset: options.offset === undefined ? 0 : options.offset,
    });
  }

  toFormat(format, options) {
    const id = is.object(format) && format !== null && is.string(format.id) ? format.id : format;
    const name = typeof id === 'string' ? id.toLowerCase() : undefined;
    // Besides sharp's names, the engine also accepts mime types and qoi/apng.
    const known = name !== undefined && (OUTPUT_FORMATS.has(name) || FORMAT_ALIASES[name] !== undefined || name === 'qoi');
    if (!known) {
      throw invalid('format', `one of: ${[...OUTPUT_FORMATS].join(', ')}`, format);
    }
    const actual = normalizeFormat(name);
    if (actual === 'heic' || actual === 'heif') return this.heif(options);
    if (actual === 'tif' || actual === 'tiff') return this.tiff(options);
    if (actual === 'jpx' || actual === 'j2k' || actual === 'j2c' || actual === 'jp2') return this.jp2(options);
    if (actual === 'dz' || actual === 'tile') return this.tile(options);
    if (actual === 'raw') return this.raw(options);
    return this._setFormat(actual, options);
  }

  _setFormat(format, options) {
    validateOutputOptions(format, options);
    const opts = is.object(options) && options !== null ? options : {};
    // force:false keeps the input format (the options only apply if it matches).
    if (opts.force !== false) this._spec.output.format = format;
    Object.assign(this._spec.output, mapOutputOptions(opts, format));
    return this;
  }

  jpeg(options) {
    return this._setFormat('jpeg', options);
  }

  png(options) {
    return this._setFormat('png', options);
  }

  webp(options) {
    return this._setFormat('webp', options);
  }

  gif(options) {
    return this._setFormat('gif', options);
  }

  avif(options) {
    if (options === null) options = {};
    validateOutputOptions('avif', options);
    return this._setFormat('avif', options);
  }

  jxl(options) {
    return this._setFormat('jxl', options);
  }

  qoi(options) {
    return this._setFormat('qoi', options);
  }

  toColourspace(space) {
    if (!is.string(space)) throw invalid('colourspace', 'string', space);
    this._spec.output.colourspace = String(space).toLowerCase();
    return this;
  }

  toColorspace(space) {
    return this.toColourspace(space);
  }

  pipelineColourspace(space) {
    if (!is.string(space)) throw invalid('colourspace', 'string', space);
    this._spec.pipelineColourspace = String(space).toLowerCase();
    return this;
  }

  pipelineColorspace(space) {
    return this.pipelineColourspace(space);
  }

  raw(options) {
    validateOutputOptions('raw', options);
    this._spec.output.format = 'raw';
    Object.assign(this._spec.output, mapOutputOptions(is.object(options) && options !== null ? options : {}));
    return this;
  }

  tile(options = {}) {
    nullOptions(options, 'size');
    if (is.object(options) && options !== null) {
      if (is.defined(options.size)) checkIntRange('size', options.size, 1, 8192);
      if (is.defined(options.overlap)) {
        checkIntRange('overlap', options.overlap, 0, 8192);
        if (options.overlap > (is.defined(options.size) ? options.size : 256)) {
          throw invalid('overlap', `<= size (${is.defined(options.size) ? options.size : 256})`, options.overlap);
        }
      }
      if (is.defined(options.container) && !(is.string(options.container) && is.inArray(options.container, ['fs', 'zip']))) {
        throw invalid('container', 'one of: fs, zip', options.container);
      }
      if (is.defined(options.layout) && !(is.string(options.layout) && is.inArray(options.layout, ['dz', 'google', 'iiif', 'iiif3', 'zoomify']))) {
        throw invalid('layout', 'one of: dz, google, iiif, iiif3, zoomify', options.layout);
      }
      if (is.defined(options.angle) && !(is.integer(options.angle) && !(options.angle % 90))) {
        throw invalid('angle', 'positive/negative multiple of 90', options.angle);
      }
      if (is.defined(options.background)) checkColour(options.background);
      if (is.defined(options.depth) && !(is.string(options.depth) && is.inArray(options.depth, ['onepixel', 'onetile', 'one']))) {
        throw invalid('depth', 'one of: onepixel, onetile, one', options.depth);
      }
      if (is.defined(options.skipBlanks)) checkIntRange('skipBlanks', options.skipBlanks, -1, 65535);
      if (is.defined(options.id) && !is.string(options.id)) throw invalid('id', 'string', options.id);
      if (is.defined(options.basename) && !is.string(options.basename)) throw invalid('basename', 'string', options.basename);
    }
    const fmt = this._spec.output.format;
    if (fmt && !['jpeg', 'png', 'webp'].includes(fmt)) throw invalid('format', 'one of: jpeg, png, webp', fmt);
    this._tile = {
      size: 256,
      overlap: 0,
      background: '#ffffff',
      container: 'fs',
      layout: 'dz',
      ...options,
    };
    return this;
  }

  heif(options) {
    // sharp's heif() needs a compression; AV1 is AVIF, which the engine can encode.
    if (!(is.object(options) && options !== null)) {
      throw new Error('HEIF/HEIC output needs options: use avif() or heif({ compression: "av1" }) (HEVC is not supported)');
    }
    if (!(is.string(options.compression) && is.inArray(options.compression, ['av1', 'hevc']))) {
      throw invalid('compression', 'one of: av1, hevc', options.compression);
    }
    if (options.compression === 'hevc') {
      throw new Error('HEVC (HEIC) output is not supported; use avif() or heif({ compression: "av1" })');
    }
    return this.avif({ ...options, compression: undefined });
  }

  tiff() {
    throw new Error('TIFF is not supported by fastpkg');
  }

  jp2() {
    throw new Error('JPEG 2000 is not supported by fastpkg');
  }

  keepMetadata() {
    this._spec.output.keepMetadata = true;
    return this;
  }

  withMetadata(options = {}) {
    nullOptions(options, 'orientation');
    if (is.object(options) && options !== null) {
      if (is.defined(options.orientation)) checkIntRange('orientation', options.orientation, 1, 8);
      if (is.defined(options.density) && !(is.number(options.density) && options.density > 0)) {
        throw invalid('density', 'positive number', options.density);
      }
    }
    // sharp: withMetadata() keeps all metadata (and optionally overrides orientation/density/...).
    this._spec.output.keepMetadata = true;
    // ... and, like sharp, converts to sRGB and embeds that profile unless `icc` says otherwise.
    this.withIccProfile('srgb');
    const meta = { ...options };
    if (is.number(meta.density)) meta.density = Math.round(meta.density);
    // A named or file profile is resolved here (built-in 'srgb' / 'p3' are generated).
    if (is.defined(meta.icc)) {
      this.withIccProfile(meta.icc);
      delete meta.icc;
    }
    this._spec.output.withMetadata = meta;
    return this;
  }

  keepExif() {
    this._spec.output.keepExif = true;
    return this;
  }

  withExif(exif) {
    if (!Buffer.isBuffer(exif) && !(typeof exif === 'string' && fs.existsSync(exif))) {
      if (!(is.object(exif) && exif !== null)) throw invalid('exif', 'object', exif);
      for (const [ifd, entries] of Object.entries(exif)) {
        if (!(is.object(entries) && entries !== null)) throw invalid(ifd, 'object', entries);
        for (const [k, v] of Object.entries(entries)) {
          if (!is.string(v)) throw invalid(`${ifd}.${k}`, 'string', v);
        }
      }
    }
    if (Buffer.isBuffer(exif)) {
      this._spec.output.withExifRaw = exif.toString('base64');
    } else if (typeof exif === 'string' && fs.existsSync(exif)) {
      this._spec.output.withExifRaw = fs.readFileSync(exif).toString('base64');
    } else {
      this._spec.output.withExif = exif;
    }
    return this;
  }

  withExifMerge(exif) {
    if (!(is.object(exif) && exif !== null)) throw invalid('exif', 'object', exif);
    for (const [ifd, entries] of Object.entries(exif)) {
      if (!(is.object(entries) && entries !== null)) throw invalid(ifd, 'object', entries);
      for (const [k, v] of Object.entries(entries)) {
        if (!is.string(v)) throw invalid(`${ifd}.${k}`, 'string', v);
      }
    }
    this._spec.output.withExifMerge = exif;
    return this;
  }

  keepIccProfile() {
    this._spec.output.keepIcc = true;
    return this;
  }

  withIccProfile(profile, options) {
    if (!Buffer.isBuffer(profile) && !is.string(profile)) throw invalid('icc', 'string', profile);
    if (is.object(options) && options !== null && is.defined(options.attach)) checkBool('attach', options.attach);
    const builtin = typeof profile === 'string' ? builtinIccProfile(profile.toLowerCase()) : undefined;
    if (Buffer.isBuffer(profile)) {
      this._spec.output.withIccRaw = profile.toString('base64');
    } else if (typeof profile === 'string' && fs.existsSync(profile)) {
      this._spec.output.withIccRaw = fs.readFileSync(profile).toString('base64');
    } else if (builtin) {
      this._spec.output.withIccRaw = builtin.toString('base64');
    } else {
      this._spec.output.withIcc = profile;
    }
    return this;
  }

  keepXmp() {
    this._spec.output.keepXmp = true;
    return this;
  }

  withXmp(xmp) {
    if (!(is.string(xmp))) throw invalid('xmp', 'non-empty string', xmp);
    this._spec.output.withXmp = xmp;
    return this;
  }

  keepGainMap() {
    this._spec.output.keepGainmap = true;
    return this;
  }

  withGainMap(gainmap) {
    this._spec.output.keepGainmap = gainmap !== undefined;
    return this;
  }

  withDensity(dpi) {
    if (!(is.number(dpi) && dpi > 0)) throw invalid('density', 'positive number', dpi);
    this._spec.output.withDensity = Math.round(dpi);
    return this;
  }

  timeout(options) {
    // sharp takes { seconds } (integer); the engine also accepts a bare number and fractional seconds.
    const seconds = is.number(options) ? options : is.plainObject(options) ? options.seconds : undefined;
    if (!(is.number(seconds) && is.inRange(seconds, 0, 3600))) {
      throw invalid('seconds', 'number between 0 and 3600', seconds);
    }
    if (seconds > 0) this._spec.output.timeout = seconds;
    else delete this._spec.output.timeout; // 0 = no timeout
    return this;
  }

  async metadata() {
    const spec = this._snap();
    let info;
    try {
      info = await native.metadataSync(await this._resolvedInput(), spec);
    } catch (error) {
      throw this._fail(error);
    }
    for (const key of ['exif', 'icc', 'xmp']) {
      if (typeof info[key] === 'string') info[key] = Buffer.from(info[key], 'base64');
    }
    // sharp omits fields it does not know instead of reporting null.
    for (const key of Object.keys(info)) if (info[key] === null) delete info[key];
    if (info.loopCount !== undefined) {
      info.loop = info.loopCount;
      delete info.loopCount;
    }
    const allPages = spec.input.animated === true;
    if (!allPages && info.pageHeight !== undefined) delete info.pageHeight;
    if (info.pages === 1 && ['jpeg', 'png', 'webp'].includes(info.format)) delete info.pages;
    return info;
  }

  async stats() {
    const spec = this._snap();
    try {
      return await native.stats(await this._resolvedInput(), spec);
    } catch (error) {
      throw this._fail(error);
    }
  }

  async probe() {
    const spec = this._snap();
    try {
      return await native.probeSync(await this._resolvedInput(), spec);
    } catch (error) {
      throw this._fail(error);
    }
  }

  async dominant() {
    const stats = await this.stats();
    return stats.dominant;
  }

  async toBuffer(options = {}, callback) {
    if (typeof options === 'function') {
      callback = options;
      options = {};
    }
    if (this._tile) {
      const result = await this._tilesBuffer(this._tile).catch((error) => {
        throw this._fail(error);
      });
      const output = options.resolveWithObject ? result : result.data;
      if (callback) {
        process.nextTick(() => callback(null, result.data, result.info));
        return undefined;
      }
      return output;
    }
    const spec = this._snap();
    queueDepth += 1;
    queue.emit('change', queueDepth);
    let result;
    try {
      result = await native.process(await this._resolvedInput(), spec);
    } catch (error) {
      this._fail(error);
      if (callback) {
        process.nextTick(() => callback(error));
        return undefined;
      }
      throw error;
    } finally {
      queueDepth = Math.max(0, queueDepth - 1);
      queue.emit('change', queueDepth);
    }
    const output = options.resolveWithObject
      ? { data: result.data, info: result.info }
      : result.data;
    if (callback) {
      process.nextTick(() => callback(null, result.data, result.info));
      return undefined;
    }
    return output;
  }

  async toUint8Array(options = {}, callback) {
    if (typeof options === 'function') {
      callback = options;
      options = {};
    }
    const result = await this.toBuffer({ ...options, resolveWithObject: true });
    const data = new Uint8Array(result.data.buffer, result.data.byteOffset, result.data.byteLength);
    if (callback) {
      process.nextTick(() => callback(null, data, result.info));
      return undefined;
    }
    // sharp resolves { data, info } here (not the bare array).
    return { data, info: result.info };
  }

  async toFile(file, options = {}, callback) {
    if (typeof options === 'function') {
      callback = options;
      options = {};
    }
    if (this._tile) {
      const info = await this._writeTiles(file, this._tile).catch((error) => {
        throw this._fail(error);
      });
      if (callback) {
        process.nextTick(() => callback(null, info));
        return undefined;
      }
      return info;
    }
    if (typeof file !== 'string' || !file) throw new TypeError('Missing output file path');
    const spec = this._snap();
    if (!spec.output.format) {
      // sharp keeps the input format for extensions it does not recognise (".txt", ".bmp", ...).
      const ext = normalizeFormat(path.extname(file).slice(1));
      if (ext && OUTPUT_EXTENSIONS.has(ext)) spec.output.format = ext;
    }
    if (typeof this._input === 'string' && sameFile(this._input, file)) {
      const error = new Error('Cannot use same file for input and output');
      if (callback) {
        process.nextTick(() => callback(error));
        return undefined;
      }
      throw error;
    }
    try {
      const info = await native.toFile(await this._resolvedInput(), file, spec);
      if (callback) {
        process.nextTick(() => callback(null, info));
        return undefined;
      }
      return info;
    } catch (error) {
      this._fail(error);
      if (callback) {
        process.nextTick(() => callback(error));
        return undefined;
      }
      throw error;
    }
  }

  async _buildTiles(tile, root) {
    const result = await native.process(await this._resolvedInput(), this._snap());
    const full = result.data;
    const info = result.info;
    const layout = String(tile.layout || 'dz').toLowerCase();
    if (!['dz', 'google', 'zoomify', 'iiif', 'iiif3'].includes(layout)) {
      throw new Error(`unsupported tile layout '${layout}'`);
    }
    const format = normalizeFormat(this._spec.output.format || info.format || 'jpeg');
    const ext = format === 'jpeg' ? 'jpg' : format;
    const size = Math.max(1, Math.min(8192, Number(tile.size) || 256));
    const overlap = Math.max(0, Math.min(8192, Number(tile.overlap) || 0));
    const background = colorValue(tile.background, '#ffffff');
    const maxLevel = Math.max(0, Math.ceil(Math.log2(Math.max(info.width, info.height))));
    const scaleAt = (level) => 2 ** (maxLevel - level);
    const baseSpec = { input: {}, operations: [], output: { ...this._spec.output, format } };
    let startLevel = 0;
    if (tile.depth === 'one') startLevel = maxLevel;
    if (tile.depth === 'onetile') {
      const tilesAcross = Math.max(1, Math.ceil(Math.max(info.width, info.height) / size));
      startLevel = Math.max(0, maxLevel - Math.ceil(Math.log2(tilesAcross)));
    }
    const files = new Map();
    for (let level = startLevel; level <= maxLevel; level++) {
      const scale = scaleAt(level);
      const width = Math.max(1, Math.round(info.width / scale));
      const height = Math.max(1, Math.round(info.height / scale));
      const levelResult = await native.process(full, {
        ...baseSpec,
        operations: [{ op: 'resize', width, height, fit: 'fill' }],
      });
      const columns = Math.ceil(width / size);
      const rows = Math.ceil(height / size);
      for (let row = 0; row < rows; row++) {
        for (let col = 0; col < columns; col++) {
          const left = Math.max(0, col * size - overlap);
          const top = Math.max(0, row * size - overlap);
          const tileWidth = Math.min(size + overlap + (col * size - left), width - left);
          const tileHeight = Math.min(size + overlap + (row * size - top), height - top);
          const operations = [
            {
              op: 'extract',
              left,
              top,
              width: Math.max(1, tileWidth),
              height: Math.max(1, tileHeight),
            },
          ];
          if (tileWidth < size || tileHeight < size) {
            operations.push({
              op: 'extend',
              width: size,
              height: size,
              position: 'top-left',
              background,
            });
          }
          const tileResult = await native.process(levelResult.data, {
            ...baseSpec,
            operations,
          });
          const rel = tilePath(layout, level, col, row, width, height, size, ext, row * columns + col);
          const name = layout === 'dz'
            ? path.join(`${root}_files`, rel)
            : path.join(root, rel);
          files.set(name, tileResult.data);
        }
      }
    }
    if (layout === 'dz') {
      const xml = `<?xml version="1.0" encoding="UTF-8"?>\n<Image TileSize="${size}" Overlap="${overlap}" Format="${format}" xmlns="http://schemas.microsoft.com/deepzoom/2008"><Size Width="${info.width}" Height="${info.height}"/></Image>\n`;
      files.set(`${root}.dzi`, Buffer.from(xml));
    } else if (layout === 'zoomify') {
      const xml = `<IMAGE_PROPERTIES WIDTH="${info.width}" HEIGHT="${info.height}" NUMTILES="${files.size}" NUMIMAGES="1" VERSION="1.8" TILESIZE="${size}" />`;
      files.set(path.join(root, 'ImageProperties.xml'), Buffer.from(xml));
    } else if (layout === 'iiif' || layout === 'iiif3') {
      const version = layout === 'iiif3' ? 3 : 2;
      const id = tile.id || 'https://example.com/iiif';
      const infoJson = version === 3
        ? {
            '@context': 'http://iiif.io/api/image/3/context.json',
            id,
            type: 'ImageService3',
            protocol: 'http://iiif.io/api/image',
            profile: 'level0',
            width: info.width,
            height: info.height,
            tiles: [{ width: size, height: size, scaleFactors: Array.from({ length: maxLevel - startLevel + 1 }, (_, i) => 2 ** i) }],
          }
        : {
            '@context': 'http://iiif.io/api/image/2/context.json',
            '@id': id,
            protocol: 'http://iiif.io/api/image',
            profile: ['http://iiif.io/api/image/2/level0.json'],
            width: info.width,
            height: info.height,
            tiles: [{ width: size, height: size, scaleFactors: Array.from({ length: maxLevel - startLevel + 1 }, (_, i) => 2 ** i) }],
          };
      files.set(path.join(root, 'info.json'), Buffer.from(JSON.stringify(infoJson, null, 2)));
    } else if (layout === 'google') {
      const properties = `<vips-properties><image><properties><width>${info.width}</width><height>${info.height}</height><tile_size>${size}</tile_size><overlap>${overlap}</overlap><format>${format}</format></properties></image></vips-properties>`;
      files.set(path.join(root, 'vips-properties.xml'), Buffer.from(properties));
    }
    return {
      files,
      info: {
        ...info,
        tile: { layout, size, overlap, levels: maxLevel - startLevel + 1 },
      },
    };
  }

  async _writeTiles(file, tile) {
    const zip = tile.container === 'zip' || /\.(zip|szi)$/i.test(file);
    const base = file.replace(/\.(dz|zip|szi)$/i, '');
    const root = zip ? (tile.basename || path.basename(base) || 'tiles') : base;
    const { files, info } = await this._buildTiles(tile, root);
    if (zip) {
      fs.mkdirSync(path.dirname(file) || '.', { recursive: true });
      fs.writeFileSync(file, zipBuffer(files));
      return info;
    }
    for (const [name, data] of files) {
      fs.mkdirSync(path.dirname(name), { recursive: true });
      fs.writeFileSync(name, data);
    }
    return info;
  }

  async _tilesBuffer(tile) {
    const root = tile.basename || 'tiles';
    const { files, info } = await this._buildTiles(tile, root);
    return { data: zipBuffer(files), info };
  }


  toStream() {
    const self = this;
    return Readable.from(
      (async function* () {
        yield await self.toBuffer();
      })()
    );
  }

  async responsive(widths, options = {}) {
    const formats = options.formats || [this._spec.output.format || 'webp'];
    const out = [];
    for (const width of widths) {
      for (const format of formats) {
        const clone = this.clone().resize({ width }).toFormat(format, options);
        const data = await clone.toBuffer();
        out.push({ width, format: normalizeFormat(format), data });
      }
    }
    return out;
  }
}

function image(input, options) {
  // sharp(undefined) is an error while sharp() creates a write-fed stream instance.
  if (arguments.length === 1 && !is.defined(input)) throw new Error('Invalid input');
  return new Sharp(input, options === undefined ? {} : options);
}

image.Sharp = Sharp;
image.default = image;
image.sharp = image;

image.cache = function cache() {
  return { memory: 0, files: 0, items: 0 };
};

image.concurrency = function concurrency(value) {
  if (value === undefined) return native.getConcurrency();
  // sharp: 0 (or any non-positive value) resets to the default, the number of available cores.
  return native.setConcurrency(Number(value) > 0 ? value : undefined);
};

image.counters = function counters() {
  return { queue: queueDepth, process: 0 };
};

image.simd = function simd(value) {
  if (value === undefined) return native.simdEnabled();
  return native.setSimd(value);
};

image.format = native.formats();
image.versions = {
  ...native.versions(),
  jpeg: 'MozJPEG + jpeg-decoder fallback',
  vips: 'fastpkg 0.1.0',
};
image.interpolators = {
  nearest: 'nearest',
  bilinear: 'bilinear',
  bicubic: 'bicubic',
  locallyBoundedBicubic: 'lbb',
  nohalo: 'nohalo',
  vertexSplitQuadraticBasisSpline: 'vsqbs',
  // engine extras (resampling kernels usable as interpolators)
  mitchell: 'mitchell',
  lanczos2: 'lanczos2',
  lanczos3: 'lanczos3',
  vsqbs: 'vsqbs',
};
// sharp's constant tables (sharp.gravity.north, sharp.fit.cover, sharp.blend.multiply, ...).
image.gravity = GRAVITY;
image.strategy = STRATEGY;
image.position = POSITION;
image.kernel = Object.fromEntries(KERNELS.map((k) => [k, k]));
image.fit = Object.fromEntries(FITS.map((k) => [k, k]));
image.blend = BLENDS;
image.bool = { and: 'and', or: 'or', eor: 'eor' };
image.colourspace = { multiband: 'multiband', 'b-w': 'b-w', bw: 'b-w', cmyk: 'cmyk', srgb: 'srgb' };
image.colorspace = image.colourspace;
image.align = ALIGN;
image.queue = queue;
image.block = function block() {};
image.unblock = function unblock() {};

image.Gradient = Gradient;
image.gradient = function gradient(type = 'linear') {
  return new Gradient(type);
};
image.linearGradient = function linearGradient(angle = 0) {
  return new Gradient().linear(angle);
};
image.radialGradient = function radialGradient(cx = 0.5, cy = 0.5, radius = 0.5) {
  return new Gradient().radial(cx, cy, radius);
};

module.exports = image;
