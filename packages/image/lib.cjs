'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { Duplex, Readable, PassThrough } = require('node:stream');
const { EventEmitter } = require('node:events');
const zlib = require('node:zlib');

function loadNative() {
  const dir = __dirname;
  const candidates = fs
    .readdirSync(dir)
    .filter((name) => name.startsWith('image') && name.endsWith('.node'));
  const ordered = candidates.sort((a, b) => {
    const platform = `image.${process.platform}-${process.arch}`;
    return Number(b.startsWith(platform)) - Number(a.startsWith(platform));
  });
  for (const name of ordered) {
    try {
      return require(path.join(dir, name));
    } catch (error) {
      if (process.env.FASTPKG_DEBUG) console.error(`[@fastpkg/image] failed to load ${name}`, error);
    }
  }
  throw new Error(
    '@fastpkg/image native addon not found. Run `npm run build:native` from the repository root.'
  );
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
  if (typeof input === 'string') return input;
  const buffer = toBufferLike(input);
  if (buffer) return buffer;
  throw new TypeError(
    'unsupported input: expected a path, Buffer, Uint8Array, ArrayBuffer, or stream'
  );
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
    if (options[from] !== undefined) out[to] = options[from];
  }
  if (options.raw) out.raw = { ...options.raw };
  if (options.create) out.create = { ...options.create };
  if (options.text) out.text = { ...options.text };
  return out;
}

function setOperation(instance, op) {
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
    if (options[key] !== undefined) out[key] = options[key];
  }
  if (typeof out.position === 'object' && out.position !== null) {
    out.position = positionName(out.position);
  }
  return out;
}

function positionName(position) {
  if (!position || typeof position !== 'object') return position;
  if (position.left !== undefined && position.top !== undefined) return 'top-left';
  if (position.left !== undefined) return 'left';
  if (position.top !== undefined) return 'top';
  return 'centre';
}

function mapOutputOptions(options = {}) {
  const out = {};
  const keys = [
    'quality',
    'lossless',
    'effort',
    'progressive',
    'chromaSubsampling',
    'compressionLevel',
    'palette',
    'colors',
    'dither',
    'delay',
    'pageHeight',
    'depth',
    'mozjpeg',
    'trellisQuantisation',
    'overshootDeringing',
    'optimiseScans',
    'optimiseCoding',
    'smartSubsample',
    'nearLossless',
    'preset',
    'bitdepth',
    'timeout',
  ];
  for (const key of keys) {
    if (options[key] !== undefined) out[key] = options[key];
  }
  if (options.loop !== undefined) out.loopCount = options.loop;
  if (options.delay !== undefined) out.delay = Array.isArray(options.delay) ? options.delay : [options.delay];
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

function parseCompositeInput(entry) {
  const value = typeof entry === 'object' && entry !== null ? entry : { input: entry };
  const input = value.input;
  let bytes;
  if (typeof input === 'string') {
    bytes = fs.readFileSync(input);
  } else {
    bytes = toBufferLike(input);
  }
  if (!bytes) throw new TypeError('composite input must be a path, Buffer, or typed array');
  return { value, bytes };
}

class Sharp extends Duplex {
  constructor(input, options = {}) {
    super({ readableObjectMode: false, writableObjectMode: false });
    this._inputFromStream = false;
    this._chunks = null;
    this._inputPromise = null;

    if (input && typeof input === 'object' && !Buffer.isBuffer(input) && !(input instanceof Uint8Array) && !(input instanceof ArrayBuffer) && !ArrayBuffer.isView(input) && !isStream(input)) {
      const source = { ...input };
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
    queueDepth += 1;
    queue.emit('change', queueDepth);
    native
      .process(input, this._spec)
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
        callback(error);
      });
  }

  async _resolvedInput() {
    if (this._input !== null) return this._input;
    if (this._inputPromise) return this._inputPromise;
    if (this._chunks && this._chunks.length) return Buffer.concat(this._chunks);
    return EMPTY;
  }

  clone() {
    const copy = Object.create(Sharp.prototype);
    Object.assign(copy, this);
    copy._spec = JSON.parse(JSON.stringify(this._spec));
    copy._chunks = null;
    copy._inputFromStream = false;
    copy._inputPromise = null;
    return copy;
  }

  resize(width, height, options = {}) {
    if (width && typeof width === 'object') {
      options = width;
      width = options.width;
      height = options.height;
    }
    return setOperation(this, {
      op: 'resize',
      width: width === null ? undefined : width,
      height: height === null ? undefined : height,
      ...mapResizeOptions(options),
    });
  }

  extend(options = {}) {
    return setOperation(this, {
      op: 'extend',
      width: options.width,
      height: options.height,
      background: options.background || '#000000',
      position: positionName(options.position) || 'centre',
      extendWith: options.extendWith || 'background',
    });
  }

  extract(options = {}) {
    return setOperation(this, {
      op: 'extract',
      left: options.left || 0,
      top: options.top || 0,
      width: options.width,
      height: options.height,
    });
  }

  trim(options = {}) {
    return setOperation(this, {
      op: 'trim',
      background: options.background || '#000000',
      threshold: options.threshold || 0,
      lineArt: options.lineArt || false,
    });
  }

  rotate(angle, options = {}) {
    if (angle === undefined) return this.autoOrient();
    return setOperation(this, {
      op: 'rotate',
      angle: Number(angle) || 0,
      background: options.background || '#000000',
    });
  }

  autoOrient() {
    return setOperation(this, { op: 'autoOrient' });
  }

  flip(flag = true) {
    return setOperation(this, { op: 'flip', flip: flag !== false });
  }

  flop(flag = true) {
    return setOperation(this, { op: 'flop', flop: flag !== false });
  }

  affine(matrix, options = {}) {
    return setOperation(this, {
      op: 'affine',
      matrix: matrix.flat ? matrix.flat() : matrix,
      idx: options.idx || 0,
      idy: options.idy || 0,
      odx: options.odx || 0,
      ody: options.ody || 0,
      background: options.background || '#000000',
      interpolator: options.interpolator,
    });
  }

  sharpen(options) {
    if (options === undefined) {
      return setOperation(this, { op: 'sharpen', simple: true, sigma: 1 });
    }
    if (typeof options === 'number') {
      return setOperation(this, { op: 'sharpen', sigma: options });
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

  median(size = 3) {
    return setOperation(this, { op: 'median', size });
  }

  blur(sigma) {
    const value = typeof sigma === 'object' && sigma !== null ? sigma.sigma : sigma;
    return setOperation(this, { op: 'blur', sigma: value === undefined ? 1 : Number(value) });
  }

  dilate(width = 1) {
    return setOperation(this, { op: 'dilate', width });
  }

  erode(width = 1) {
    return setOperation(this, { op: 'erode', width });
  }

  flatten(options = {}) {
    return setOperation(this, { op: 'flatten', background: options.background || '#000000' });
  }

  unflatten() {
    return setOperation(this, { op: 'unflatten' });
  }

  gamma(gamma = 2.2, gammaOut) {
    return setOperation(this, { op: 'gamma', gamma, gammaOut });
  }

  negate(options = {}) {
    return setOperation(this, { op: 'negate', alpha: options.alpha !== false });
  }

  normalise(options = {}) {
    return setOperation(this, {
      op: 'normalise',
      lower: options.lower === undefined ? 1 : options.lower,
      upper: options.upper === undefined ? 99 : options.upper,
    });
  }

  normalize(options = {}) {
    return this.normalise(options);
  }

  clahe(options = {}) {
    return setOperation(this, {
      op: 'clahe',
      width: options.width || 3,
      height: options.height || 3,
      maxSlope: options.maxSlope === undefined ? 3 : options.maxSlope,
    });
  }

  convolve(kernel) {
    return setOperation(this, {
      op: 'convolve',
      width: kernel.width,
      height: kernel.height,
      kernel: Array.from(kernel.kernel),
      scale: kernel.scale,
      offset: kernel.offset || 0,
    });
  }

  threshold(threshold = 128, options = {}) {
    if (typeof threshold === 'object') {
      options = threshold;
      threshold = options.threshold === undefined ? 128 : options.threshold;
    }
    return setOperation(this, {
      op: 'threshold',
      threshold,
      grayscale: options.greyscale !== false && options.grayscale !== false,
    });
  }

  boolean(operand, operator, options = {}) {
    const bytes = typeof operand === 'string' ? fs.readFileSync(operand) : toBufferLike(operand);
    if (!bytes) throw new TypeError('boolean operand must be a path or Buffer');
    return setOperation(this, {
      op: 'boolean',
      operand: bytes.toString('base64'),
      operator,
      raw: options.raw,
    });
  }

  linear(a, b) {
    return setOperation(this, { op: 'linear', a, b });
  }

  recomb(matrix) {
    return setOperation(this, { op: 'recomb', matrix });
  }

  modulate(options = {}) {
    return setOperation(this, {
      op: 'modulate',
      brightness: options.brightness === undefined ? 1 : options.brightness,
      saturation: options.saturation === undefined ? 1 : options.saturation,
      hue: options.hue || 0,
      lightness: options.lightness || 0,
    });
  }

  tint(color) {
    return setOperation(this, { op: 'tint', tint: String(color) });
  }

  greyscale() {
    return setOperation(this, { op: 'greyscale' });
  }

  grayscale() {
    return this.greyscale();
  }

  removeAlpha() {
    return setOperation(this, { op: 'removeAlpha' });
  }

  ensureAlpha(alpha = 1) {
    return setOperation(this, { op: 'ensureAlpha', alpha });
  }

  extractChannel(channel) {
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
    return setOperation(this, { op: 'bandbool', operator });
  }

  composite(images) {
    const list = Array.isArray(images) ? images : [images];
    for (const entry of list) {
      const { value, bytes } = parseCompositeInput(entry);
      setOperation(this, {
        op: 'composite',
        input: bytes.toString('base64'),
        blend: value.blend || 'over',
        gravity: value.gravity || value.position || 'centre',
        top: value.top,
        left: value.left,
        tile: value.tile || false,
        density: value.density,
        premultiplied: value.premultiplied || false,
      });
    }
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

  toFormat(format, options = {}) {
    this._spec.output.format = normalizeFormat(format);
    Object.assign(this._spec.output, mapOutputOptions(options));
    return this;
  }

  jpeg(options = {}) {
    return this.toFormat('jpeg', options);
  }

  png(options = {}) {
    return this.toFormat('png', options);
  }

  webp(options = {}) {
    return this.toFormat('webp', options);
  }

  gif(options = {}) {
    return this.toFormat('gif', options);
  }

  avif(options = {}) {
    return this.toFormat('avif', options);
  }

  jxl(options = {}) {
    return this.toFormat('jxl', options);
  }

  qoi(options = {}) {
    return this.toFormat('qoi', options);
  }

  toColourspace(space) {
    this._spec.output.colourspace = String(space).toLowerCase();
    return this;
  }

  toColorspace(space) {
    return this.toColourspace(space);
  }

  pipelineColourspace(space) {
    this._spec.pipelineColourspace = String(space).toLowerCase();
    return this;
  }

  pipelineColorspace(space) {
    return this.pipelineColourspace(space);
  }

  raw(options = {}) {
    this._spec.output.format = 'raw';
    Object.assign(this._spec.output, mapOutputOptions(options));
    return this;
  }

  tile(options = {}) {
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

  heif() {
    throw new Error('HEIF/HEIC output is not supported; use avif()');
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
    this._spec.output.withMetadata = { ...options };
    return this;
  }

  keepExif() {
    this._spec.output.keepExif = true;
    return this;
  }

  withExif(exif) {
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
    this._spec.output.withExifMerge = exif;
    return this;
  }

  keepIccProfile() {
    this._spec.output.keepIcc = true;
    return this;
  }

  withIccProfile(profile) {
    if (Buffer.isBuffer(profile)) {
      this._spec.output.withIccRaw = profile.toString('base64');
    } else if (typeof profile === 'string' && fs.existsSync(profile)) {
      this._spec.output.withIccRaw = fs.readFileSync(profile).toString('base64');
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
    this._spec.output.withDensity = dpi;
    return this;
  }

  timeout(options = {}) {
    this._spec.output.timeout = typeof options === 'number' ? options : options.seconds;
    return this;
  }

  async metadata() {
    const info = await native.metadataSync(await this._resolvedInput(), this._spec);
    for (const key of ['exif', 'icc', 'xmp']) {
      if (typeof info[key] === 'string') info[key] = Buffer.from(info[key], 'base64');
    }
    return info;
  }

  async stats() {
    return native.stats(await this._resolvedInput(), this._spec);
  }

  async probe() {
    return native.probeSync(await this._resolvedInput(), this._spec);
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
      const result = await this._tilesBuffer(this._tile);
      const output = options.resolveWithObject ? result : result.data;
      if (callback) {
        process.nextTick(() => callback(null, result.data, result.info));
        return undefined;
      }
      return output;
    }
    const input = await this._resolvedInput();
    queueDepth += 1;
    queue.emit('change', queueDepth);
    let result;
    try {
      result = await native.process(input, this._spec);
    } catch (error) {
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
    const data = new Uint8Array(result.data);
    if (callback) {
      process.nextTick(() => callback(null, data, result.info));
      return undefined;
    }
    return data;
  }

  async toFile(file, options = {}, callback) {
    if (typeof options === 'function') {
      callback = options;
      options = {};
    }
    if (this._tile) {
      const info = await this._writeTiles(file, this._tile);
      if (callback) {
        process.nextTick(() => callback(null, info));
        return undefined;
      }
      return info;
    }
    if (!this._spec.output.format) {
      const ext = path.extname(file).slice(1);
      if (ext) this._spec.output.format = normalizeFormat(ext);
    }
    try {
      const info = await native.toFile(await this._resolvedInput(), file, this._spec);
      if (callback) {
        process.nextTick(() => callback(null, info));
        return undefined;
      }
      return info;
    } catch (error) {
      if (callback) {
        process.nextTick(() => callback(error));
        return undefined;
      }
      throw error;
    }
  }

  async _buildTiles(tile, root) {
    const result = await native.process(await this._resolvedInput(), this._spec);
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
    const background = tile.background || '#ffffff';
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

function image(input, options = {}) {
  return new Sharp(input, options);
}

image.Sharp = Sharp;
image.default = image;
image.sharp = image;

image.cache = function cache() {
  return { memory: 0, files: 0, items: 0 };
};

image.concurrency = function concurrency(value) {
  if (value === undefined) return native.getConcurrency();
  return native.setConcurrency(value);
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
  mitchell: 'mitchell',
  lanczos2: 'lanczos2',
  lanczos3: 'lanczos3',
  nohalo: 'lanczos3',
  vsqbs: 'bilinear',
};
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
