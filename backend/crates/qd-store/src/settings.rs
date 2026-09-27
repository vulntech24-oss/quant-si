//! Settings versions and encrypted secrets (ADR 0013).

use std::sync::Arc;

use async_trait::async_trait;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use qd_app::ports::{
    AuditLog, SecretReader, SecretStatus, SecretStore, SecretValue, SettingVersion, SettingsStore,
    StoreError,
};
use qd_app::secrets::{check_value, spec};
use sqlx::{PgPool, Row};

fn store_error(e: impl std::fmt::Display) -> StoreError {
    StoreError(e.to_string())
}

/// Settings versions in PostgreSQL.
#[derive(Clone, Debug)]
pub struct PgSettings {
    pool: PgPool,
}

impl PgSettings {
    /// Creates the store.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SettingsStore for PgSettings {
    async fn latest(&self) -> Result<Vec<SettingVersion>, StoreError> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (section) section, version, value, updated_by, updated_at \
             FROM settings_versions ORDER BY section, version DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.iter()
            .map(|r| {
                Ok(SettingVersion {
                    section: r.try_get("section").map_err(store_error)?,
                    value: r.try_get("value").map_err(store_error)?,
                    version: r.try_get("version").map_err(store_error)?,
                    updated_by: r.try_get("updated_by").map_err(store_error)?,
                    updated_at: r.try_get("updated_at").map_err(store_error)?,
                })
            })
            .collect()
    }

    async fn history(&self, section: &str) -> Result<Vec<SettingVersion>, StoreError> {
        let rows = sqlx::query(
            "SELECT section, version, value, updated_by, updated_at FROM settings_versions \
             WHERE section = $1 ORDER BY version DESC",
        )
        .bind(section)
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.iter()
            .map(|r| {
                Ok(SettingVersion {
                    section: r.try_get("section").map_err(store_error)?,
                    value: r.try_get("value").map_err(store_error)?,
                    version: r.try_get("version").map_err(store_error)?,
                    updated_by: r.try_get("updated_by").map_err(store_error)?,
                    updated_at: r.try_get("updated_at").map_err(store_error)?,
                })
            })
            .collect()
    }

    async fn put(
        &self,
        section: &str,
        value: Option<&serde_json::Value>,
        actor: &str,
    ) -> Result<i64, StoreError> {
        // The primary key makes a concurrent save of the same version fail
        // rather than silently overwrite.
        sqlx::query_scalar(
            "INSERT INTO settings_versions (section, version, value, updated_by) \
             VALUES ($1, COALESCE((SELECT max(version) FROM settings_versions WHERE section = $1), 0) + 1, $2, $3) \
             RETURNING version",
        )
        .bind(section)
        .bind(value)
        .bind(actor)
        .fetch_one(&self.pool)
        .await
        .map_err(store_error)
    }
}

/// A 256-bit master key. `Debug` is redacted.
#[derive(Clone)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    /// Wraps key bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(***)")
    }
}

/// Encrypted secrets in PostgreSQL. Each value is sealed with
/// XChaCha20-Poly1305 under the master key, with a random 24-byte nonce and
/// the secret's name as associated data, so a ciphertext cannot be moved to
/// another name. Without a master key nothing can be stored or read.
pub struct PgSecrets {
    pool: PgPool,
    key: std::sync::RwLock<Option<MasterKey>>,
    audit: Arc<dyn AuditLog>,
}

impl std::fmt::Debug for PgSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgSecrets")
            .field("available", &self.available_key())
            .finish_non_exhaustive()
    }
}

const NONCE_LEN: usize = 24;

impl PgSecrets {
    /// Creates the store.
    #[must_use]
    pub fn new(pool: PgPool, key: Option<MasterKey>, audit: Arc<dyn AuditLog>) -> Self {
        Self {
            pool,
            key: std::sync::RwLock::new(key),
            audit,
        }
    }

    fn available_key(&self) -> bool {
        self.key
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    fn cipher(&self) -> Result<XChaCha20Poly1305, StoreError> {
        let guard = self
            .key
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = guard.as_ref().ok_or_else(|| {
            StoreError("no master key is configured, so secrets cannot be stored".to_owned())
        })?;
        Ok(XChaCha20Poly1305::new((&key.0).into()))
    }

    fn seal(&self, name: &str, value: &str) -> Result<Vec<u8>, StoreError> {
        seal_with(&self.cipher()?, name, value)
    }

    fn open(&self, name: &str, blob: &[u8]) -> Result<String, StoreError> {
        open_with(&self.cipher()?, name, blob)
    }

    /// Every encrypted value as (table, key column value, associated data, blob).
    async fn sealed_rows(
        &self,
    ) -> Result<Vec<(&'static str, String, String, Vec<u8>)>, StoreError> {
        let mut rows = Vec::new();
        for r in sqlx::query("SELECT name, ciphertext FROM secrets")
            .fetch_all(&self.pool)
            .await
            .map_err(store_error)?
        {
            let name: String = r.try_get("name").map_err(store_error)?;
            rows.push((
                "secrets",
                name.clone(),
                name,
                r.try_get("ciphertext").map_err(store_error)?,
            ));
        }
        for r in sqlx::query("SELECT user_id, ciphertext FROM user_totp")
            .fetch_all(&self.pool)
            .await
            .map_err(store_error)?
        {
            let user: uuid::Uuid = r.try_get("user_id").map_err(store_error)?;
            let user = qd_domain::ids::UserId::from_uuid(user);
            rows.push((
                "user_totp",
                user.to_string(),
                totp_aad(user),
                r.try_get("ciphertext").map_err(store_error)?,
            ));
        }
        Ok(rows)
    }

    /// Switches to another key (after a recovered rotation).
    pub fn use_key(&self, key: MasterKey) {
        *self
            .key
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(key);
    }

    /// Whether `key` opens every stored value (true when nothing is stored).
    pub async fn key_opens_all(&self, key: &MasterKey) -> Result<bool, StoreError> {
        let cipher = XChaCha20Poly1305::new((&key.0).into());
        Ok(self
            .sealed_rows()
            .await?
            .iter()
            .all(|(_, _, aad, blob)| open_with(&cipher, aad, blob).is_ok()))
    }

    /// Re-encrypts every stored value (API keys and TOTP secrets) under
    /// `new_key` in one transaction, then uses it. If anything fails, the
    /// transaction rolls back and the old key stays in use. Returns how
    /// many values were re-encrypted. The caller persists the new key
    /// first (ADR 0015).
    pub async fn rotate(&self, new_key: MasterKey, actor: &str) -> Result<u64, StoreError> {
        let old = self.cipher()?;
        let new = XChaCha20Poly1305::new((&new_key.0).into());
        let rows = self.sealed_rows().await?;
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        for (table, id, aad, blob) in &rows {
            let plain = open_with(&old, aad, blob)?;
            let sealed = seal_with(&new, aad, &plain)?;
            let sql = if *table == "secrets" {
                "UPDATE secrets SET ciphertext = $2 WHERE name = $1"
            } else {
                "UPDATE user_totp SET ciphertext = $2 WHERE user_id::text = $1"
            };
            sqlx::query(sql)
                .bind(id)
                .bind(sealed)
                .execute(&mut *tx)
                .await
                .map_err(store_error)?;
        }
        sqlx::query(
            "INSERT INTO audit_log (actor, action, detail) VALUES ($1, 'secrets.key_rotated', $2)",
        )
        .bind(actor)
        .bind(serde_json::json!({ "values": rows.len() }))
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
        tx.commit().await.map_err(store_error)?;
        *self
            .key
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(new_key);
        Ok(u64::try_from(rows.len()).unwrap_or(u64::MAX))
    }
}

fn seal_with(cipher: &XChaCha20Poly1305, name: &str, value: &str) -> Result<Vec<u8>, StoreError> {
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(store_error)?;
    let sealed = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: value.as_bytes(),
                aad: name.as_bytes(),
            },
        )
        .map_err(|_| StoreError("encryption failed".to_owned()))?;
    let mut out = nonce.to_vec();
    out.extend(sealed);
    Ok(out)
}

fn open_with(cipher: &XChaCha20Poly1305, name: &str, blob: &[u8]) -> Result<String, StoreError> {
    if blob.len() <= NONCE_LEN {
        return Err(StoreError("stored secret is corrupt".to_owned()));
    }
    let (nonce, sealed) = blob.split_at(NONCE_LEN);
    let plain = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: sealed,
                aad: name.as_bytes(),
            },
        )
        .map_err(|_| {
            StoreError("secret cannot be decrypted (was the master key changed?)".to_owned())
        })?;
    String::from_utf8(plain).map_err(|_| StoreError("stored secret is corrupt".to_owned()))
}

fn known(name: &str) -> Result<(), StoreError> {
    spec(name)
        .map(|_| ())
        .ok_or_else(|| StoreError(format!("unknown secret {name}")))
}

#[async_trait]
impl SecretStore for PgSecrets {
    fn available(&self) -> bool {
        self.available_key()
    }

    async fn status(&self) -> Result<Vec<SecretStatus>, StoreError> {
        let rows = sqlx::query(
            "SELECT name, ciphertext, updated_by, updated_at FROM secrets ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.iter()
            .map(|r| {
                let name: String = r.try_get("name").map_err(store_error)?;
                let blob: Vec<u8> = r.try_get("ciphertext").map_err(store_error)?;
                Ok(SecretStatus {
                    readable: self.open(&name, &blob).is_ok(),
                    name,
                    set: true,
                    updated_at: Some(r.try_get("updated_at").map_err(store_error)?),
                    updated_by: Some(r.try_get("updated_by").map_err(store_error)?),
                })
            })
            .collect()
    }

    async fn set(&self, name: &str, value: &SecretValue, actor: &str) -> Result<(), StoreError> {
        known(name)?;
        check_value(value.expose()).map_err(StoreError)?;
        let blob = self.seal(name, value.expose())?;
        sqlx::query(
            "INSERT INTO secrets (name, ciphertext, updated_by, updated_at) VALUES ($1, $2, $3, now()) \
             ON CONFLICT (name) DO UPDATE SET ciphertext = $2, updated_by = $3, updated_at = now()",
        )
        .bind(name)
        .bind(blob)
        .bind(actor)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        // The name only: the value never reaches the audit log.
        self.audit
            .record(actor, "secret.set", serde_json::json!({ "name": name }))
            .await
    }

    async fn clear(&self, name: &str, actor: &str) -> Result<(), StoreError> {
        known(name)?;
        sqlx::query("DELETE FROM secrets WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        self.audit
            .record(actor, "secret.clear", serde_json::json!({ "name": name }))
            .await
    }
}

#[async_trait]
impl SecretReader for PgSecrets {
    async fn get(&self, name: &str) -> Result<Option<SecretValue>, StoreError> {
        known(name)?;
        let row = sqlx::query("SELECT ciphertext FROM secrets WHERE name = $1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(store_error)?;
        row.map(|r| {
            let blob: Vec<u8> = r.try_get("ciphertext").map_err(store_error)?;
            self.open(name, &blob).map(SecretValue::new)
        })
        .transpose()
    }
}

fn totp_aad(user: qd_domain::ids::UserId) -> String {
    format!("totp:{user}")
}

#[async_trait]
impl qd_app::ports::TotpStore for PgSecrets {
    async fn totp(
        &self,
        user: qd_domain::ids::UserId,
    ) -> Result<Option<qd_app::ports::TotpRecord>, StoreError> {
        let row =
            sqlx::query("SELECT ciphertext, enabled, last_step FROM user_totp WHERE user_id = $1")
                .bind(user.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(store_error)?;
        row.map(|r| {
            let blob: Vec<u8> = r.try_get("ciphertext").map_err(store_error)?;
            Ok(qd_app::ports::TotpRecord {
                secret: SecretValue::new(self.open(&totp_aad(user), &blob)?),
                enabled: r.try_get("enabled").map_err(store_error)?,
                last_step: r.try_get("last_step").map_err(store_error)?,
            })
        })
        .transpose()
    }

    async fn put_pending_totp(
        &self,
        user: qd_domain::ids::UserId,
        secret: &SecretValue,
    ) -> Result<(), StoreError> {
        let blob = self.seal(&totp_aad(user), secret.expose())?;
        sqlx::query(
            "INSERT INTO user_totp (user_id, ciphertext, enabled, last_step) VALUES ($1, $2, false, 0) \
             ON CONFLICT (user_id) DO UPDATE SET ciphertext = $2, enabled = false, last_step = 0, updated_at = now()",
        )
        .bind(user.as_uuid())
        .bind(blob)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn enable_totp(&self, user: qd_domain::ids::UserId) -> Result<(), StoreError> {
        sqlx::query("UPDATE user_totp SET enabled = true, updated_at = now() WHERE user_id = $1")
            .bind(user.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn remove_totp(&self, user: qd_domain::ids::UserId) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM user_totp WHERE user_id = $1")
            .bind(user.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn use_totp_step(
        &self,
        user: qd_domain::ids::UserId,
        step: i64,
    ) -> Result<bool, StoreError> {
        let updated = sqlx::query(
            "UPDATE user_totp SET last_step = $2, updated_at = now() WHERE user_id = $1 AND last_step < $2",
        )
        .bind(user.as_uuid())
        .bind(step)
        .execute(&self.pool)
        .await
        .map_err(store_error)?
        .rows_affected();
        Ok(updated == 1)
    }
}
