//! Shoebox core: everything that touches the library on disk. No UI dependencies.

pub mod cache;
pub mod catalog;
pub mod dupes;
pub mod error;
pub mod exif;
pub mod fileops;
pub mod hash;
pub mod meta;
pub mod paths;
pub mod pipeline;
pub mod scan;
pub mod settings;
pub mod thumbs;

pub use error::{CoreError, Result};

/// Bundle identifier used for the cache and journal locations.
pub const BUNDLE_ID: &str = "com.crankyunicorn.shoebox";
