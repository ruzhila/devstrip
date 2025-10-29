pub mod core;

#[cfg(feature = "gui")]
pub mod gui;

#[cfg(feature = "cli")]
pub mod cli;

#[cfg(all(not(feature = "gui"), not(feature = "cli")))]
compile_error!("Enable either the `gui` or `cli` feature.");

#[cfg(all(feature = "gui", feature = "cli"))]
compile_error!("Select only one of `gui` or `cli` features at a time.");
