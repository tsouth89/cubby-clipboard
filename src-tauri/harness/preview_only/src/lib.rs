//! Compiles the same `clip_list` mapping the app uses, without the Windows crate.
//!
//! `cargo test --lib` for cubby cannot build on Linux (windows crate, hwnd,
//! paste engine). This crate path-includes `src/clip_list.rs` so the
//! preview_only contract still runs here.

#[path = "../../../src/clip_list.rs"]
mod clip_list;

pub use clip_list::{details_item_content, list_item_content, list_item_notes, list_item_preview};
