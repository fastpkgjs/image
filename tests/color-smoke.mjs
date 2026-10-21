import image, { Gradient } from '../packages/image/index.js';

const solid = (color, w = 8, h = 8) =>
  image({ create: { width: w, height: h, channels: 4, background: color } }).raw().toBuffer();

async function pixel(buf, idx = 0) {
  return [buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]];
}

let b = await image({ create: { width: 4, height: 4, channels: 4, background: '#ff0000' } })
  .replaceColor('#ff0000', '#00ff00')
  .raw()
  .toBuffer();
console.log('replaceColor', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#808080' } })
  .recolor('#0000ff')
  .raw()
  .toBuffer();
console.log('recolor', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#000000' } })
  .duotone('#0000ff', '#ffff00')
  .raw()
  .toBuffer();
console.log('duotone', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#808080' } })
  .gradientMap(image.gradient().addStop(0, '#000000').addStop(1, '#ff0000'))
  .raw()
  .toBuffer();
console.log('gradientMap', await pixel(b));

b = await image({ create: { width: 4, height: 1, channels: 4, background: '#808080' } })
  .gradientFill(new Gradient().linear(0).addStop(0, '#000000').addStop(1, '#ffffff'))
  .raw()
  .toBuffer();
console.log('gradientFill', await pixel(b, 0), await pixel(b, 12));

const cube = 'LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n';
b = await image({ create: { width: 4, height: 4, channels: 4, background: '#336699' } })
  .applyLut(cube)
  .raw()
  .toBuffer();
console.log('applyLut', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#808080' } })
  .curves({ rgb: [[0, 0], [0.5, 0.8], [1, 1]] })
  .raw()
  .toBuffer();
console.log('curves', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#808080' } })
  .levels({ inputBlack: 16, inputWhite: 240, gamma: 1.2 })
  .raw()
  .toBuffer();
console.log('levels', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#808080' } })
  .whiteBalance({ temperature: 50, tint: 10 })
  .raw()
  .toBuffer();
console.log('whiteBalance', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#808080' } })
  .colourBalance({ shadows: [20, 0, -20], midtones: [0, 10, 0], highlights: [-10, 0, 10] })
  .raw()
  .toBuffer();
console.log('colourBalance', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#7f7f7f' } })
  .posterize(2)
  .raw()
  .toBuffer();
console.log('posterize', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#c8c8c8' } })
  .solarize(128)
  .raw()
  .toBuffer();
console.log('solarize', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#ff0000' } })
  .fill('#0000ff')
  .raw()
  .toBuffer();
console.log('fill', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#ff0000' } })
  .paletteSwap(['#000000', '#ffffff'])
  .raw()
  .toBuffer();
console.log('paletteSwap', await pixel(b));

b = await image({ create: { width: 4, height: 4, channels: 4, background: '#ffffff' } })
  .alphaFromLuma()
  .raw()
  .toBuffer();
console.log('alphaFromLuma white', await pixel(b));
