//! Runtime configuration, read once from the environment.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// Root of the photo library (mounted from the host, usually read-write).
    pub photos_root: PathBuf,
    /// Persistent app data: index database, journal, sidecar backups.
    pub config_dir: PathBuf,
    /// Thumbnail and preview cache (safe to delete at any time).
    pub cache_dir: PathBuf,
    /// Built web UI.
    pub web_dir: PathBuf,
    pub port: u16,
    /// When true nothing is ever written to the photo library.
    pub readonly: bool,
    /// Name of the reject sub folder (FastRawViewer uses `_Rejected`).
    pub reject_dir: String,
    /// Minutes between background re-scans (0 = never).
    pub rescan_minutes: u64,
    /// Build thumbnails for the whole library in the background.
    pub pregen_thumbs: bool,
    /// RAM budget for developed RAW data (MB).
    pub raw_cache_mb: usize,
    /// Disk budget for medium-size previews (MB).
    pub preview_cache_mb: usize,
    /// Parallel metadata readers while indexing.
    pub index_threads: usize,
    /// Parallel RAW developments (each one is multi-threaded itself).
    pub decode_concurrency: usize,
    /// Optional single password protecting the UI.
    pub password: Option<String>,
    pub exiftool: String,
    /// `libraw`: decode every RAW file with LibRaw instead of only when FlashRaw cannot.
    pub raw_decoder_libraw: bool,
}

fn env_str(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    match env_str(key).as_deref().map(|s| s.to_ascii_lowercase()) {
        Some(v) if ["1", "true", "yes", "on"].contains(&v.as_str()) => true,
        Some(v) if ["0", "false", "no", "off"].contains(&v.as_str()) => false,
        _ => default,
    }
}

fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    env_str(key).and_then(|s| s.parse().ok()).unwrap_or(default)
}

impl Config {
    pub fn from_env() -> Config {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        let config_dir = PathBuf::from(env_str("HYPERFOCAL_CONFIG").unwrap_or_else(|| "/config".into()));
        let cache_dir = env_str("HYPERFOCAL_CACHE").map(PathBuf::from).unwrap_or_else(|| config_dir.join("cache"));
        let reject_dir = env_str("HYPERFOCAL_REJECT_DIR").unwrap_or_else(|| "_Rejected".into());
        let reject_dir = if reject_dir.contains('/') || reject_dir.contains('\\') || reject_dir.starts_with("..") {
            "_Rejected".into()
        } else {
            reject_dir
        };
        Config {
            photos_root: PathBuf::from(env_str("HYPERFOCAL_PHOTOS").unwrap_or_else(|| "/photos".into())),
            config_dir,
            cache_dir,
            web_dir: PathBuf::from(env_str("HYPERFOCAL_WEB").unwrap_or_else(|| "/app/web".into())),
            port: env_num("HYPERFOCAL_PORT", 8080),
            readonly: env_bool("HYPERFOCAL_READONLY", false),
            reject_dir,
            rescan_minutes: env_num("HYPERFOCAL_RESCAN_MINUTES", 15),
            pregen_thumbs: env_bool("HYPERFOCAL_PREGEN_THUMBS", true),
            raw_cache_mb: env_num("HYPERFOCAL_RAW_CACHE_MB", 1536),
            preview_cache_mb: env_num("HYPERFOCAL_PREVIEW_CACHE_MB", 4096),
            index_threads: env_num("HYPERFOCAL_INDEX_THREADS", cores.clamp(2, 8)),
            decode_concurrency: env_num("HYPERFOCAL_DECODE_CONCURRENCY", (cores / 4).clamp(1, 4)),
            password: env_str("HYPERFOCAL_PASSWORD"),
            exiftool: env_str("HYPERFOCAL_EXIFTOOL").unwrap_or_else(|| "exiftool".into()),
            raw_decoder_libraw: env_str("HYPERFOCAL_RAW_DECODER").is_some_and(|v| v.eq_ignore_ascii_case("libraw")),
        }
    }
}
