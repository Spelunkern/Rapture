//! Reads `config.ini` (relative to the process's working directory,
//! matching this project's existing relative-path convention for
//! data/logs/runtime). A missing file, section, or key is not an error:
//! every field falls back to the existing RAPTURE_SQLSERVER_* env var (see
//! db.rs), then a hardcoded default -- nothing breaks for a machine that
//! has no config.ini at all, and Windows Integrated Authentication remains
//! the default when no credentials are configured either way.

use ini::Ini;

/// Placeholder values shipped in the distributed `config.ini` template --
/// treated as "not configured" rather than attempted as a real login, so
/// the template can safely ship with these in place.
const PLACEHOLDER_USER: &str = "your_user_here";
const PLACEHOLDER_PASSWORD: &str = "your_password_here";

const CONFIG_FILE: &str = "config.ini";

#[derive(Clone, Debug)]
pub struct SqlServerCredentials {
    pub user: String,
    pub password: String,
}

#[derive(Clone, Debug, Default)]
pub struct SqlServerConfig {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub pool_size: Option<usize>,
    /// `None` when unset or still the shipped placeholders -- callers
    /// should fall back to Windows Integrated Authentication
    /// (`AuthMethod::Integrated`) in that case.
    pub credentials: Option<SqlServerCredentials>,
}

/// Reads the `[SqlServer]` section of `config.ini`, if the file exists.
/// Every field is independently optional: a value present in the file
/// takes priority, otherwise the caller is expected to fall back to its
/// own env-var-then-default handling (see db.rs's `Pool::new`).
pub fn load_sql_server_config() -> SqlServerConfig {
    let Ok(ini) = Ini::load_from_file(CONFIG_FILE) else {
        return SqlServerConfig::default();
    };
    let Some(section) = ini.section(Some("SqlServer")) else {
        return SqlServerConfig::default();
    };

    let host = section
        .get("Host")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let port = section.get("Port").and_then(|value| value.trim().parse().ok());
    let pool_size = section
        .get("PoolSize")
        .and_then(|value| value.trim().parse().ok());

    let user = section.get("User").map(str::trim).unwrap_or_default();
    let password = section.get("Password").map(str::trim).unwrap_or_default();
    let credentials = if user.is_empty()
        || password.is_empty()
        || user == PLACEHOLDER_USER
        || password == PLACEHOLDER_PASSWORD
    {
        None
    } else {
        Some(SqlServerCredentials {
            user: user.to_string(),
            password: password.to_string(),
        })
    };

    SqlServerConfig {
        host,
        port,
        pool_size,
        credentials,
    }
}
