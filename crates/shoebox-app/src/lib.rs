//! The Shoebox egui application (a thin layer over `shoebox-core`).

pub mod app;
mod display;
pub mod jobs;
pub mod menu;
mod panels;
pub mod people;
mod platform;
pub mod semantic;
pub mod textures;
pub mod updater;
mod views;
mod watch;

pub use app::{AppPaths, ShoeboxApp, View};
