use std::{env, fmt, fs, net::SocketAddr, path::PathBuf, str::FromStr};

use clap::{Parser, ValueEnum};
use faultkeep_domain::BoundedDuration;
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_SHUTDOWN_GRACE_MILLIS: u64 = 5 * 60 * 1_000;

pub type ShutdownGrace = BoundedDuration<MAX_SHUTDOWN_GRACE_MILLIS>;

#[derive(Debug, Clone, Parser)]
#[command(name = "faultkeep", version, about = "Faultkeep all-in-one server")]
pub struct Cli {
    /// TOML configuration file.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Deployment role. Version one accepts only `all`.
    #[arg(long, value_enum)]
    pub role: Option<Role>,
    /// Validate configuration and required secret references, then exit.
    #[arg(long, conflicts_with = "print_effective_config")]
    pub check_config: bool,
    /// Print validated effective configuration with secrets redacted, then exit.
    #[arg(long, conflicts_with = "check_config")]
    pub print_effective_config: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    All,
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => formatter.write_str("all"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub role: Role,
    pub server: ServerConfig,
    pub mongodb: MongoConfig,
    pub development: DevelopmentConfig,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub http_address: SocketAddr,
    pub shutdown_grace: ShutdownGrace,
}

#[derive(Debug, Clone)]
pub struct MongoConfig {
    pub uri: Option<SecretReference>,
}

#[derive(Debug, Clone, Copy)]
pub struct DevelopmentConfig {
    pub allow_literal_secrets: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SecretReference {
    Environment(EnvironmentReference),
    File(FileReference),
    Literal(LiteralReference),
}

impl SecretReference {
    fn validate(&self, allow_literal: bool) -> Result<(), ConfigError> {
        match self {
            Self::Environment(reference) => {
                let valid = !reference.env.is_empty()
                    && reference.env.len() <= 128
                    && reference
                        .env
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
                if !valid {
                    return Err(ConfigError::InvalidSecretReference);
                }
            }
            Self::File(reference) => {
                if reference.file.as_os_str().is_empty() {
                    return Err(ConfigError::InvalidSecretReference);
                }
            }
            Self::Literal(reference) => {
                if !allow_literal {
                    return Err(ConfigError::LiteralSecretForbidden);
                }
                validate_secret_bytes(reference.literal.as_bytes())?;
            }
        }
        Ok(())
    }

    pub fn resolve(&self) -> Result<SecretValue, ConfigError> {
        let bytes = match self {
            Self::Environment(reference) => env::var_os(&reference.env)
                .ok_or_else(|| ConfigError::MissingSecretEnvironment(reference.env.clone()))?
                .to_string_lossy()
                .into_owned()
                .into_bytes(),
            Self::File(reference) => {
                fs::read(&reference.file).map_err(|source| ConfigError::ReadSecretFile {
                    path: reference.file.clone(),
                    source,
                })?
            }
            Self::Literal(reference) => reference.literal.as_bytes().to_vec(),
        };
        validate_secret_bytes(&bytes)?;
        let value = String::from_utf8(bytes).map_err(|_| ConfigError::SecretNotUtf8)?;
        let value = value.trim_end_matches(['\r', '\n']).to_owned();
        if value.is_empty() {
            return Err(ConfigError::EmptySecret);
        }
        Ok(SecretValue(value.into_boxed_str()))
    }

    fn redacted_origin(&self) -> &'static str {
        match self {
            Self::Environment(_) => "<redacted:env>",
            Self::File(_) => "<redacted:file>",
            Self::Literal(_) => "<redacted:literal>",
        }
    }
}

impl fmt::Debug for SecretReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.redacted_origin())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentReference {
    env: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReference {
    file: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiteralReference {
    literal: String,
}

pub struct SecretValue(Box<str>);

impl SecretValue {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    role: Role,
    server: RawServerConfig,
    mongodb: RawMongoConfig,
    development: RawDevelopmentConfig,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            role: Role::All,
            server: RawServerConfig::default(),
            mongodb: RawMongoConfig::default(),
            development: RawDevelopmentConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawServerConfig {
    http_address: String,
    shutdown_grace: String,
}

impl Default for RawServerConfig {
    fn default() -> Self {
        Self {
            http_address: "127.0.0.1:3000".to_owned(),
            shutdown_grace: "10s".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawMongoConfig {
    uri: Option<SecretReference>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawDevelopmentConfig {
    allow_literal_secrets: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration could not be loaded")]
    Load(#[source] Box<figment::Error>),
    #[error("server.http_address is invalid")]
    InvalidHttpAddress,
    #[error("configuration file does not exist or is not a regular file")]
    MissingConfigFile,
    #[error("server.shutdown_grace is invalid or exceeds five minutes")]
    InvalidShutdownGrace,
    #[error("secret reference is invalid")]
    InvalidSecretReference,
    #[error("literal secrets require development.allow_literal_secrets=true")]
    LiteralSecretForbidden,
    #[error("secret is empty")]
    EmptySecret,
    #[error("secret exceeds 65536 bytes")]
    SecretTooLarge,
    #[error("secret is not valid UTF-8")]
    SecretNotUtf8,
    #[error("required secret environment variable is absent: {0}")]
    MissingSecretEnvironment(String),
    #[error("secret file cannot be read")]
    ReadSecretFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl From<figment::Error> for ConfigError {
    fn from(error: figment::Error) -> Self {
        Self::Load(Box::new(error))
    }
}

pub fn load(cli: &Cli) -> Result<AppConfig, ConfigError> {
    let mut figment = Figment::from(Serialized::defaults(RawConfig::default()));
    if let Some(path) = &cli.config {
        if !path.is_file() {
            return Err(ConfigError::MissingConfigFile);
        }
        figment = figment.merge(Toml::file(path));
    }
    figment = figment.merge(Env::prefixed("APP__").split("__"));
    let mut raw = figment.extract::<RawConfig>()?;
    if let Some(role) = cli.role {
        raw.role = role;
    }
    AppConfig::try_from(raw)
}

impl TryFrom<RawConfig> for AppConfig {
    type Error = ConfigError;

    fn try_from(raw: RawConfig) -> Result<Self, Self::Error> {
        let http_address = SocketAddr::from_str(&raw.server.http_address)
            .map_err(|_| ConfigError::InvalidHttpAddress)?;
        let shutdown_grace = ShutdownGrace::from_str(&raw.server.shutdown_grace)
            .map_err(|_| ConfigError::InvalidShutdownGrace)?;
        if let Some(reference) = &raw.mongodb.uri {
            reference.validate(raw.development.allow_literal_secrets)?;
        }
        Ok(Self {
            role: raw.role,
            server: ServerConfig {
                http_address,
                shutdown_grace,
            },
            mongodb: MongoConfig {
                uri: raw.mongodb.uri,
            },
            development: DevelopmentConfig {
                allow_literal_secrets: raw.development.allow_literal_secrets,
            },
        })
    }
}

impl AppConfig {
    pub fn validate_secrets(&self) -> Result<Option<SecretValue>, ConfigError> {
        self.mongodb
            .uri
            .as_ref()
            .map(SecretReference::resolve)
            .transpose()
    }

    #[must_use]
    pub fn effective_redacted(&self) -> String {
        let uri = self
            .mongodb
            .uri
            .as_ref()
            .map_or("<not-configured>", SecretReference::redacted_origin);
        format!(
            "role = \"{}\"\n\n[server]\nhttp_address = \"{}\"\nshutdown_grace = \"{}\"\n\n[mongodb]\nuri = \"{}\"\n\n[development]\nallow_literal_secrets = {}\n",
            self.role,
            self.server.http_address,
            humantime::format_duration(self.server.shutdown_grace.get()),
            uri,
            self.development.allow_literal_secrets,
        )
    }

    #[must_use]
    pub fn has_literal_secret_warning(&self) -> bool {
        matches!(self.mongodb.uri, Some(SecretReference::Literal(_)))
    }
}

fn validate_secret_bytes(bytes: &[u8]) -> Result<(), ConfigError> {
    if bytes.is_empty() {
        return Err(ConfigError::EmptySecret);
    }
    if bytes.len() > MAX_SECRET_BYTES {
        return Err(ConfigError::SecretTooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bounded_and_role_is_all() {
        let cli = Cli::parse_from(["faultkeep", "--check-config"]);
        let config = load(&cli).unwrap();
        assert_eq!(config.role, Role::All);
        assert_eq!(config.server.shutdown_grace.get().as_secs(), 10);
    }

    #[test]
    fn redacted_view_never_uses_secret_debug_or_display() {
        let raw = RawConfig {
            mongodb: RawMongoConfig {
                uri: Some(SecretReference::Literal(LiteralReference {
                    literal: "do-not-print-this".to_owned(),
                })),
            },
            development: RawDevelopmentConfig {
                allow_literal_secrets: true,
            },
            ..RawConfig::default()
        };
        let config = AppConfig::try_from(raw).unwrap();
        let output = config.effective_redacted();
        assert!(!output.contains("do-not-print-this"));
        assert!(output.contains("<redacted:literal>"));
    }

    #[test]
    fn production_literal_secret_is_rejected() {
        let raw = RawConfig {
            mongodb: RawMongoConfig {
                uri: Some(SecretReference::Literal(LiteralReference {
                    literal: "secret".to_owned(),
                })),
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(raw),
            Err(ConfigError::LiteralSecretForbidden)
        ));
    }

    #[test]
    fn secret_value_debug_is_always_redacted() {
        let value = SecretValue("sensitive".into());
        assert_eq!(format!("{value:?}"), "<redacted>");
        assert_eq!(value.expose(), "sensitive");
    }

    #[test]
    fn config_path_is_not_silently_ignored() {
        let cli = Cli::parse_from([
            "faultkeep",
            "--config",
            "definitely-missing-config.toml",
            "--check-config",
        ]);
        assert!(load(&cli).is_err());
    }

    #[test]
    fn secret_file_line_ending_is_removed_without_general_trimming() {
        let path = temporary_path("secret");
        fs::write(&path, " value with spaces \r\n").unwrap();
        let reference = SecretReference::File(FileReference { file: path.clone() });
        let value = reference.resolve().unwrap();
        assert_eq!(value.expose(), " value with spaces ");
        fs::remove_file(path).unwrap();
    }

    fn temporary_path(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "faultkeep-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn config_file_unknown_fields_are_errors() {
        let path = temporary_path("unknown");
        fs::write(&path, "unknown_field = true\n").unwrap();
        let cli = Cli::parse_from([
            "faultkeep",
            "--config",
            path.to_str().unwrap(),
            "--check-config",
        ]);
        assert!(load(&cli).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn path_parameter_remains_a_normal_path() {
        assert_eq!(std::path::Path::new("a.toml").extension().unwrap(), "toml");
    }
}
