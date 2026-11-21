//! Persistence of saved connections (`connections.json` in the platform
//! config directory, readable only by the current user).

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use dbclaw_core::model::ConnectionConfig;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
struct StoreFile {
    connections: Vec<ConnectionConfig>,
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DBCLAW_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("DB Claw")
}

fn path() -> PathBuf {
    config_dir().join("connections.json")
}

pub fn load_connections() -> Vec<ConnectionConfig> {
    let Ok(text) = std::fs::read_to_string(path()) else {
        return Vec::new();
    };
    match serde_json::from_str::<StoreFile>(&text) {
        Ok(f) => f.connections,
        Err(e) => {
            eprintln!("db-claw: could not read {}: {e}", path().display());
            Vec::new()
        }
    }
}

pub fn save_connections(connections: &[ConnectionConfig]) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let text = serde_json::to_string_pretty(&StoreFile { connections: connections.to_vec() })?;
    let target = path();
    let tmp = target.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &target)?;
    Ok(())
}
