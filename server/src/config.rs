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
    /// Make previews of recent imports ahead of the first look, at idle.
    pub pregen_previews: bool,
    /// How recent (days since indexed) an import is for that.
    pub pregen_days: u32,
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
    /// Culling help: measure the sharpness, eyes and exposure of every photo.
    pub cull: bool,
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
    /// The AI features (faces and pets, search by content, eyes): which are on, the
    /// device, and where their models go (`crate::ml`).
    pub ml: crate::ml::MlConfig,
    /// Long side of the image sent for analysis (px).
    pub ml_size: usize,
    /// Photos in flight to the AI engine at once.
    pub ml_concurrency: usize,
    /// Write confirmed names to the sidecars (keywords, face regions, person shown).
    pub people_xmp: bool,
    /// Duplicates: a perceptual hash of every photo's thumbnail, made in the background,
    /// for near-duplicates and series (exact copies need only the integrity hashes).
    pub dupes: bool,
    /// Send to Immich: the server (as Hyperfocal reaches it) and an API key.
    pub immich_url: Option<String>,
    pub immich_api_key: Option<String>,
    /// Immich's address for links in the browser, when it differs (an internal URL above).
    pub immich_public_url: Option<String>,
    /// Where Immich sees the photo folder, when it reads it as an external library (so
    /// photos it has are found by path instead of being uploaded).
    pub immich_library_path: Option<String>,
    /// Debug mode (`HYPERFOCAL_DEBUG`): detailed telemetry, see `crate::telemetry`.
    pub debug: crate::telemetry::DebugConfig,
}

pub(crate) fn env_str(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub(crate) fn env_bool(key: &str, default: bool) -> bool {
    match env_str(key).as_deref().map(|s| s.to_ascii_lowercase()) {
        Some(v) if ["1", "true", "yes", "on"].contains(&v.as_str()) => true,
        Some(v) if ["0", "false", "no", "off"].contains(&v.as_str()) => false,
        _ => default,
    }
}

pub(crate) fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
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
            pregen_previews: false,
            pregen_days: 14,
            raw_cache_mb: 16,
            preview_cache_mb: 16,
            index_threads: 2,
            decode_concurrency: 1,
            password: None,
            exiftool: "exiftool".into(),
            rename_sidecars: true,
            raw_decoder_libraw: false,
            integrity: false,
            cull: false,
            verify_days: 90,
            verify_hour: 4,
            verify_minutes: 120,
            hash_mbps: 0,
            alert_url: None,
            alert_format: "auto".into(),
            alert_token: None,
            ml: crate::ml::MlConfig::off(&root.join("config")),
            ml_size: 2048,
            ml_concurrency: 2,
            people_xmp: true,
            dupes: false,
            immich_url: None,
            immich_api_key: None,
            immich_public_url: None,
            immich_library_path: None,
            debug: crate::telemetry::DebugConfig::off(&root.join("config")),
        }
    }

    pub fn from_env() -> Config {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        let config_dir = PathBuf::from(env_str("HYPERFOCAL_CONFIG").unwrap_or_else(|| "/config".into()));
        let cache_dir = env_str("HYPERFOCAL_CACHE").map(PathBuf::from).unwrap_or_else(|| config_dir.join("cache"));
        let ml = crate::ml::MlConfig::from_env(&config_dir);
        let debug = crate::telemetry::DebugConfig::from_env(&config_dir);
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
            pregen_previews: env_bool("HYPERFOCAL_PREGEN_PREVIEWS", true),
            pregen_days: env_num("HYPERFOCAL_PREGEN_DAYS", 14u32).clamp(1, 365),
            raw_cache_mb: env_num("HYPERFOCAL_RAW_CACHE_MB", 1536),
            preview_cache_mb: env_num("HYPERFOCAL_PREVIEW_CACHE_MB", 4096),
            index_threads: env_num("HYPERFOCAL_INDEX_THREADS", cores.clamp(2, 8)),
            decode_concurrency: env_num("HYPERFOCAL_DECODE_CONCURRENCY", (cores / 4).clamp(1, 4)),
            password: env_str("HYPERFOCAL_PASSWORD"),
            exiftool: env_str("HYPERFOCAL_EXIFTOOL").unwrap_or_else(|| "exiftool".into()),
            rename_sidecars: env_bool("HYPERFOCAL_RENAME_SIDECARS", true),
            raw_decoder_libraw: env_str("HYPERFOCAL_RAW_DECODER").is_some_and(|v| v.eq_ignore_ascii_case("libraw")),
            integrity: env_bool("HYPERFOCAL_INTEGRITY", true),
            cull: env_bool("HYPERFOCAL_CULL", true),
            verify_days: env_num("HYPERFOCAL_VERIFY_DAYS", 90u32).clamp(1, 3650),
            verify_hour: env_num("HYPERFOCAL_VERIFY_HOUR", 4u32).min(23),
            verify_minutes: env_num("HYPERFOCAL_VERIFY_MINUTES", 120u32).clamp(1, 1440),
            hash_mbps: env_num("HYPERFOCAL_HASH_MBPS", 60u32),
            alert_url: env_str("HYPERFOCAL_ALERT_URL"),
            alert_format: env_str("HYPERFOCAL_ALERT_FORMAT").unwrap_or_else(|| "auto".into()),
            alert_token: env_str("HYPERFOCAL_ALERT_TOKEN"),
            ml,
            ml_size: env_num("HYPERFOCAL_ML_SIZE", 2048usize).clamp(640, 4096),
            ml_concurrency: env_num("HYPERFOCAL_ML_CONCURRENCY", 2usize).clamp(1, 16),
            people_xmp: env_bool("HYPERFOCAL_PEOPLE_XMP", true),
            dupes: env_bool("HYPERFOCAL_DUPES", true),
            immich_url: env_str("HYPERFOCAL_IMMICH_URL").map(|u| u.trim_end_matches('/').to_string()),
            immich_api_key: env_str("HYPERFOCAL_IMMICH_API_KEY"),
            immich_public_url: env_str("HYPERFOCAL_IMMICH_PUBLIC_URL").map(|u| u.trim_end_matches('/').to_string()),
            immich_library_path: env_str("HYPERFOCAL_IMMICH_LIBRARY_PATH").map(|p| p.trim_end_matches('/').to_string()).filter(|p| !p.is_empty()),
            debug,
        }
    }
}
