//! One-time pairing and independent bearer-token authentication.

use std::fs;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::{Error, Result};

const MAX_CREDENTIAL_BYTES: usize = 512;
const MAX_CLIENT_LABEL_BYTES: usize = 128;
const MAX_CLIENTS: usize = 32;
const PAIRING_LIFETIME_SECONDS: i64 = 10 * 60;
const REVOKED_PAIRING_EXPIRY: i64 = 0;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthState {
    pending_pairing: Option<PendingPairing>,
    clients: Vec<ClientToken>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingPairing {
    digest: [u8; 32],
    expires_at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientToken {
    id: String,
    label: String,
    digest: [u8; 32],
    created_at: i64,
}

/// Identity returned after a successful authentication handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    pub id: String,
    pub label: String,
}

/// A newly issued bearer token. Only the digest is persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedToken {
    pub client_id: String,
    pub token: String,
}

/// One pending code that may be consumed by exactly one new client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingGrant {
    pub code: String,
    pub expires_at: i64,
}

#[cfg(any(unix, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingStatus {
    Pending,
    Consumed,
    Replaced,
}

/// File-backed authentication state shared by all accepted connections.
pub struct AuthStore {
    path: PathBuf,
    state: Mutex<AuthState>,
}

impl AuthStore {
    /// Creates fresh auth state and returns the short-lived bootstrap code.
    pub fn initialize(path: impl Into<PathBuf>) -> Result<(Self, PairingGrant)> {
        let path = path.into();
        let grant = new_pairing_grant()?;
        let state = AuthState {
            pending_pairing: Some(PendingPairing {
                digest: digest(&grant.code),
                expires_at: grant.expires_at,
            }),
            clients: Vec::new(),
        };
        save_private_json(&path, &state, true)?;
        Ok((
            Self {
                path,
                state: Mutex::new(state),
            },
            grant,
        ))
    }

    /// Opens previously initialized authentication state.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let contents = fs::read(&path)?;
        if contents.len() > 64 * 1024 {
            return Err(Error::Config("authentication state is too large".into()));
        }
        let state: AuthState = serde_json::from_slice(&contents)?;
        if state.clients.len() > MAX_CLIENTS {
            return Err(Error::Config(
                "authentication state exceeds the client limit".into(),
            ));
        }
        if state.pending_pairing.is_none() && state.clients.is_empty() {
            return Err(Error::Config(
                "authentication state has neither pairing nor client access".into(),
            ));
        }
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    /// Consumes the pending code and appends an independently issued client token.
    pub fn pair(&self, code: &str, client_label: &str) -> Result<IssuedToken> {
        validate_client_label(client_label)?;
        let now = unix_timestamp()?;
        let mut state = self.lock_state()?;
        let Some(pending) = &state.pending_pairing else {
            return Err(Error::Unauthorized);
        };
        if pending.expires_at < now || !credential_matches(code, &pending.digest) {
            return Err(Error::Unauthorized);
        }
        if state.clients.len() == MAX_CLIENTS {
            return Err(Error::Config("paired client limit reached".into()));
        }

        let token = random_secret(2);
        let client_id = pairing_client_id(code);
        let mut next = state.clone();
        next.pending_pairing = None;
        next.clients.push(ClientToken {
            id: client_id.clone(),
            label: client_label.into(),
            digest: digest(&token),
            created_at: now,
        });
        save_private_json(&self.path, &next, false)?;
        *state = next;
        Ok(IssuedToken { client_id, token })
    }

    /// Replaces any unused pairing code without invalidating paired clients.
    pub fn create_pairing_code(&self) -> Result<PairingGrant> {
        let mut state = self.lock_state()?;
        if state.clients.len() == MAX_CLIENTS {
            return Err(Error::Config("paired client limit reached".into()));
        }
        let grant = new_pairing_grant()?;
        let mut next = state.clone();
        next.pending_pairing = Some(PendingPairing {
            digest: digest(&grant.code),
            expires_at: grant.expires_at,
        });
        save_private_json(&self.path, &next, false)?;
        *state = next;
        Ok(grant)
    }

    /// Verifies a bearer token against every paired client digest.
    pub fn authenticate(&self, token: &str) -> Result<ClientIdentity> {
        if token.is_empty() || token.len() > MAX_CREDENTIAL_BYTES {
            return Err(Error::Unauthorized);
        }
        let candidate = digest(token);
        let state = self.lock_state()?;
        let mut matched = None;
        for client in &state.clients {
            if bool::from(candidate.ct_eq(&client.digest)) {
                matched = Some(ClientIdentity {
                    id: client.id.clone(),
                    label: client.label.clone(),
                });
            }
        }
        matched.ok_or(Error::Unauthorized)
    }

    #[cfg(any(unix, test))]
    pub(crate) fn pairing_status(&self, code: &str) -> Result<PairingStatus> {
        let state = self.lock_state()?;
        if state
            .clients
            .iter()
            .any(|client| client.id == pairing_client_id(code))
        {
            return Ok(PairingStatus::Consumed);
        }
        Ok(match &state.pending_pairing {
            Some(pending) if credential_matches(code, &pending.digest) => PairingStatus::Pending,
            _ => PairingStatus::Replaced,
        })
    }

    #[cfg(any(unix, test))]
    pub(crate) fn revoke_pairing_code(&self, code: &str) -> Result<()> {
        let mut state = self.lock_state()?;
        let Some(pending) = &state.pending_pairing else {
            return Ok(());
        };
        if !credential_matches(code, &pending.digest) {
            return Ok(());
        }
        let mut next = state.clone();
        next.pending_pairing = Some(PendingPairing {
            digest: digest(&random_secret(1)),
            expires_at: REVOKED_PAIRING_EXPIRY,
        });
        save_private_json(&self.path, &next, false)?;
        *state = next;
        Ok(())
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, AuthState>> {
        self.state
            .lock()
            .map_err(|_| Error::Config("authentication state lock is poisoned".into()))
    }
}

fn validate_client_label(label: &str) -> Result<()> {
    if label.trim().is_empty() || label.len() > MAX_CLIENT_LABEL_BYTES {
        return Err(Error::Config(format!(
            "client label must be 1–{MAX_CLIENT_LABEL_BYTES} bytes"
        )));
    }
    Ok(())
}

fn credential_matches(candidate: &str, expected: &[u8; 32]) -> bool {
    if candidate.is_empty() || candidate.len() > MAX_CREDENTIAL_BYTES {
        return false;
    }
    bool::from(digest(candidate).ct_eq(expected))
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn pairing_client_id(code: &str) -> String {
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest(code)[..16]);
    Uuid::from_bytes(bytes).to_string()
}

fn new_pairing_grant() -> Result<PairingGrant> {
    Ok(PairingGrant {
        code: random_secret(1),
        expires_at: unix_timestamp()?
            .checked_add(PAIRING_LIFETIME_SECONDS)
            .ok_or_else(|| Error::Config("pairing expiry overflow".into()))?,
    })
}

fn random_secret(parts: usize) -> String {
    (0..parts)
        .map(|_| Uuid::new_v4().simple().to_string())
        .collect()
}

fn unix_timestamp() -> Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Config("system clock is before the Unix epoch".into()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Error::Config("system clock is unsupported".into()))
}

fn save_private_json(path: &Path, value: &impl Serialize, create_new: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Config("authentication path has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let contents = serde_json::to_vec_pretty(value)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(&contents)?;
    file.as_file().sync_all()?;
    if create_new {
        file.persist_noclobber(path).map_err(|error| error.error)?;
    } else {
        file.persist(path).map_err(|error| error.error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_three_clients_keeps_every_issued_token_valid() {
        let directory = tempfile::tempdir().expect("state directory");
        let path = directory.path().join("auth.json");
        let (auth, first) = AuthStore::initialize(&path).expect("initialize auth");
        let first = auth.pair(&first.code, "Mac").expect("pair Mac");
        let second_code = auth.create_pairing_code().expect("second code");
        let second = auth.pair(&second_code.code, "iPhone").expect("pair iPhone");
        let third_code = auth.create_pairing_code().expect("third code");
        let third = auth.pair(&third_code.code, "CLI").expect("pair CLI");

        assert!(auth.authenticate(&first.token).is_ok());
        assert!(auth.authenticate(&second.token).is_ok());
        assert!(auth.authenticate(&third.token).is_ok());
    }

    #[test]
    fn creating_a_new_pairing_code_invalidates_the_previous_code_only() {
        let directory = tempfile::tempdir().expect("state directory");
        let path = directory.path().join("auth.json");
        let (auth, bootstrap) = AuthStore::initialize(&path).expect("initialize auth");
        let replacement = auth.create_pairing_code().expect("replacement code");

        let error = auth
            .pair(&bootstrap.code, "stale")
            .expect_err("old code must fail");

        assert!(matches!(error, Error::Unauthorized));
        assert!(auth.pair(&replacement.code, "current").is_ok());
        assert_eq!(
            auth.pairing_status(&bootstrap.code)
                .expect("replaced status"),
            PairingStatus::Replaced
        );
    }

    #[test]
    fn pairing_status_tracks_a_durable_client_issuance() {
        let directory = tempfile::tempdir().expect("state directory");
        let path = directory.path().join("auth.json");
        let (auth, grant) = AuthStore::initialize(&path).expect("initialize auth");
        let pending = auth.pairing_status(&grant.code).expect("pending status");
        auth.pair(&grant.code, "iPhone").expect("pair iPhone");
        let reopened = AuthStore::open(path).expect("reopen auth");
        let replacement = reopened.create_pairing_code().expect("replacement code");
        let consumed = reopened
            .pairing_status(&grant.code)
            .expect("consumed status");

        assert_eq!(
            (pending, consumed),
            (PairingStatus::Pending, PairingStatus::Consumed)
        );
        assert_eq!(
            reopened
                .pairing_status(&replacement.code)
                .expect("replacement status"),
            PairingStatus::Pending
        );
    }

    #[test]
    fn revoking_a_pairing_code_does_not_revoke_its_replacement() {
        let directory = tempfile::tempdir().expect("state directory");
        let path = directory.path().join("auth.json");
        let (auth, revoked) = AuthStore::initialize(path).expect("initialize auth");

        auth.revoke_pairing_code(&revoked.code)
            .expect("revoke code");
        assert!(auth.pair(&revoked.code, "stale").is_err());

        let replacement = auth.create_pairing_code().expect("replacement code");
        auth.revoke_pairing_code(&revoked.code)
            .expect("revoke old code");
        assert!(auth.pair(&replacement.code, "current").is_ok());
    }

    #[test]
    fn pairing_code_is_not_created_at_the_client_limit() {
        let directory = tempfile::tempdir().expect("state directory");
        let path = directory.path().join("auth.json");
        let (auth, mut grant) = AuthStore::initialize(path).expect("initialize auth");
        for index in 0..MAX_CLIENTS {
            auth.pair(&grant.code, &format!("client {index}"))
                .expect("pair client");
            if index + 1 < MAX_CLIENTS {
                grant = auth.create_pairing_code().expect("next code");
            }
        }

        let error = auth
            .create_pairing_code()
            .expect_err("client limit must reject a code");

        assert!(error.to_string().contains("client limit"));
    }

    #[cfg(unix)]
    #[test]
    fn auth_state_is_owner_only() {
        let directory = tempfile::tempdir().expect("state directory");
        let path = directory.path().join("auth.json");
        AuthStore::initialize(&path).expect("initialize auth");

        let mode = fs::metadata(path)
            .expect("auth metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(mode, 0o600);
    }
}
