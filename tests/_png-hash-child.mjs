import engine from '../packages/image/index.js';
import { allHashes } from './_png-cases.mjs';
const threads = Number(process.argv[2]);
if (threads) engine.concurrency(threads);
process.stdout.write(JSON.stringify({ threads: engine.concurrency(), hashes: await allHashes() }));
