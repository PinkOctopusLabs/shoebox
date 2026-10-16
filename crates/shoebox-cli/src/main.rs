fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    tracing::info!("shoebox {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}
