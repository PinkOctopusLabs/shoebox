//! On-disk thumbnail cache (JPEG, sRGB, upright) in the app's cache folder.
//! Keyed by path, size and mtime, so an edited file gets a new thumbnail.

use std::path::{Path, PathBuf};

use shoebox_decode::{Format, Rgb8, Target};

use crate::error::{CoreError, Result, io};

/// Bump when rendering changes (colour, resampling), to regenerate.
/// 3: JPEG previews are MPF large thumbnails only (an iPhone's depth map
/// was shown as a Portrait photo's thumbnail).
pub const RENDER_VERSION: u32 = 3;

/// The two cached sizes from the spec.
pub const SMALL: u32 = 256;
pub const LARGE: u32 = 1024;

const QUALITY: u8 = 85;

#[derive(Debug, Clone)]
pub struct ThumbCache {
    dir: PathBuf,
}

/// What identifies a source file's current contents.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub path: &'a Path,
    pub format: Format,
    pub size: u64,
    pub mtime_ns: i64,
    pub orientation: u16,
    /// Colour space the thumbnail is rendered in (part of the cache key).
    pub target: Target,
}

impl ThumbCache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn file_for(&self, src: &Source<'_>, edge: u32) -> PathBuf {
        let mut h = blake3::Hasher::new();
        h.update(src.path.as_os_str().as_encoded_bytes());
        h.update(&src.size.to_le_bytes());
        h.update(&src.mtime_ns.to_le_bytes());
        h.update(&src.orientation.to_le_bytes());
        h.update(match src.target {
            Target::Srgb => b"srgb",
            Target::DisplayP3 => b"dp3_",
        });
        h.update(&RENDER_VERSION.to_le_bytes());
        let k = h.finalize().to_hex();
        self.dir
            .join(edge.to_string())
            .join(&k[..2])
            .join(format!("{k}.jpg"))
    }

    /// The cached thumbnail, if present and readable. Never touches the source.
    pub fn cached(&self, src: &Source<'_>, edge: u32) -> Option<Rgb8> {
        let bytes = std::fs::read(self.file_for(src, edge)).ok()?;
        shoebox_decode::jpeg::decode_rgb(&bytes)
            .ok()
            .map(|(img, _)| img)
    }

    /// The cached thumbnail, or render it from the source and cache it.
    pub fn get(&self, src: &Source<'_>, edge: u32) -> Result<Rgb8> {
        if let Some(img) = self.cached(src, edge) {
            return Ok(img);
        }
        let img = shoebox_decode::render_path(
            src.format,
            src.path,
            Some(edge),
            src.orientation,
            src.target,
        )
        .map_err(|e| CoreError::Decode {
            path: src.path.to_path_buf(),
            source: e,
        })?;
        if let Err(e) = self.store(src, edge, &img) {
            tracing::warn!("thumbnail cache write failed: {e}");
        }
        Ok(img)
    }

    fn store(&self, src: &Source<'_>, edge: u32, img: &Rgb8) -> Result<()> {
        let path = self.file_for(src, edge);
        let dir = path.parent().unwrap_or(&self.dir);
        std::fs::create_dir_all(dir).map_err(io(dir))?;
        let mut jpeg = Vec::new();
        let (w, h) = (
            u16::try_from(img.width)
                .map_err(|_| CoreError::Invalid("thumbnail too wide".into()))?,
            u16::try_from(img.height)
                .map_err(|_| CoreError::Invalid("thumbnail too tall".into()))?,
        );
        jpeg_encoder::Encoder::new(&mut jpeg, QUALITY)
            .encode(&img.px, w, h, jpeg_encoder::ColorType::Rgb)
            .map_err(|e| CoreError::Invalid(format!("thumbnail encode: {e}")))?;
        // Unique temp name per writer, then rename: concurrent writers of the
        // same thumbnail just replace each other's identical output.
        let tmp = path.with_extension(format!("{}.tmp", std::process::id() ^ thread_tag()));
        std::fs::write(&tmp, &jpeg).map_err(io(&tmp))?;
        std::fs::rename(&tmp, &path).map_err(io(&path))?;
        Ok(())
    }
}

fn thread_tag() -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut h);
    h.finish() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_caches_and_invalidates() {
        let t = tempfile::tempdir().unwrap();
        let src_path = t.path().join("a.jpg");
        let img = shoebox_fixtures::scene(4, 1200, 800);
        std::fs::write(&src_path, shoebox_fixtures::jpeg(&img, 90).unwrap()).unwrap();
        let cache = ThumbCache::new(t.path().join("cache"));
        let mut src = Source {
            path: &src_path,
            format: Format::Jpeg,
            size: 1,
            mtime_ns: 1,
            orientation: 6,
            target: Target::Srgb,
        };
        assert!(cache.cached(&src, SMALL).is_none());
        let a = cache.get(&src, SMALL).unwrap();
        assert_eq!((a.width, a.height), (171, 256), "upright portrait");
        assert!(cache.cached(&src, SMALL).is_some());
        src.mtime_ns = 2;
        assert!(
            cache.cached(&src, SMALL).is_none(),
            "edited file → new thumbnail"
        );
        let before = std::fs::read(&src_path).unwrap();
        cache.get(&src, LARGE).unwrap();
        assert_eq!(
            std::fs::read(&src_path).unwrap(),
            before,
            "source untouched"
        );
    }
}
