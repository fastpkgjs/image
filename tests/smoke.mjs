import image from '../packages/image/index.js';

const meta = await image('fixtures/photo.jpg').metadata();
console.log('metadata', meta);

const out = await image('fixtures/photo.jpg')
  .resize(200, 150)
  .webp({ lossless: true })
  .toBuffer({ resolveWithObject: true });
console.log('webp', out.info, out.data.length, out.data.subarray(0, 12).toString('hex'));

await image('fixtures/photo.jpg').resize({ width: 320 }).rotate(90).png().toFile('fixtures/out.png');
console.log('out.png', await image('fixtures/out.png').metadata());

const cover = await image('fixtures/photo.jpg').resize(100, 100, { fit: 'cover' }).toBuffer();
console.log('cover', cover.length);

const filtered = await image('fixtures/photo.jpg').blur(3).sharpen().toBuffer();
console.log('filtered', filtered.length);

const stats = await image('fixtures/photo.jpg').stats();
console.log('stats', JSON.stringify(stats).slice(0, 400));

const qoi = await image('fixtures/photo.png').qoi().toBuffer(); console.log('qoi', qoi.length);
const raw = await image('fixtures/photo.png').raw().toBuffer({ resolveWithObject: true });
console.log('raw', raw.info, raw.data.length);

const svg = Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><rect width="64" height="64" fill="red"/></svg>');
const png = await image(svg).png().toBuffer();
console.log('svg png', png.length, await image(png).metadata());

const text = await image({ text: { text: 'Fastpkg', width: 240, height: 80, align: 'center' } }).png().toBuffer();
console.log('text png', text.length);
