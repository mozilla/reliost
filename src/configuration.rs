use std::{path::PathBuf, time::Duration};

use config::ConfigError;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Settings {
    pub server: ServerSettings,
    pub symbols: Option<SymbolSettings>,
    pub proguard: Option<ProguardSettings>,
    pub quota: Option<QuotaSettings>,
    pub self_profiles: Option<SelfProfilesSettings>,
}

#[derive(Deserialize)]
pub struct SelfProfilesSettings {
    pub dir: PathBuf,
}

#[derive(Deserialize)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    pub workers: Option<usize>,
}

#[derive(Deserialize)]
pub struct SymbolSettings {
    pub breakpad: Option<BreakpadSymbolSettings>,
    pub windows: Option<WindowsSymbolSettings>,
}

#[derive(Deserialize)]
pub struct BreakpadSymbolSettings {
    #[serde(default)]
    pub servers: Vec<String>,

    pub cache_dir: PathBuf,
    pub symindex_dir: Option<PathBuf>,

    #[serde(default)]
    #[serde(with = "humantime_serde")]
    pub negative_cache_ttl: Option<Duration>,
}

#[derive(Deserialize)]
pub struct WindowsSymbolSettings {
    #[serde(default)]
    pub servers: Vec<String>,

    pub cache_dir: PathBuf,
}

/// Settings for Android R8/ProGuard mapping files, used by /deobfuscate/java/v1.
#[derive(Deserialize)]
pub struct ProguardSettings {
    /// Base URLs from which mapping files are downloaded, tried in order.
    /// A mapping file is requested from
    /// `<server>/<proguard uuid>/mapping.txt.zst`, with the UUID written in
    /// lowercase with dashes. The file must be zstd-compressed.
    #[serde(default)]
    pub servers: Vec<String>,

    /// The directory in which the converted mapping files are cached.
    pub cache_dir: PathBuf,
}

/// Settings for automatic file deletion
#[derive(Debug, Clone, Deserialize)]
pub struct QuotaSettings {
    /// The root of the managed directory tree.
    pub managed_dir: PathBuf,
    /// The .db file in which the list of files should be stored. Must be outside of `managed_dir`.
    pub db_path: PathBuf,
    /// The maximum size of the managed directory, as a string that's
    /// parsed by the [parse-size crate](https://crates.io/crates/parse-size).
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_bytes")]
    pub size_limit: Option<u64>,
    /// The maximum age of each file in the managed directory, as a string
    /// that's parsed by [`humantime::parse_duration`](https://docs.rs/humantime/latest/humantime/fn.parse_duration.html).
    #[serde(default)]
    #[serde(with = "humantime_serde")]
    pub age_limit: Option<Duration>,
}

fn deserialize_bytes<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let maybe_string = Option::<String>::deserialize(deserializer)?;
    let maybe_bytes = match maybe_string {
        Some(s) => {
            let bytes = parse_size::parse_size(s).map_err(serde::de::Error::custom)?;
            Some(bytes)
        }
        None => None,
    };
    Ok(maybe_bytes)
}

/// The possible runtime environment for our application.
pub enum Environment {
    Local,
    Production,
}

impl Environment {
    pub fn as_str(&self) -> &'static str {
        match self {
            Environment::Local => "local",
            Environment::Production => "production",
        }
    }
}

impl TryFrom<String> for Environment {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.to_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "production" => Ok(Self::Production),
            other => Err(format!(
                "{} is not a supported environment. Use either `local` or `production`.",
                other
            )),
        }
    }
}

pub fn get_configuration() -> Result<Settings, ConfigError> {
    let base_path = std::env::current_dir().expect("Failed to determine the current directory");
    let configuration_directory = base_path.join("configuration");

    // Detect the running environment.
    // Default to `local` if unspecified.
    let environment: Environment = std::env::var("APP_ENVIRONMENT")
        .unwrap_or_else(|_| "local".into())
        .try_into()
        .expect("Failed to parse APP_ENVIRONMENT.");
    let environment_filename = format!("{}.toml", environment.as_str());

    let settings = config::Config::builder()
        .add_source(config::File::from(
            configuration_directory.join("base.toml"),
        ))
        .add_source(config::File::from(
            configuration_directory.join(environment_filename),
        ))
        // Add in settings from environment variables (with a prefix of RELIOST
        // and '_' as separator)
        // E.g. `RELIOST_SERVER_PORT=5001 would set `Settings.server.port`.
        // Note that env vars take precedence over other config files.
        .add_source(
            config::Environment::with_prefix("RELIOST")
                .prefix_separator("_")
                .separator("_"),
        )
        .build()?;
    settings.try_deserialize::<Settings>()
}
