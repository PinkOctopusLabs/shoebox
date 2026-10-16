//! Shoebox core: everything that touches the library on disk. No UI dependencies.

pub mod cache;
pub mod catalog;
pub mod dupes;
pub mod error;
pub mod exif;
pub mod fileops;
pub mod hash;
pub mod marks;
pub mod meta;
pub mod paths;
pub mod pipeline;
pub mod query;
pub mod scan;
pub mod settings;
pub mod sidecar;
pub mod stacks;
pub mod thumbs;
pub mod xmp;

pub use error::{CoreError, Result};

/// Bundle identifier used for the cache and journal locations.
pub const BUNDLE_ID: &str = "com.crankyunicorn.shoebox";
