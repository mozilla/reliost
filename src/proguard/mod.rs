//! Deobfuscation of Java stack frames using Android R8/ProGuard mapping files.

pub mod api;
mod store;

pub use store::{MappingFile, MappingFileId, MappingFileStore, ProguardError};
