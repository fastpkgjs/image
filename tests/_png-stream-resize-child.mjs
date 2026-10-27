// Child of png-stream-resize.test.mjs: runs the jobs of a manifest through the engine (the PNG
// stream/shrink thresholds come from the environment, which the engine reads once) and prints
// one JSON line per job with the output pixels (or the error message).
import fs from 'node:fs';
import engine from '../packages/image/index.js';

const jobs = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const results = [];
for (const job of jobs) {
  try {
    const input = engine(fs.readFileSync(job.file), { limitInputPixels: false, ...(job.failOn ? { failOn: job.failOn } : {}) });
    const { data, info } = await eval(job.pipeline)(input).toBuffer({ resolveWithObject: true });
    results.push({ width: info.width, height: info.height, channels: info.channels, depth: info.depth, data: data.toString('base64') });
  } catch (error) {
    results.push({ error: String(error.message) });
  }
}
console.log(JSON.stringify(results));
