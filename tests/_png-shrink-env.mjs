// Side-effect import: lets small test images take the decode-time PNG shrink path
// (production only shrinks images of at least 128 MB decoded as RGBA: 32 MP of 8-bit, 16 MP of 16-bit). Import before the engine.
process.env.FASTPKG_PNG_SHRINK_MIN_BYTES = '0';
