// ESM import forms that must work for both packages. Rewritten to 'sharp' for the sharp pass.
import sharp from '@fastpkg/image';
import type { Metadata, Sharp } from '@fastpkg/image';

const s: Sharp = sharp('a.jpg');
export const m: Promise<Metadata> = s.metadata();
