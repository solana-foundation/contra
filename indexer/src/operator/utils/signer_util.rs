use once_cell::sync::Lazy;
use solana_keychain::{Signer, SignerError, SolanaSigner};
use solana_sdk::pubkey::Pubkey;
use std::env;
use tracing::{info, warn};

/// Environment variables for admin signer
const ADMIN_SIGNER: &str = "ADMIN_SIGNER";

/// Environment variables for operator signer
const OPERATOR_SIGNER: &str = "OPERATOR_SIGNER";

// In memory env vars (per-signer)
const ADMIN_PRIVATE_KEY: &str = "ADMIN_PRIVATE_KEY";
const OPERATOR_PRIVATE_KEY: &str = "OPERATOR_PRIVATE_KEY";

// Vault env vars (per-signer)
const ADMIN_VAULT_ADDR: &str = "ADMIN_VAULT_ADDR";
const ADMIN_VAULT_TOKEN: &str = "ADMIN_VAULT_TOKEN";
const ADMIN_VAULT_KEY_NAME: &str = "ADMIN_VAULT_KEY_NAME";
const ADMIN_VAULT_PUBKEY: &str = "ADMIN_VAULT_PUBKEY";
const OPERATOR_VAULT_ADDR: &str = "OPERATOR_VAULT_ADDR";
const OPERATOR_VAULT_TOKEN: &str = "OPERATOR_VAULT_TOKEN";
const OPERATOR_VAULT_KEY_NAME: &str = "OPERATOR_VAULT_KEY_NAME";
const OPERATOR_VAULT_PUBKEY: &str = "OPERATOR_VAULT_PUBKEY";

// Turnkey env vars (per-signer)
const ADMIN_TURNKEY_API_PUBLIC_KEY: &str = "ADMIN_TURNKEY_API_PUBLIC_KEY";
const ADMIN_TURNKEY_API_PRIVATE_KEY: &str = "ADMIN_TURNKEY_API_PRIVATE_KEY";
const ADMIN_TURNKEY_ORGANIZATION_ID: &str = "ADMIN_TURNKEY_ORGANIZATION_ID";
const ADMIN_TURNKEY_PRIVATE_KEY_ID: &str = "ADMIN_TURNKEY_PRIVATE_KEY_ID";
const ADMIN_TURNKEY_PUBKEY: &str = "ADMIN_TURNKEY_PUBKEY";
const OPERATOR_TURNKEY_API_PUBLIC_KEY: &str = "OPERATOR_TURNKEY_API_PUBLIC_KEY";
const OPERATOR_TURNKEY_API_PRIVATE_KEY: &str = "OPERATOR_TURNKEY_API_PRIVATE_KEY";
const OPERATOR_TURNKEY_ORGANIZATION_ID: &str = "OPERATOR_TURNKEY_ORGANIZATION_ID";
const OPERATOR_TURNKEY_PRIVATE_KEY_ID: &str = "OPERATOR_TURNKEY_PRIVATE_KEY_ID";
const OPERATOR_TURNKEY_PUBKEY: &str = "OPERATOR_TURNKEY_PUBKEY";

// Privy env vars (per-signer)
const ADMIN_PRIVY_APP_ID: &str = "ADMIN_PRIVY_APP_ID";
const ADMIN_PRIVY_APP_SECRET: &str = "ADMIN_PRIVY_APP_SECRET";
const ADMIN_PRIVY_WALLET_ID: &str = "ADMIN_PRIVY_WALLET_ID";
const OPERATOR_PRIVY_APP_ID: &str = "OPERATOR_PRIVY_APP_ID";
const OPERATOR_PRIVY_APP_SECRET: &str = "OPERATOR_PRIVY_APP_SECRET";
const OPERATOR_PRIVY_WALLET_ID: &str = "OPERATOR_PRIVY_WALLET_ID";

#[derive(Debug, Clone, Copy)]
enum SignerType {
    Memory,
    Vault,
    Turnkey,
    Privy,
}

impl SignerType {
    fn from_str(s: &str) -> Result<Self, SignerError> {
        match s.to_lowercase().as_str() {
            "memory" => Ok(Self::Memory),
            "vault" => Ok(Self::Vault),
            "turnkey" => Ok(Self::Turnkey),
            "privy" => Ok(Self::Privy),
            other => Err(SignerError::InvalidPrivateKey(format!(
                "Unsupported signer type: {}. Supported: memory, vault, turnkey, privy",
                other
            ))),
        }
    }
}

/// Signer role for selecting env var prefixes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignerRole {
    Admin,
    Operator,
}

/// Global admin signer (required for both programs)
static ADMIN_SIGNER_INSTANCE: Lazy<Signer> =
    Lazy::new(|| load_signer(SignerRole::Admin).expect("ADMIN_SIGNER must be configured"));

/// Global operator signer (optional, only for release funds)
static OPERATOR_SIGNER_INSTANCE: Lazy<Option<Signer>> =
    Lazy::new(|| match load_signer(SignerRole::Operator) {
        Ok(signer) => Some(signer),
        Err(_) => {
            warn!("OPERATOR_SIGNER not configured - release funds will use admin as operator");
            None
        }
    });

/// Load signer from environment variables
fn load_signer(role: SignerRole) -> Result<Signer, SignerError> {
    let (role_name, type_var) = match role {
        SignerRole::Admin => ("admin", ADMIN_SIGNER),
        SignerRole::Operator => ("operator", OPERATOR_SIGNER),
    };

    let signer_type_str = env::var(type_var)
        .map_err(|_| SignerError::InvalidPrivateKey(format!("{} not set", type_var)))?;
    let signer_type = SignerType::from_str(&signer_type_str)?;

    let signer = match signer_type {
        SignerType::Memory => {
            let private_key_var = match role {
                SignerRole::Admin => ADMIN_PRIVATE_KEY,
                SignerRole::Operator => OPERATOR_PRIVATE_KEY,
            };
            let private_key = env::var(private_key_var).map_err(|_| {
                SignerError::InvalidPrivateKey(format!("{} not set", private_key_var))
            })?;

            Signer::from_memory(&private_key)?
        }
        SignerType::Vault => {
            let (vault_addr_var, vault_token_var, key_name_var, pubkey_var) = match role {
                SignerRole::Admin => (
                    ADMIN_VAULT_ADDR,
                    ADMIN_VAULT_TOKEN,
                    ADMIN_VAULT_KEY_NAME,
                    ADMIN_VAULT_PUBKEY,
                ),
                SignerRole::Operator => (
                    OPERATOR_VAULT_ADDR,
                    OPERATOR_VAULT_TOKEN,
                    OPERATOR_VAULT_KEY_NAME,
                    OPERATOR_VAULT_PUBKEY,
                ),
            };
            let vault_addr = env::var(vault_addr_var).map_err(|_| {
                SignerError::InvalidPrivateKey(format!("{} not set", vault_addr_var))
            })?;
            let vault_token = env::var(vault_token_var).map_err(|_| {
                SignerError::InvalidPrivateKey(format!("{} not set", vault_token_var))
            })?;

            let key_name = env::var(key_name_var)
                .map_err(|_| SignerError::InvalidPrivateKey(format!("{} not set", key_name_var)))?;
            let pubkey = env::var(pubkey_var)
                .map_err(|_| SignerError::InvalidPrivateKey(format!("{} not set", pubkey_var)))?;
            Signer::from_vault(vault_addr, vault_token, key_name, pubkey)?
        }
        SignerType::Turnkey => {
            let (
                api_public_key_var,
                api_private_key_var,
                organization_id_var,
                pubkey_var,
                private_key_id_var,
            ) = match role {
                SignerRole::Admin => (
                    ADMIN_TURNKEY_API_PUBLIC_KEY,
                    ADMIN_TURNKEY_API_PRIVATE_KEY,
                    ADMIN_TURNKEY_ORGANIZATION_ID,
                    ADMIN_TURNKEY_PUBKEY,
                    ADMIN_TURNKEY_PRIVATE_KEY_ID,
                ),
                SignerRole::Operator => (
                    OPERATOR_TURNKEY_API_PUBLIC_KEY,
                    OPERATOR_TURNKEY_API_PRIVATE_KEY,
                    OPERATOR_TURNKEY_ORGANIZATION_ID,
                    OPERATOR_TURNKEY_PUBKEY,
                    OPERATOR_TURNKEY_PRIVATE_KEY_ID,
                ),
            };
            let api_public_key = env::var(api_public_key_var).map_err(|_| {
                SignerError::InvalidPrivateKey(format!("{} not set", api_public_key_var))
            })?;
            let api_private_key = env::var(api_private_key_var).map_err(|_| {
                SignerError::InvalidPrivateKey(format!("{} not set", api_private_key_var))
            })?;
            let organization_id = env::var(organization_id_var).map_err(|_| {
                SignerError::InvalidPrivateKey(format!("{} not set", organization_id_var))
            })?;
            let public_key = env::var(pubkey_var)
                .map_err(|_| SignerError::InvalidPrivateKey(format!("{} not set", pubkey_var)))?;
            let private_key_id = env::var(private_key_id_var).map_err(|_| {
                SignerError::InvalidPrivateKey(format!("{} not set", private_key_id_var))
            })?;
            Signer::from_turnkey(
                api_public_key,
                api_private_key,
                organization_id,
                private_key_id,
                public_key,
            )?
        }
        SignerType::Privy => {
            let (app_id_var, app_secret_var, wallet_id_var) = match role {
                SignerRole::Admin => (
                    ADMIN_PRIVY_APP_ID,
                    ADMIN_PRIVY_APP_SECRET,
                    ADMIN_PRIVY_WALLET_ID,
                ),
                SignerRole::Operator => (
                    OPERATOR_PRIVY_APP_ID,
                    OPERATOR_PRIVY_APP_SECRET,
                    OPERATOR_PRIVY_WALLET_ID,
                ),
            };
            let app_id = env::var(app_id_var)
                .map_err(|_| SignerError::InvalidPrivateKey(format!("{} not set", app_id_var)))?;
            let app_secret = env::var(app_secret_var).map_err(|_| {
                SignerError::InvalidPrivateKey(format!("{} not set", app_secret_var))
            })?;
            let wallet_id = env::var(wallet_id_var).map_err(|_| {
                SignerError::InvalidPrivateKey(format!("{} not set", wallet_id_var))
            })?;

            // Block on async initialization
            tokio::runtime::Handle::current()
                .block_on(Signer::from_privy(app_id, app_secret, wallet_id))?
        }
    };

    info!(
        "Loaded {} signer ({}): {}",
        role_name,
        signer_type_str,
        signer.pubkey()
    );
    Ok(signer)
}

pub struct SignerUtil;

impl SignerUtil {
    pub fn get_admin_pubkey() -> Pubkey {
        Self::admin_signer().pubkey()
    }

    pub fn get_operator_pubkey() -> Pubkey {
        Self::operator_signer().pubkey()
    }

    pub fn admin_signer() -> &'static Signer {
        &ADMIN_SIGNER_INSTANCE
    }

    pub fn operator_signer() -> &'static Signer {
        OPERATOR_SIGNER_INSTANCE
            .as_ref()
            .unwrap_or(&ADMIN_SIGNER_INSTANCE)
    }
}
