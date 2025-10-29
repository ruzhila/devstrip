#[cfg(feature = "gui")]
pub fn main() {
    devstrip::gui::run();
}

#[cfg(not(feature = "gui"))]
pub fn main() {
    devstrip::cli::run();
}
