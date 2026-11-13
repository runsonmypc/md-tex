//! GPUI shell: windows, views, painting, input, palette, settings and IPC.

pub mod app_settings;
#[cfg(feature = "bench")]
pub mod bench;
pub mod editor;
pub mod field;
pub mod find;
pub mod ipc;
pub mod keymap;
pub mod launch;
pub mod links;
pub mod menu;
pub mod notices;
pub mod palette;
pub mod persist;
pub mod prompt;
pub mod reader;
pub mod recent;
pub mod settings;
pub mod theme;
pub mod view;
