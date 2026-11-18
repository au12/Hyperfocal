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
    /// Rename darktable/digiKam-style sidecars (`IMG_1.CR3.xmp`) to `IMG_1.xmp`.
    pub rename_sidecars: bool,
    /// `libraw`: decode every RAW file with LibRaw instead of only when FlashRaw cannot.
    pub raw_decoder_libraw: bool,
    /// Archive integrity monitor: hash every original and verify it again over time.
    pub integrity: bool,
    /// Days in which every original is verified once.
    pub verify_days: u32,
    /// The nightly window for verification: start (local hour, the container's TZ) and length.
    pub verify_hour: u32,
    pub verify_minutes: u32,
    /// Read rate while hashing (MB/s).
    pub hash_mbps: u32,
    /// Where integrity alerts go: an ntfy topic, a Gotify `/message` URL or a webhook.
    pub alert_url: Option<String>,
    /// `auto` (from the URL), `ntfy`, `gotify` or `json`.
    pub alert_format: String,
    /// Token for the alert service (ntfy access token, Gotify app token, webhook bearer).
    pub alert_token: Option<String>,
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
    /// A configuration for tests: library in `root/photos`, app data in `root/config`,
    /// no background work.
    pub fn for_tests(root: &std::path::Path) -> Config {
        Config {
            photos_root: root.join("photos"),
            config_dir: root.join("config"),
            cache_dir: root.join("config").join("cache"),
            web_dir: root.join("web"),
            port: 0,
            readonly: false,
            reject_dir: "_Rejected".into(),
            rescan_minutes: 0,
            pregen_thumbs: false,
            raw_cache_mb: 16,
            preview_cache_mb: 16,
            index_threads: 2,
            decode_concurrency: 1,
            password: None,
            exiftool: "exiftool".into(),
            rename_sidecars: true,
            raw_decoder_libraw: false,
            integrity: false,
            verify_days: 90,
            verify_hour: 4,
            verify_minutes: 120,
            hash_mbps: 0,
            alert_url: None,
            alert_format: "auto".into(),
            alert_token: None,
        }
    }

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
            rename_sidecars: env_bool("HYPERFOCAL_RENAME_SIDECARS", true),
            raw_decoder_libraw: env_str("HYPERFOCAL_RAW_DECODER").is_some_and(|v| v.eq_ignore_ascii_case("libraw")),
            integrity: env_bool("HYPERFOCAL_INTEGRITY", true),
            verify_days: env_num("HYPERFOCAL_VERIFY_DAYS", 90u32).clamp(1, 3650),
            verify_hour: env_num("HYPERFOCAL_VERIFY_HOUR", 4u32).min(23),
            verify_minutes: env_num("HYPERFOCAL_VERIFY_MINUTES", 120u32).clamp(1, 1440),
            hash_mbps: env_num("HYPERFOCAL_HASH_MBPS", 60u32),
            alert_url: env_str("HYPERFOCAL_ALERT_URL"),
            alert_format: env_str("HYPERFOCAL_ALERT_FORMAT").unwrap_or_else(|| "auto".into()),
            alert_token: env_str("HYPERFOCAL_ALERT_TOKEN"),
        }
    }
}
