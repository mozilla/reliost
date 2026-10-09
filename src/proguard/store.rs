use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use memmap2::Mmap;
use proguard::{CacheError, ProguardCache, ProguardMapping, PRGCACHE_VERSION};
use samply_quota_manager::QuotaManagerNotifier;
use tokio::io::AsyncWriteExt;

use crate::configuration::ProguardSettings;
use crate::symbol_manager::USER_AGENT;

/// Identifies one mapping file by its ProGuard UUID. The UUID is the MD5-based
/// (version 3) UUID of the SHA-256 hex string on the `# pg_map_hash:` line of
/// the mapping file. This matches what the Sentry Gradle plugin computes. Once
/// https://bugzilla.mozilla.org/show_bug.cgi?id=2079950 lands, the UUID will
/// also be recorded in the Android package itself. Until then,
/// `scripts/extract-aab-mapping.py` computes it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MappingFileId {
    /// Lowercase and dashed, e.g. `fe506e08-58e3-3f15-9117-67ccb4d01f19`.
    /// This is the form used in the object name on the server. Checking it also
    /// makes the UUID safe to use as a path component.
    uuid: String,
}

impl MappingFileId {
    /// Validates the identifier. Only the lowercase dashed form is accepted.
    pub fn new(uuid: &str) -> Result<Self, ProguardError> {
        let is_valid = uuid.len() == 36
            && uuid.bytes().enumerate().all(|(i, b)| match i {
                8 | 13 | 18 | 23 => b == b'-',
                _ => matches!(b, b'0'..=b'9' | b'a'..=b'f'),
            });
        if !is_valid {
            return Err(ProguardError::InvalidUuid(uuid.to_owned()));
        }
        Ok(Self {
            uuid: uuid.to_owned(),
        })
    }

    /// The path of the zstd-compressed mapping file relative to a server base
    /// URL.
    fn url_path(&self) -> String {
        format!("{}/mapping.txt.zst", self.uuid)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ProguardError {
    #[error("Invalid ProGuard UUID {0:?}")]
    InvalidUuid(String),

    #[error("No servers for mapping files are configured")]
    NotConfigured,

    #[error("The mapping file was not found on any server; tried: {0}")]
    NotFound(String),

    #[error("Downloading the mapping file failed: {0}")]
    Download(String),

    #[error("The downloaded file could not be decompressed: {0}")]
    Decompression(std::io::Error),

    #[error("The downloaded file is not a valid mapping file")]
    InvalidMapping,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("The cached mapping file could not be parsed: {0}")]
    Cache(#[from] CacheError),
}

/// A memory-mapped [`ProguardCache`] file.
pub struct MappingFile {
    mmap: Mmap,
}

impl MappingFile {
    fn open(path: &Path) -> Result<Self, ProguardError> {
        let file = File::open(path)?;
        // SAFETY: Cache files are only ever written to a temporary path and
        // then renamed into place, so the file is not modified while mapped.
        let mmap = unsafe { Mmap::map(&file)? };
        let mapping_file = Self { mmap };
        // Validate the header so that a corrupt file is detected here.
        mapping_file.cache()?;
        Ok(mapping_file)
    }

    /// Parses the cache. This is cheap: it only validates the header and
    /// creates slices into the mapped data.
    pub fn cache(&self) -> Result<ProguardCache<'_>, CacheError> {
        ProguardCache::parse(&self.mmap)
    }
}

/// Downloads mapping files, converts them into [`ProguardCache`] files, and
/// keeps those on disk.
pub struct MappingFileStore {
    settings: Option<ProguardSettings>,
    client: reqwest::Client,
    quota_manager_notifiers: Vec<QuotaManagerNotifier>,
    /// One lock per mapping file which is currently being looked up, so that
    /// concurrent requests for the same file only download it once.
    in_flight: Mutex<HashMap<MappingFileId, Arc<tokio::sync::Mutex<()>>>>,
}

impl MappingFileStore {
    pub fn new(
        settings: Option<ProguardSettings>,
        quota_manager_notifiers: Vec<QuotaManagerNotifier>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("Failed to create HTTP client");
        Self {
            settings,
            client,
            quota_manager_notifiers,
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the cached mapping file, downloading and converting it first if
    /// needed.
    pub async fn get(&self, id: &MappingFileId) -> Result<MappingFile, ProguardError> {
        let settings = self.settings.as_ref().ok_or(ProguardError::NotConfigured)?;
        if settings.servers.is_empty() {
            return Err(ProguardError::NotConfigured);
        }

        let lock = self
            .in_flight
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_default()
            .clone();
        let guard = lock.lock().await;
        let result = self.get_locked(settings, id).await;
        drop(guard);

        // Remove the lock if nobody else is waiting for it. The count is only
        // incremented while `in_flight` is locked, so this check is reliable.
        let mut in_flight = self.in_flight.lock().unwrap();
        if Arc::strong_count(&lock) == 2 {
            in_flight.remove(id);
        }

        result
    }

    async fn get_locked(
        &self,
        settings: &ProguardSettings,
        id: &MappingFileId,
    ) -> Result<MappingFile, ProguardError> {
        let dir = settings.cache_dir.join(&id.uuid);
        // Include the cache format version so that a proguard crate update
        // which changes the format doesn't try to read old files.
        let cache_path = dir.join(format!("mapping.v{PRGCACHE_VERSION}.prgcache"));

        match MappingFile::open(&cache_path) {
            Ok(mapping_file) => {
                for notifier in &self.quota_manager_notifiers {
                    notifier.on_file_accessed(&cache_path, SystemTime::now());
                }
                return Ok(mapping_file);
            }
            Err(ProguardError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(
                    path = cache_path.to_string_lossy().to_string(),
                    error = e.to_string(),
                    "Removing unreadable mapping cache file"
                );
                let _ = std::fs::remove_file(&cache_path);
            }
        }

        tokio::fs::create_dir_all(&dir).await?;
        let download_path = dir.join(format!("mapping.txt.zst.{}.download", std::process::id()));
        let mapping_path = dir.join(format!("mapping.txt.{}.tmp", std::process::id()));
        let result = self
            .download_and_convert(settings, id, &download_path, &mapping_path, &cache_path)
            .await;
        let _ = tokio::fs::remove_file(&download_path).await;
        let _ = tokio::fs::remove_file(&mapping_path).await;
        let size_in_bytes = result?;

        for notifier in &self.quota_manager_notifiers {
            notifier.on_file_created(&cache_path, size_in_bytes, SystemTime::now());
            notifier.trigger_eviction_if_needed();
        }
        MappingFile::open(&cache_path)
    }

    /// Downloads the compressed mapping file to `download_path`, decompresses
    /// it to `mapping_path`, and writes the converted cache to `cache_path`.
    /// Returns the size of the cache file.
    async fn download_and_convert(
        &self,
        settings: &ProguardSettings,
        id: &MappingFileId,
        download_path: &Path,
        mapping_path: &Path,
        cache_path: &Path,
    ) -> Result<u64, ProguardError> {
        self.download(settings, id, download_path).await?;

        let download_path = download_path.to_owned();
        let mapping_path = mapping_path.to_owned();
        let cache_path = cache_path.to_owned();
        tokio::task::spawn_blocking(move || {
            decompress(&download_path, &mapping_path)?;
            convert_mapping(&mapping_path, &cache_path)
        })
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?
    }

    /// Tries each server in order until one has the file.
    async fn download(
        &self,
        settings: &ProguardSettings,
        id: &MappingFileId,
        download_path: &Path,
    ) -> Result<(), ProguardError> {
        let mut not_found_urls = Vec::new();
        let mut last_error = None;
        for server in &settings.servers {
            let url = format!("{}/{}", server.trim_end_matches('/'), id.url_path());
            let start = Instant::now();
            match self.download_from_url(&url, download_path).await {
                Ok(true) => {
                    tracing::info!(
                        url,
                        elapsed_in_seconds = start.elapsed().as_secs_f64(),
                        "Downloaded mapping file"
                    );
                    return Ok(());
                }
                Ok(false) => not_found_urls.push(url),
                Err(e) => {
                    tracing::warn!(url, error = e.to_string(), "Mapping file download failed");
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| ProguardError::NotFound(not_found_urls.join(", "))))
    }

    /// Returns `Ok(false)` if the server doesn't have the file.
    async fn download_from_url(&self, url: &str, path: &Path) -> Result<bool, ProguardError> {
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ProguardError::Download(e.to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(ProguardError::Download(format!(
                "{url} returned HTTP status {}",
                response.status()
            )));
        }

        let mut file = tokio::fs::File::create(path).await?;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ProguardError::Download(e.to_string()))?
        {
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(true)
    }
}

/// Decompresses the zstd file at `compressed_path` into `output_path`.
fn decompress(compressed_path: &Path, output_path: &Path) -> Result<(), ProguardError> {
    let start = Instant::now();
    let input = File::open(compressed_path)?;
    let mut output = BufWriter::new(File::create(output_path)?);
    zstd::stream::copy_decode(input, &mut output).map_err(ProguardError::Decompression)?;
    output.into_inner().map_err(|e| e.into_error())?;
    tracing::info!(
        path = output_path.to_string_lossy().to_string(),
        elapsed_in_seconds = start.elapsed().as_secs_f64(),
        "Decompressed mapping file"
    );
    Ok(())
}

/// Converts the mapping.txt file at `mapping_path` into a cache file at
/// `cache_path`. Returns the size of the cache file.
fn convert_mapping(mapping_path: &Path, cache_path: &Path) -> Result<u64, ProguardError> {
    let start = Instant::now();
    let file = File::open(mapping_path)?;
    // SAFETY: We created this file and nobody else writes to it.
    let mmap = unsafe { Mmap::map(&file)? };
    let mapping = ProguardMapping::new(&mmap);
    if !mapping.is_valid() {
        return Err(ProguardError::InvalidMapping);
    }

    let temp_path = PathBuf::from(format!(
        "{}.{}.tmp",
        cache_path.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| -> Result<u64, ProguardError> {
        let mut writer = BufWriter::new(File::create(&temp_path)?);
        ProguardCache::write(&mapping, &mut writer)?;
        writer
            .into_inner()
            .map_err(|e| e.into_error())?
            .sync_all()?;
        std::fs::rename(&temp_path, cache_path)?;
        Ok(std::fs::metadata(cache_path)?.len())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    if let Ok(size_in_bytes) = result {
        tracing::info!(
            path = cache_path.to_string_lossy().to_string(),
            mapping_size_in_bytes = mmap.len(),
            size_in_bytes,
            elapsed_in_seconds = start.elapsed().as_secs_f64(),
            "Created mapping cache file"
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_file_id_accepts_lowercase_dashed_uuid() {
        let id = MappingFileId::new("fe506e08-58e3-3f15-9117-67ccb4d01f19").unwrap();
        assert_eq!(
            id.url_path(),
            "fe506e08-58e3-3f15-9117-67ccb4d01f19/mapping.txt.zst"
        );
    }

    #[test]
    fn mapping_file_id_rejects_bad_input() {
        for uuid in [
            "",
            "fe506e08",
            "FE506E08-58E3-3F15-9117-67CCB4D01F19",
            "fe506e0858e33f15911767ccb4d01f19",
            "zz506e08-58e3-3f15-9117-67ccb4d01f19",
            "fe506e08-58e3-3f15-9117-67ccb4d01f1-",
            "fe506e0858-e3-3f15-9117-67ccb4d01f19",
            "../..",
        ] {
            assert!(
                matches!(MappingFileId::new(uuid), Err(ProguardError::InvalidUuid(_))),
                "{uuid:?} should be rejected"
            );
        }
    }
}
