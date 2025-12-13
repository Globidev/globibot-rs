pub mod imageops;

pub use anyhow;
pub use gif;
pub use image;

pub fn load_env(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|why| panic!("Failed to load environment variable '{key}': {why}"))
}
