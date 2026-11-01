//! Everything that touches sample data: decoding, playback, analysis, export.
//! No egui in here.

pub mod decode;
pub mod engine;
pub mod export;
pub mod peaks;
pub mod stats;

#[cfg(test)]
mod tests;
