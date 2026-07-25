//! Resolves the data directory that holds the two SQLite database files
//! (`gamedata.db`, read-only static game content shipped with the release,
//! and `world.db`, live account/character/inventory state). No config file
//! required at all: defaults to `./data` relative to the process's working
//! directory (matching this project's existing relative-path convention for
//! `logs/`/`runtime/`/`data/maps/`), overridable with the `RAPTURE_DATA_DIR`
//! env var for anyone who wants the files somewhere else.

use std::path::PathBuf;

const DEFAULT_DATA_DIR: &str = "data";
const DATA_DIR_ENV_VAR: &str = "RAPTURE_DATA_DIR";

pub fn data_dir() -> PathBuf {
    std::env::var(DATA_DIR_ENV_VAR)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_DATA_DIR))
}

pub fn gamedata_db_path() -> PathBuf {
    data_dir().join("gamedata.db")
}

pub fn world_db_path() -> PathBuf {
    data_dir().join("world.db")
}
