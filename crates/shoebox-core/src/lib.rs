//! Shoebox core: everything that touches the library on disk. No UI dependencies.

pub mod cache;
pub mod catalog;
pub mod changes;
pub mod cloud;
pub mod collections;
pub mod dupes;
pub mod edits;
pub mod embeddings;
pub mod error;
pub mod events;
pub mod exif;
pub mod fileops;
pub mod hash;
pub mod import;
pub mod marks;
pub mod merges;
pub mod meta;
pub mod organise;
pub mod paths;
pub mod people;
pub mod pipeline;
pub mod query;
pub mod review;
pub mod scan;
pub mod settings;
pub mod sidecar;
pub mod stacks;
pub mod template;
pub mod thumbs;
pub mod timeline;
pub mod xmp;

pub use error::{CoreError, Result};

/// Bundle identifier used for the cache and journal locations.
pub const BUNDLE_ID: &str = "com.crankyunicorn.shoebox";
