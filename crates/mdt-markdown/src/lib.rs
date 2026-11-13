//! pulldown-cmark to block model: layout units, source maps, heading slugs, rendered-text maps.

mod build;
pub mod html;
pub mod model;
pub mod prefix;
pub mod slug;

pub use build::{SPLIT_THRESHOLD, options, parse, parse_with, plain_text};
pub use model::*;

#[cfg(test)]
mod tests;
