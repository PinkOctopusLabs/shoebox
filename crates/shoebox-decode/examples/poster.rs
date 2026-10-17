//! A movie's poster frame and header: `cargo run -p shoebox-decode --example poster -- <file.mov or .mp4>`.

fn main() {
    for arg in std::env::args().skip(1) {
        let path = std::path::Path::new(&arg);
        let info = std::fs::File::open(path).map(|mut f| shoebox_decode::mov::info(&mut f));
        println!("{arg}: {info:?}");
        match shoebox_decode::render_path(
            shoebox_decode::Format::Mov,
            path,
            Some(512),
            1,
            shoebox_decode::Target::Srgb,
        ) {
            Ok(img) => {
                let c = img.px.len() / 2 / 3 * 3;
                println!(
                    "  poster {}x{}, centre pixel {:?}",
                    img.width,
                    img.height,
                    &img.px[c..c + 3]
                );
            }
            Err(e) => println!("  poster failed: {e}"),
        }
    }
}
