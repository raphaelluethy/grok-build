//! Legacy types for trace-export / upload config.
//!
//! Full repository-change archival (`upload_repo` / dedup blob packs) was
//! removed from this tree. The remaining re-exports are shared wire types
//! (`TraceExportConfig`, `UploadMethod`, skip-dir lists) still referenced by
//! the stubbed / gated upload paths. Those paths are permanently disabled in
//! this fork via [`crate::privacy::optional_uploads_disabled`].

pub use xai_file_utils::BlobCompression;
pub use xai_file_utils::{
    ARCHIVE_SCHEMA_VERSION, ARCHIVE_SCHEMA_VERSION_V3, DEDUP_BLOB_SUBDIR, DEDUP_GCS_PREFIX,
    DEDUP_PATCH_SUBDIR, DedupMetadata, ExcludedContent, FileReference, PatchReference,
    SKIP_DIR_NAMES, TraceExportConfig, UploadMethod, skip_dir_set,
};
