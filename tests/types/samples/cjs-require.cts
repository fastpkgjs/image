// CommonJS consumers: `import x = require()` (types) and `require()` (runtime-typed any).
// Rewritten to 'sharp' for the sharp pass.
import sharp = require('@fastpkg/image');

const s: sharp.Sharp = sharp('a.jpg');
const m: Promise<sharp.Metadata> = s.metadata();
const opts: sharp.ResizeOptions = { width: 1, fit: 'inside' };
void [m, opts, s.resize(opts).jpeg({ quality: 80 })];
