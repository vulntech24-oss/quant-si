//! Master-key rotation (ADR 0015): re-encrypt every stored API key and TOTP
//! secret under a new key, crash-safely.
//!
//! 1. Write the new key to `master.key.new` (mode 0600; refuse if present).
//! 2. Re-encrypt every value in one database transaction.
//! 3. Rename `master.key` to `master.key.previous` and `master.key.new` to
//!    `master.key`.
//!
//! A crash between 2 and 3 leaves the database under the new key and the
//! file still old; [`recover`] at the next start sees which key opens the
//! stored values and finishes (or discards) the rotation. Only a key kept
//! in `data_dir` can be rotated here; a key from `QD_MASTER_KEY` is changed
//! by the operator.

use std::path::{Path, PathBuf};

use qd_store::settings::{MasterKey, PgSecrets};

use crate::config::write_private;

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn read_key(path: &Path) -> Result<MasterKey, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let bytes = hex::decode(text.trim()).map_err(|_| format!("{} is not hex", path.display()))?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| format!("{} is not a 32-byte key", path.display()))?;
    Ok(MasterKey::new(key))
}

fn swap_files(file: &Path) -> Result<(), String> {
    let next = with_suffix(file, ".new");
    std::fs::rename(file, with_suffix(file, ".previous")).map_err(|e| e.to_string())?;
    std::fs::rename(&next, file).map_err(|e| e.to_string())
}

/// Rotates the master key. Returns how many values were re-encrypted.
pub async fn rotate(secrets: &PgSecrets, file: Option<&Path>, actor: &str) -> Result<u64, String> {
    let file = file.ok_or(
        "the master key comes from QD_MASTER_KEY: rotate it on the server (see the operator guide)",
    )?;
    let next = with_suffix(file, ".new");
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    write_private(&next, hex::encode(bytes).as_bytes()).map_err(|e| {
        format!("{e} (a file master.key.new means an unfinished rotation; restart the server to finish it)")
    })?;
    match secrets.rotate(MasterKey::new(bytes), actor).await {
        Ok(count) => {
            swap_files(file)?;
            Ok(count)
        }
        Err(e) => {
            // Nothing was re-encrypted: the old key stays valid.
            let _ = std::fs::remove_file(&next);
            Err(e.0)
        }
    }
}

/// Finishes or discards a rotation interrupted by a crash. Returns the key
/// to use when it changed.
pub async fn recover(
    secrets: &PgSecrets,
    file: Option<&Path>,
) -> Result<Option<MasterKey>, String> {
    let Some(file) = file else { return Ok(None) };
    let next = with_suffix(file, ".new");
    if !next.exists() {
        return Ok(None);
    }
    let new_key = read_key(&next)?;
    let current = read_key(file)?;
    let new_opens = secrets.key_opens_all(&new_key).await.map_err(|e| e.0)?;
    let old_opens = secrets.key_opens_all(&current).await.map_err(|e| e.0)?;
    if new_opens && !old_opens {
        swap_files(file)?;
        tracing::warn!("finished an interrupted master-key rotation");
        Ok(Some(new_key))
    } else if old_opens {
        std::fs::remove_file(&next).map_err(|e| e.to_string())?;
        tracing::warn!("discarded an unfinished master-key rotation (nothing was re-encrypted)");
        Ok(None)
    } else {
        Err("neither master.key nor master.key.new opens the stored secrets; restore the key from backup".to_owned())
    }
}
