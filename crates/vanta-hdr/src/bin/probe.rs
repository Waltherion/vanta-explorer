//! `probe <file>...` decodes and reports what was detected.
//! `probe --encode <in> <out> [quality]` decodes, then writes it back out in the format the
//! output extension names, so the encoders can be checked against other tools' readers.
//! A binary so the colour pipeline can be checked against real files without a GPU.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: probe <file>... | probe --encode <in> <out> [quality]");
        std::process::exit(2);
    }
    if args[0] == "--encode" {
        let [_, input, output] = &args[..3] else {
            eprintln!("usage: probe --encode <in> <out> [quality]");
            std::process::exit(2);
        };
        let quality: u8 = args.get(3).and_then(|q| q.parse().ok()).unwrap_or(92);
        let img = match vanta_hdr::decode_image(std::path::Path::new(input), 0, 0) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("decode failed: {e}");
                std::process::exit(1);
            }
        };
        let t = std::time::Instant::now();
        match vanta_hdr::encode_image(std::path::Path::new(output), &img, Default::default(), 0, quality) {
            Ok(()) => println!(
                "wrote {output} from {}x{} {:?} {:?} hdr={} in {:?}",
                img.w, img.h, img.kind, img.primaries, img.hdr, t.elapsed()
            ),
            Err(e) => {
                eprintln!("encode failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    for a in &args {
        let path = std::path::Path::new(a);
        let t = std::time::Instant::now();
        match vanta_hdr::decode_image(path, 0, 0) {
            Ok(img) => {
                let mut peak = 0.0f32;
                let mut sum = 0.0f64;
                for px in img.rgba16f.chunks(4) {
                    for c in &px[..3] {
                        let v = c.to_f32();
                        peak = peak.max(v);
                        sum += v as f64;
                    }
                }
                let mean = sum / (img.rgba16f.len() as f64 / 4.0 * 3.0);
                println!(
                    "{}\n  {}x{} kind={:?} primaries={:?} hdr={} valid={}\n  peak={:.3} ({:.0} nits)  mean={:.3}  {:?}",
                    path.display(), img.w, img.h, img.kind, img.primaries, img.hdr, img.valid(),
                    peak, peak * 203.0, mean, t.elapsed()
                );
            }
            Err(e) => println!("{}\n  FAILED: {e}", path.display()),
        }
    }
}
