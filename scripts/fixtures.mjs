import fs from 'node:fs';
import sharp from 'sharp';

fs.mkdirSync('fixtures', { recursive: true });

const width = 800;
const height = 600;
const data = Buffer.allocUnsafe(width * height * 4);
for (let y = 0; y < height; y++) {
  for (let x = 0; x < width; x++) {
    const i = (y * width + x) * 4;
    data[i] = (x * 255 / width) | 0;
    data[i + 1] = (y * 255 / height) | 0;
    data[i + 2] = (x + y) % 256;
    data[i + 3] = 255;
  }
}

const input = { raw: { width, height, channels: 4 } };
await sharp(data, input).jpeg({ quality: 88 }).toFile('fixtures/photo.jpg');
await sharp(data, input).png().toFile('fixtures/photo.png');
await sharp(data, input).webp({ lossless: true }).toFile('fixtures/photo.webp');

console.log('fixtures/photo.{jpg,png,webp} created');
