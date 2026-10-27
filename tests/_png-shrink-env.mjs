// Side-effect import: lets small test images take the decode-time PNG shrink path
// (production only shrinks images of at least 32 MP). Import before the engine.
process.env.FASTPKG_PNG_SHRINK_MIN_PIXELS = '0';
