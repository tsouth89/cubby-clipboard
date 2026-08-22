#![allow(dead_code)]
//! Compiles the same `backup` module the app uses, without the Windows crate.
//!
//! `cargo test --lib` for cubby cannot build on Linux (windows crate, hwnd,
//! paste engine). This crate path-includes `src/backup.rs` so the SBS-919
//! original-bytes contract still runs here.

#[path = "../../../src/crypto.rs"]
mod crypto;

mod clipboard;
mod database;
mod search_index;

#[path = "../../../src/image_persist.rs"]
mod image_persist;

#[path = "../../../src/backup_import_optional.rs"]
mod backup_import_optional;

#[path = "../../../src/managed_image.rs"]
mod managed_image;

#[path = "../../../src/backup.rs"]
mod backup;
