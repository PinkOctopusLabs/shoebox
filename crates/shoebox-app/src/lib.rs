//! The Shoebox egui application (a thin layer over `shoebox-core`).

pub mod app;
pub mod develop;
mod display;
pub mod icons;
pub mod jobs;
pub mod menu;
pub mod models;
pub mod objects;
mod panels;
pub mod people;
mod picker;
mod platform;
pub mod semantic;
pub mod textures;
mod theme;
pub mod updater;
pub mod views;
mod watch;

pub use app::{AppPaths, ShoeboxApp, View};
