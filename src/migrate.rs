//! One-time move of user data from the browser's former name, `cosmic-web`.
//!
//! The rename changed both the XDG directory name and the app id that
//! cosmic-config files settings under. Without this, an existing user would
//! start with no bookmarks, history, cookies, user scripts or settings.

use std::path::{Path, PathBuf};

/// Directory name used under the XDG data, config and cache directories.
const DIR_NAME: &str = "astrolabe";
const OLD_DIR_NAME: &str = "cosmic-web";
const OLD_APP_ID: &str = "io.github.snaggen.CosmicWeb";

/// What happened to one directory during migration.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Moved,
    NothingToMove,
    /// The new directory already exists; the old one is left alone so data is
    /// never merged or overwritten.
    TargetExists,
    Failed,
}

/// Moves each directory left by `cosmic-web` to its new name, if not done yet.
///
/// Must run before anything reads or creates the new directories.
pub fn migrate_from_cosmic_web() {
    for (old, new) in legacy_dirs() {
        if move_dir(&old, &new) == Outcome::Moved {
            tracing::info!("Migrated {} to {}", old.display(), new.display());
        }
    }
}

/// Old and new location of every directory the browser keeps data in.
fn legacy_dirs() -> Vec<(PathBuf, PathBuf)> {
    let xdg_bases = [dirs::data_dir(), dirs::config_dir(), dirs::cache_dir()];
    let named = xdg_bases
        .into_iter()
        .flatten()
        .map(|base| (base.join(OLD_DIR_NAME), base.join(DIR_NAME)));

    // cosmic-config keeps settings and state under `cosmic/<app id>`.
    let cosmic_bases = [dirs::config_dir(), dirs::state_dir()];
    let by_app_id = cosmic_bases.into_iter().flatten().map(|base| {
        let cosmic = base.join("cosmic");
        (cosmic.join(OLD_APP_ID), cosmic.join(crate::app::APP_ID))
    });

    named.chain(by_app_id).collect()
}

/// Renames `old` to `new` unless `old` is missing or `new` already exists.
fn move_dir(old: &Path, new: &Path) -> Outcome {
    if !old.is_dir() {
        return Outcome::NothingToMove;
    }
    if new.exists() {
        return Outcome::TargetExists;
    }
    match std::fs::rename(old, new) {
        Ok(()) => Outcome::Moved,
        Err(e) => {
            tracing::warn!("Could not move {} to {}: {e}", old.display(), new.display());
            Outcome::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh, empty directory for one test.
    fn scratch_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("astrolabe-migrate-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test scratch directory must be creatable");
        dir
    }

    #[test]
    fn moves_old_directory_with_its_contents() {
        let base = scratch_dir();
        let (old, new) = (base.join("old"), base.join("new"));
        std::fs::create_dir(&old).expect("create old dir");
        std::fs::write(old.join("browser.db"), "data").expect("write file");

        assert_eq!(move_dir(&old, &new), Outcome::Moved);
        assert!(!old.exists());
        assert_eq!(
            std::fs::read_to_string(new.join("browser.db")).expect("read moved file"),
            "data"
        );
    }

    #[test]
    fn does_nothing_without_old_directory() {
        let base = scratch_dir();
        let new = base.join("new");

        assert_eq!(move_dir(&base.join("old"), &new), Outcome::NothingToMove);
        assert!(!new.exists());
    }

    #[test]
    fn leaves_both_alone_when_new_directory_exists() {
        let base = scratch_dir();
        let (old, new) = (base.join("old"), base.join("new"));
        std::fs::create_dir(&old).expect("create old dir");
        std::fs::write(old.join("browser.db"), "old").expect("write old file");
        std::fs::create_dir(&new).expect("create new dir");

        assert_eq!(move_dir(&old, &new), Outcome::TargetExists);
        assert!(old.join("browser.db").exists());
        assert!(!new.join("browser.db").exists());
    }

    #[test]
    fn ignores_a_file_with_the_old_name() {
        let base = scratch_dir();
        let old = base.join("old");
        std::fs::write(&old, "not a directory").expect("write file");

        assert_eq!(move_dir(&old, &base.join("new")), Outcome::NothingToMove);
    }

    #[test]
    fn legacy_dirs_pair_old_and_new_names() {
        for (old, new) in legacy_dirs() {
            assert_eq!(old.parent(), new.parent());
            let old_name = old.file_name().and_then(|n| n.to_str());
            assert!(matches!(old_name, Some(OLD_DIR_NAME | OLD_APP_ID)));
        }
    }
}
