//! iCloud Photos (`docs/PHOTOS-SPEC.md`): copy the Mac's Photos library
//! into a library root, and sync again later without copying anything
//! twice.
//!
//! Everything that decides what to copy lives here, in pure Rust, and is
//! tested against [`fake::FakeSource`]; `shoebox-photos` is only the
//! PhotoKit translation behind [`PhotoSource`].
//!
//! A sync: [`PhotoSource::fetch`] lists the library; [`plan::plan`] diffs
//! it against the [`ledger::Ledgers`] of every root; each wanted file is
//! downloaded into the cache and read ([`place::Staged`]); [`place::place`]
//! names its copies and writes its sidecars and record; the copies and
//! writes run through `fileops` in the sync's batch.

pub mod fake;
pub mod ledger;
pub mod model;
pub mod place;
pub mod plan;
pub mod push;
pub mod source;
pub mod sync;

pub use model::{
    Access, Album, Asset, AssetId, Library, Media, PushRequest, Resource, ResourceKind,
};
pub use source::PhotoSource;
