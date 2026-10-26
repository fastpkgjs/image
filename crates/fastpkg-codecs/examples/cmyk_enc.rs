// Scratch tool: writes CMYK / YCCK JPEGs from generated ink values (Adobe-inverted storage).
// usage: cmyk_enc <out.jpg> <random|kramp|grid> <cmyk|ycck> [k]
use mozjpeg::{ColorSpace, Compress};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (w, h) = (128usize, 128usize);
    let mut px = vec![0u8; w * h * 4];
    let mut seed = 12345u32;
    let mut rnd = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 24) as u8
    };
    let k_fixed: u8 = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(0);
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let ink: [u8; 4] = match args[2].as_str() {
                "random" => [rnd(), rnd(), rnd(), rnd()],
                "kramp" => {
                    let v = (x * 255 / (w - 1)) as u8;
                    [0, 0, 0, v]
                }
                _ => {
                    // 16 x 16 cells over C and M, rows over Y; K fixed.
                    let c = ((x * 16 / w) * 17) as u8;
                    let m = ((y % 16) * 17) as u8;
                    let yy = ((y * 8 / h) * 36) as u8;
                    [c, m, yy, k_fixed]
                }
            };
            for i in 0..4 {
                px[o + i] = 255 - ink[i];
            }
        }
    }
    let mut c = Compress::new(ColorSpace::JCS_CMYK);
    c.set_size(w, h);
    if args[3] == "ycck" {
        c.set_color_space(ColorSpace::JCS_YCCK);
    }
    c.set_quality(100.0);
    let mut started = c.start_compress(Vec::new()).unwrap();
    started.write_scanlines(&px).unwrap();
    std::fs::write(&args[1], started.finish().unwrap()).unwrap();
}
