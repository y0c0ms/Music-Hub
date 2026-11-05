//! Shell-outs to `ffprobe`, `yt-dlp`, `python3 + mutagen`, and the Essentia
//! feature extractor, plus pure tag-cleaning helpers. Fleshed out per phase.
#![forbid(unsafe_code)]

pub mod acquire;
pub mod bookfetch;
pub mod ffprobe;
pub mod tags;
pub mod ytdlp;

/// Anything that can go wrong invoking an external media tool.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{tool} failed: {message}")]
    Tool { tool: &'static str, message: String },
    #[error("parse: {0}")]
    Parse(String),
}
