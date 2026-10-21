import sharp from 'sharp';

const meta = await sharp('fixtures/photo.jpg').metadata();
console.log(JSON.stringify(meta, null, 2));
console.log(sharp.versions);
