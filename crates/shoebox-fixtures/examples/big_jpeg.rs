//! Write one synthetic 24 MP JPEG, for throughput tests.
fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: big_jpeg <out.jpg>"))?;
    let img = shoebox_fixtures::scene(11, 6048, 4032);
    std::fs::write(out, shoebox_fixtures::jpeg(&img, 92)?)?;
    Ok(())
}
