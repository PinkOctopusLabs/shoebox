# Vendored crates

## candle-metal-kernels 0.11.0

The crates.io release (MIT OR Apache-2.0; candle's licence files added),
unchanged except for one line in `src/kernel.rs`:
shaders are compiled as Metal Shading Language 3.2
(`MTLCompileOptions.languageVersion`).

Without it, candle compiles its kernels with the OS default language
version, which is Metal 4 from macOS 27. MLX's GEMM kernels
(`mlx_gemm.metal`) don't compile as Metal 4, because `simdgroup_matrix`
changed, so every matrix multiply on the GPU fails with "Error while loading
library". Shoebox's minimum OS is macOS 27, so Metal 3.2 is always
available.

Drop this patch (the `[patch.crates-io]` entry in the root `Cargo.toml`)
once a candle release compiles on macOS 27. To check: remove it and run
`cargo run --release -p shoebox-ml --example probe` with a model folder.
