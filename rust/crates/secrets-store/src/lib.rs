use base64::Engine;
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde::Serialize;
use secret_service::blocking::SecretService;
use secret_service::EncryptionType;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::iter;
use zeroize::Zeroizing;

const ENVELOPE_PREFIX: &str = "age1:";
const STREAM_ENVELOPE_PREFIX: &[u8] = b"agebin1\n";
const DEFAULT_SECRET_SERVICE_LOOKUP_ENV: &str = "EDEX_CORE_SECRET_SERVICE_LOOKUP";
const DEFAULT_SECRET_SERVICE_SERVICE_ENV: &str = "EDEX_CORE_SECRET_SERVICE_SERVICE";
const DEFAULT_SECRET_SERVICE_ACCOUNT_ENV: &str = "EDEX_CORE_SECRET_SERVICE_ACCOUNT";
const DEFAULT_SECRET_SERVICE_LABEL_ENV: &str = "EDEX_CORE_SECRET_SERVICE_LABEL";
const DEFAULT_SECRET_SERVICE_COLLECTION_ENV: &str = "EDEX_CORE_SECRET_SERVICE_COLLECTION";
const DEFAULT_SECRET_SERVICE_SERVICE: &str = "edex-ui-2026";
const DEFAULT_SECRET_SERVICE_ACCOUNT: &str = "rust-core-master-passphrase";
const DEFAULT_SECRET_SERVICE_LABEL: &str = "eDEX-UI 2026 Rust Core Master Passphrase";
const DEFAULT_SECRET_SERVICE_COLLECTION: &str = "default";

#[derive(Debug, Clone)]
pub struct SecretsStore {
    passphrase: SecretString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSource {
    Env,
    SecretService,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretServiceTarget {
    pub service: String,
    pub account: String,
    pub label: String,
    pub collection: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedSecretsStore {
    pub store: SecretsStore,
    pub source: SecretSource,
}

#[derive(Debug, thiserror::Error)]
pub enum SecretsStoreError {
    #[error("master passphrase is not configured")]
    MissingMasterPassphrase,
    #[error("encrypted payload is malformed")]
    MalformedEnvelope,
    #[error("secrets-store encryption error: {0}")]
    Encryption(String),
    #[error("secrets-store decryption error: {0}")]
    Decryption(String),
    #[error("secret-service integration error: {0}")]
    SecretService(String),
}

impl SecretsStore {
    pub fn from_passphrase(passphrase: SecretString) -> Result<Self, SecretsStoreError> {
        if passphrase.expose_secret().trim().is_empty() {
            return Err(SecretsStoreError::MissingMasterPassphrase);
        }

        Ok(Self { passphrase })
    }

    pub fn from_owned_passphrase(passphrase: String) -> Result<Self, SecretsStoreError> {
        Self::from_passphrase(SecretString::from(passphrase))
    }

    pub fn from_env() -> Result<Option<Self>, SecretsStoreError> {
        Self::from_env_var("EDEX_CORE_MASTER_PASSPHRASE")
    }

    pub fn from_env_var(var_name: &str) -> Result<Option<Self>, SecretsStoreError> {
        match std::env::var(var_name) {
            Ok(value) => Ok(Some(Self::from_passphrase(SecretString::from(value))?)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(error) => Err(SecretsStoreError::Encryption(error.to_string())),
        }
    }

    pub fn resolve() -> Result<Option<ResolvedSecretsStore>, SecretsStoreError> {
        if let Some(store) = Self::from_env()? {
            return Ok(Some(ResolvedSecretsStore {
                store,
                source: SecretSource::Env,
            }));
        }

        if !env_flag(DEFAULT_SECRET_SERVICE_LOOKUP_ENV) {
            return Ok(None);
        }

        let target = SecretServiceTarget::from_env();
        let store = match Self::load_from_secret_service(&target)? {
            Some(store) => store,
            None => return Ok(None),
        };

        Ok(Some(ResolvedSecretsStore {
            store,
            source: SecretSource::SecretService,
        }))
    }

    pub fn load_from_secret_service(
        target: &SecretServiceTarget,
    ) -> Result<Option<Self>, SecretsStoreError> {
        let secret_service = SecretService::connect(EncryptionType::Dh)
            .map_err(|error| SecretsStoreError::SecretService(error.to_string()))?;
        let collection = secret_service
            .get_collection_by_alias(&target.collection)
            .map_err(|error| SecretsStoreError::SecretService(error.to_string()))?;

        let search = collection
            .search_items(target.attributes())
            .map_err(|error| SecretsStoreError::SecretService(error.to_string()))?;
        let Some(item) = search.into_iter().next() else {
            return Ok(None);
        };

        item.unlock()
            .map_err(|error| SecretsStoreError::SecretService(error.to_string()))?;

        let secret = item
            .get_secret()
            .map_err(|error| SecretsStoreError::SecretService(error.to_string()))?;
        let passphrase = String::from_utf8(secret)
            .map_err(|error| SecretsStoreError::SecretService(error.to_string()))?;

        Ok(Some(Self::from_passphrase(SecretString::from(passphrase))?))
    }

    pub fn store_in_secret_service(
        &self,
        target: &SecretServiceTarget,
    ) -> Result<(), SecretsStoreError> {
        let secret_service = SecretService::connect(EncryptionType::Dh)
            .map_err(|error| SecretsStoreError::SecretService(error.to_string()))?;
        let collection = secret_service
            .get_collection_by_alias(&target.collection)
            .map_err(|error| SecretsStoreError::SecretService(error.to_string()))?;

        collection
            .create_item(
                &target.label,
                target.attributes(),
                self.passphrase.expose_secret().as_bytes(),
                true,
                "text/plain",
            )
            .map_err(|error| SecretsStoreError::SecretService(error.to_string()))?;

        Ok(())
    }

    pub fn encrypt_string(&self, plaintext: &str) -> Result<String, SecretsStoreError> {
        let encryptor = age::Encryptor::with_user_passphrase(self.passphrase.clone());
        let mut output = Vec::new();
        let mut writer = encryptor
            .wrap_output(&mut output)
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;
        writer
            .write_all(plaintext.as_bytes())
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;
        writer
            .finish()
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;

        Ok(format!(
            "{ENVELOPE_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(output)
        ))
    }

    pub fn decrypt_string(&self, ciphertext: &str) -> Result<String, SecretsStoreError> {
        let payload = ciphertext
            .strip_prefix(ENVELOPE_PREFIX)
            .ok_or(SecretsStoreError::MalformedEnvelope)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|_| SecretsStoreError::MalformedEnvelope)?;

        let decryptor = age::Decryptor::new(decoded.as_slice())
            .map_err(|error| SecretsStoreError::Decryption(error.to_string()))?;
        let identity = age::scrypt::Identity::new(self.passphrase.clone());
        let mut reader = decryptor
            .decrypt(iter::once(&identity as &dyn age::Identity))
            .map_err(|error| SecretsStoreError::Decryption(error.to_string()))?;

        let mut plaintext = Zeroizing::new(Vec::new());
        reader
            .read_to_end(&mut plaintext)
            .map_err(|error| SecretsStoreError::Decryption(error.to_string()))?;
        String::from_utf8(plaintext.to_vec())
            .map_err(|error| SecretsStoreError::Decryption(error.to_string()))
    }

    pub fn is_encrypted_payload(value: &str) -> bool {
        value.starts_with(ENVELOPE_PREFIX)
    }

    pub fn encrypt_json_to_writer<T, W>(
        &self,
        writer: &mut W,
        value: &T,
    ) -> Result<(), SecretsStoreError>
    where
        T: Serialize,
        W: Write,
    {
        writer
            .write_all(STREAM_ENVELOPE_PREFIX)
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;
        let encryptor = age::Encryptor::with_user_passphrase(self.passphrase.clone());
        let mut encrypted = encryptor
            .wrap_output(writer)
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;
        serde_json::to_writer(&mut encrypted, value)
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;
        encrypted
            .finish()
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;
        Ok(())
    }

    pub fn decrypt_json_from_reader<T, R>(&self, mut reader: R) -> Result<T, SecretsStoreError>
    where
        T: DeserializeOwned,
        R: Read,
    {
        let mut prefix = vec![0_u8; STREAM_ENVELOPE_PREFIX.len()];
        reader
            .read_exact(&mut prefix)
            .map_err(|_| SecretsStoreError::MalformedEnvelope)?;
        if prefix.as_slice() != STREAM_ENVELOPE_PREFIX {
            return Err(SecretsStoreError::MalformedEnvelope);
        }
        let decryptor = age::Decryptor::new(reader)
            .map_err(|error| SecretsStoreError::Decryption(error.to_string()))?;
        let identity = age::scrypt::Identity::new(self.passphrase.clone());
        let decrypted = decryptor
            .decrypt(iter::once(&identity as &dyn age::Identity))
            .map_err(|error| SecretsStoreError::Decryption(error.to_string()))?;
        serde_json::from_reader(decrypted)
            .map_err(|error| SecretsStoreError::Decryption(error.to_string()))
    }

    pub fn reencrypt_reader_to_writer<R, W>(
        &self,
        mut input: R,
        new_secrets: &SecretsStore,
        output: &mut W,
    ) -> Result<(), SecretsStoreError>
    where
        R: Read,
        W: Write,
    {
        let mut prefix = vec![0_u8; STREAM_ENVELOPE_PREFIX.len()];
        input
            .read_exact(&mut prefix)
            .map_err(|_| SecretsStoreError::MalformedEnvelope)?;
        if prefix.as_slice() != STREAM_ENVELOPE_PREFIX {
            return Err(SecretsStoreError::MalformedEnvelope);
        }

        output
            .write_all(STREAM_ENVELOPE_PREFIX)
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;
        let decryptor = age::Decryptor::new(input)
            .map_err(|error| SecretsStoreError::Decryption(error.to_string()))?;
        let identity = age::scrypt::Identity::new(self.passphrase.clone());
        let mut decrypted = decryptor
            .decrypt(iter::once(&identity as &dyn age::Identity))
            .map_err(|error| SecretsStoreError::Decryption(error.to_string()))?;

        let encryptor = age::Encryptor::with_user_passphrase(new_secrets.passphrase.clone());
        let mut encrypted = encryptor
            .wrap_output(output)
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;
        std::io::copy(&mut decrypted, &mut encrypted)
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;
        encrypted
            .finish()
            .map_err(|error| SecretsStoreError::Encryption(error.to_string()))?;
        Ok(())
    }

    pub fn is_encrypted_stream_payload(bytes: &[u8]) -> bool {
        bytes.starts_with(STREAM_ENVELOPE_PREFIX)
    }
}

impl SecretServiceTarget {
    pub fn from_env() -> Self {
        Self {
            service: read_env_or_default(
                DEFAULT_SECRET_SERVICE_SERVICE_ENV,
                DEFAULT_SECRET_SERVICE_SERVICE,
            ),
            account: read_env_or_default(
                DEFAULT_SECRET_SERVICE_ACCOUNT_ENV,
                DEFAULT_SECRET_SERVICE_ACCOUNT,
            ),
            label: read_env_or_default(
                DEFAULT_SECRET_SERVICE_LABEL_ENV,
                DEFAULT_SECRET_SERVICE_LABEL,
            ),
            collection: read_env_or_default(
                DEFAULT_SECRET_SERVICE_COLLECTION_ENV,
                DEFAULT_SECRET_SERVICE_COLLECTION,
            ),
        }
    }

    fn attributes(&self) -> HashMap<&str, &str> {
        HashMap::from([
            ("service", self.service.as_str()),
            ("account", self.account.as_str()),
        ])
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn read_env_or_default(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypts_and_decrypts_string_payload() {
        let secrets = SecretsStore::from_passphrase(SecretString::from("test-passphrase"))
            .expect("passphrase should initialize secrets-store");

        let ciphertext = secrets
            .encrypt_string("terminal output goes here")
            .expect("payload should encrypt");
        assert!(SecretsStore::is_encrypted_payload(&ciphertext));

        let plaintext = secrets
            .decrypt_string(&ciphertext)
            .expect("payload should decrypt");
        assert_eq!(plaintext, "terminal output goes here");
    }

    #[test]
    fn env_resolution_prefers_master_passphrase() {
        std::env::set_var("EDEX_CORE_MASTER_PASSPHRASE", "env-passphrase");
        std::env::set_var(DEFAULT_SECRET_SERVICE_LOOKUP_ENV, "1");

        let resolved = SecretsStore::resolve()
            .expect("resolution should succeed")
            .expect("env source should win");

        assert_eq!(resolved.source, SecretSource::Env);
        let roundtrip = resolved
            .store
            .encrypt_string("hello")
            .and_then(|ciphertext| resolved.store.decrypt_string(&ciphertext))
            .expect("env-backed store should work");
        assert_eq!(roundtrip, "hello");

        std::env::remove_var("EDEX_CORE_MASTER_PASSPHRASE");
        std::env::remove_var(DEFAULT_SECRET_SERVICE_LOOKUP_ENV);
    }

    #[test]
    fn secret_service_target_uses_expected_defaults() {
        std::env::remove_var(DEFAULT_SECRET_SERVICE_SERVICE_ENV);
        std::env::remove_var(DEFAULT_SECRET_SERVICE_ACCOUNT_ENV);
        std::env::remove_var(DEFAULT_SECRET_SERVICE_LABEL_ENV);
        std::env::remove_var(DEFAULT_SECRET_SERVICE_COLLECTION_ENV);

        let target = SecretServiceTarget::from_env();
        assert_eq!(target.service, DEFAULT_SECRET_SERVICE_SERVICE);
        assert_eq!(target.account, DEFAULT_SECRET_SERVICE_ACCOUNT);
        assert_eq!(target.label, DEFAULT_SECRET_SERVICE_LABEL);
        assert_eq!(target.collection, DEFAULT_SECRET_SERVICE_COLLECTION);
    }
}
