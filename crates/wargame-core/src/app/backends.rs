//! Backend and transport selection (spec.md §5 item 1: "backend resolver는
//! launch에는 profile, 기존 세션 작업에는 저장된 backend/VM ID를 사용한다.
//! 전역 단일 provider에 우회 분기를 누적하지 않는다").
//!
//! Two registries and three lookups:
//!
//! * [`BackendRegistry`] holds every wired [`VmProvider`] by name.
//! * [`TransportRegistry`] picks a [`GuestTransport`] by an endpoint's shell.
//! * [`resolve_backend`] answers "which backend runs this *profile*"
//!   (new launches) and [`provider_for_session`] answers "which backend owns
//!   this *session's* VM" (everything afterwards). The second never consults
//!   the profile or a global default, so editing a profile or changing the
//!   default backend cannot strand a running session.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::domain::compat::{self, ResolvedBackend};
use crate::domain::{GuestEndpoint, GuestShell, Profile, Session};
use crate::error::{Result, WargameError};
use crate::ports::{GuestTransport, VmProvider};

use super::AppContext;

/// Every VM backend this build has wired, keyed by [`VmProvider::name`].
#[derive(Clone, Default)]
pub struct BackendRegistry {
    providers: BTreeMap<String, Arc<dyn VmProvider>>,
    /// Registration order, so `primary()` is stable and independent of
    /// alphabetical accidents.
    order: Vec<String>,
}

impl BackendRegistry {
    /// Registers `providers` in order. A later provider with the same name
    /// replaces an earlier one (there is exactly one adapter per backend).
    pub fn new(providers: Vec<Arc<dyn VmProvider>>) -> Self {
        let mut registry = BackendRegistry::default();
        for provider in providers {
            registry.insert(provider);
        }
        registry
    }

    fn insert(&mut self, provider: Arc<dyn VmProvider>) {
        let name = provider.name().to_string();
        if !self.providers.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.providers.insert(name, provider);
    }

    /// The provider registered under `name`, or a
    /// [`WargameError::BackendUnavailable`] naming it and what *is* wired.
    pub fn get(&self, name: &str) -> Result<Arc<dyn VmProvider>> {
        self.providers
            .get(name)
            .cloned()
            .ok_or_else(|| WargameError::BackendUnavailable {
                backend: name.to_string(),
                reason: format!(
                    "no {name:?} backend is wired into this build (available: {})",
                    if self.order.is_empty() {
                        "none".to_string()
                    } else {
                        self.order.join(", ")
                    }
                ),
            })
    }

    /// Every provider, in registration order.
    pub fn all(&self) -> Vec<Arc<dyn VmProvider>> {
        self.order
            .iter()
            .filter_map(|name| self.providers.get(name).cloned())
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.order.clone()
    }
}

impl std::fmt::Debug for BackendRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendRegistry")
            .field("backends", &self.order)
            .finish()
    }
}

/// The transports available for talking to a guest, chosen by the endpoint's
/// [`GuestShell`]. `wiring.rs` registers the same endpoint-driven
/// [`crate::infra::SshGuestTransport`] in both slots; a build with no
/// PowerShell transport registered refuses a Windows endpoint outright,
/// rather than opening a POSIX connection that would quote its arguments
/// wrongly.
#[derive(Clone)]
pub struct TransportRegistry {
    posix: Arc<dyn GuestTransport>,
    powershell: Option<Arc<dyn GuestTransport>>,
}

impl TransportRegistry {
    pub fn new(posix: Arc<dyn GuestTransport>) -> Self {
        TransportRegistry {
            posix,
            powershell: None,
        }
    }

    pub fn with_powershell(mut self, transport: Arc<dyn GuestTransport>) -> Self {
        self.powershell = Some(transport);
        self
    }

    /// The transport that can speak `endpoint`'s shell.
    pub fn for_endpoint(&self, endpoint: &GuestEndpoint) -> Result<Arc<dyn GuestTransport>> {
        self.for_shell(endpoint.shell)
    }

    /// The transport for a shell, with no address yet -- what `launch`
    /// preflight uses to refuse a guest it could never talk to *before*
    /// creating or starting anything (spec.md §3 FR-4).
    pub fn for_shell(&self, shell: GuestShell) -> Result<Arc<dyn GuestTransport>> {
        match shell {
            GuestShell::Posix => Ok(self.posix.clone()),
            GuestShell::PowerShell => self.powershell.clone().ok_or_else(|| {
                WargameError::Other(
                    "no PowerShell guest transport is wired into this build".to_string(),
                )
            }),
        }
    }
}

/// Which backend a *new* launch of `profile` runs on, and in which mode
/// (spec.md §3 FR-1). Pure resolution against the support matrix: it touches
/// no backend, so it is safe -- and required -- to call before anything is
/// created, cloned or started.
pub fn resolve_backend(profile: &Profile) -> Result<ResolvedBackend> {
    compat::resolve(profile)
}

/// [`resolve_backend`] plus the wired provider that implements it.
pub fn provider_for_profile(
    ctx: &AppContext,
    profile: &Profile,
) -> Result<(ResolvedBackend, Arc<dyn VmProvider>)> {
    let resolved = resolve_backend(profile)?;
    let provider = ctx.backends.get(resolved.backend.as_str())?;
    Ok((resolved, provider))
}

/// The backend that owns `session`'s VM.
///
/// Routing is by the session's recorded backend, never by the profile (which
/// may have been edited) or by a global default (which may have changed), so
/// two sessions whose VMs share a name on different backends can never be
/// confused (spec.md §3 FR-5).
pub fn provider_for_session(ctx: &AppContext, session: &Session) -> Result<Arc<dyn VmProvider>> {
    ctx.backends.get(session.backend_name())
}

/// The endpoint and transport for talking to `session`'s guest, built from
/// the connection settings the session saved at launch.
pub fn transport_for_session(
    ctx: &AppContext,
    session: &Session,
) -> Result<(GuestEndpoint, Arc<dyn GuestTransport>)> {
    let endpoint = session.endpoint().ok_or_else(|| {
        WargameError::NetworkUnavailable("session has no guest IP yet".to_string())
    })?;
    let transport = ctx.transports.for_endpoint(&endpoint)?;
    Ok((endpoint, transport))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeTransport, FakeVmProvider};
    use std::net::IpAddr;
    use std::str::FromStr;

    fn endpoint(shell: GuestShell) -> GuestEndpoint {
        GuestEndpoint {
            host: IpAddr::from_str("10.0.0.5").unwrap(),
            port: 22,
            user: "admin".to_string(),
            key_path: None,
            shell,
        }
    }

    #[test]
    fn registry_looks_up_by_backend_name() {
        let registry = BackendRegistry::new(vec![
            Arc::new(FakeVmProvider::new()) as Arc<dyn VmProvider>,
            Arc::new(FakeVmProvider::with_backend("utm")) as Arc<dyn VmProvider>,
        ]);
        assert_eq!(registry.get("tart").unwrap().name(), "tart");
        assert_eq!(registry.get("utm").unwrap().name(), "utm");
        assert_eq!(registry.names(), vec!["tart", "utm"]);
    }

    #[test]
    fn an_unwired_backend_is_named_in_the_error() {
        let registry = BackendRegistry::new(vec![Arc::new(FakeVmProvider::new())]);
        let message = registry
            .get("utm")
            .err()
            .expect("an unwired backend is an error")
            .to_string();
        assert!(message.contains("utm"), "{message}");
        assert!(message.contains("tart"), "{message}");
    }

    #[test]
    fn transports_are_selected_by_shell() {
        let registry = TransportRegistry::new(Arc::new(FakeTransport::new()));
        assert!(registry.for_endpoint(&endpoint(GuestShell::Posix)).is_ok());
        assert!(registry
            .for_endpoint(&endpoint(GuestShell::PowerShell))
            .is_err());

        let registry = registry.with_powershell(Arc::new(FakeTransport::new()));
        assert!(registry
            .for_endpoint(&endpoint(GuestShell::PowerShell))
            .is_ok());
    }
}
