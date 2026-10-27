// Side-effect import: selects sharp's exact JPEG shrink-on-load rule (FASTPKG_JPEG_SHRINK=sharp)
// for tests that pin PSNR against sharp. Import before the engine.
process.env.FASTPKG_JPEG_SHRINK = 'sharp';
