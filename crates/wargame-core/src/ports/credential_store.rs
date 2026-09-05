//! `CredentialStore`: where secrets (Dreamhack session cookies, etc.) live
//! (DESIGN.md §3.3, §3.6; MVP.md §9).

use super::Secret;
use crate::error::Result;

pub trait CredentialStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Secret<String>>>;
    fn set(&self, key: &str, value: &Secret<String>) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}
