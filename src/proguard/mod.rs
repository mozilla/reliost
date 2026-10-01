//! Deobfuscation of Java stack frames using Android R8/ProGuard mapping files.

mod store;

pub use store::{MappingFile, MappingFileId, MappingFileStore, ProguardError};
