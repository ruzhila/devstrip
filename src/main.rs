#[cfg(feature = "gui")]
pub fn main() {
    devstrip::gui::run();
}

#[cfg(feature = "cli")]
pub fn main() {
    devstrip::cli::run();
}
