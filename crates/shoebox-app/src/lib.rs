//! The Shoebox egui application (a thin layer over `shoebox-core`).

pub mod app;
mod display;
pub mod jobs;
mod panels;
pub mod textures;
mod views;

pub use app::{AppPaths, ShoeboxApp, View};
