//! Font registry, Parley layout, inline boxes and the layout cache.

pub mod cache;
pub mod emoji;
pub mod fonts;
pub mod layout;
pub mod paint;

pub use cache::{CacheKey, LayoutCache};
pub use fonts::{FontRegistry, GpuiFonts, Role};
pub use layout::{
    BaseStyle, InlineBoxSpec, OBJECT_CHAR, Paragraph, StyleRun, layout_cell, layout_paragraph,
};
pub use paint::{
    Brush, PaintOp, Rect, paint, paint_ops, ranges_rects, selection_rects, visible_bytes,
};

#[cfg(test)]
mod layout_tests;
