//! Shared in-memory state: pushed project configs + currently-tracked service
//! containers.
//!
//! The proxy reads `/etc/projects/*.json` from a docker volume on startup, and
//! mutates the service map in response to docker container start/die events.
//! The derived `names` map (hostname → container IP) is rebuilt on each
//! change and consumed by the DNS server.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use fqdn::FQDN;
use fqdn_trie::FqdnTrieMap;
use shared::ProxyOptions;
use tokio::sync::RwLock;

/// One running compose service container tracked from docker start events.
#[derive(Debug, Clone)]
pub struct RunningService {
    pub project: String,
    pub workspace: String,
    pub service: String,
    pub target_cid: String,
    pub container_ip: IpAddr,
    pub sidecar_id: Option<String>,
}

pub struct RegistryInner {
    pub configs: HashMap<String, ProxyOptions>,
    pub services: HashMap<String, RunningService>,
    pub names: FqdnTrieMap<FQDN, Option<IpAddr>>,
}

impl Default for RegistryInner {
    fn default() -> Self {
        Self {
            configs: HashMap::new(),
            services: HashMap::new(),
            names: FqdnTrieMap::new(None),
        }
    }
}

#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<RwLock<RegistryInner>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn load_configs(&self, configs: Vec<(String, ProxyOptions)>) {
        let mut inner = self.inner.write().await;
        inner.configs.clear();
        for (name, opts) in configs {
            inner.configs.insert(name, opts);
        }
        rebuild_names(&mut inner);
    }

    pub async fn config_for(&self, project: &str) -> Option<ProxyOptions> {
        self.inner.read().await.configs.get(project).cloned()
    }

    pub async fn track_service(&self, svc: RunningService) {
        let mut inner = self.inner.write().await;
        inner.services.insert(svc.target_cid.clone(), svc);
        rebuild_names(&mut inner);
    }

    pub async fn has_service(&self, target_cid: &str) -> bool {
        self.inner.read().await.services.contains_key(target_cid)
    }

    pub async fn untrack_service(&self, target_cid: &str) -> Option<RunningService> {
        let mut inner = self.inner.write().await;
        let removed = inner.services.remove(target_cid);
        if removed.is_some() {
            rebuild_names(&mut inner);
        }
        removed
    }

    /// Reconcile tracked services against the containers docker currently
    /// reports as running (`cid` → its IP, if it has one).
    ///
    /// Entries whose container is absent are dropped and returned, so the
    /// caller can clean up their sidecars; entries whose IP moved are updated
    /// in place. This is what makes a lost `die` or `start` event self-heal.
    pub async fn reconcile_services(
        &self,
        alive: &HashMap<String, Option<IpAddr>>,
    ) -> Vec<RunningService> {
        let mut inner = self.inner.write().await;
        let mut dropped = Vec::new();
        inner.services.retain(|cid, svc| {
            if alive.contains_key(cid) {
                return true;
            }
            dropped.push(svc.clone());
            false
        });
        let mut moved = false;
        for (cid, svc) in &mut inner.services {
            let Some(Some(ip)) = alive.get(cid) else {
                continue;
            };
            if svc.container_ip != *ip {
                tracing::info!(
                    project = svc.project,
                    workspace = svc.workspace,
                    service = svc.service,
                    old = %svc.container_ip,
                    new = %ip,
                    "container IP changed"
                );
                svc.container_ip = *ip;
                moved = true;
            }
        }
        if moved || !dropped.is_empty() {
            rebuild_names(&mut inner);
        }
        dropped
    }

    /// Lookup a hostname → IP for DNS. The caller is expected to have already
    /// lowercased the host and trimmed any trailing dot.
    ///
    /// Subdomains resolve to their parent: if `foo.test` is registered, then
    /// `bar.foo.test` and `baz.bar.foo.test` resolve to the same IP. The most
    /// specific (longest) registered suffix wins.
    pub async fn resolve(&self, host: &str) -> Option<IpAddr> {
        let fqdn: FQDN = host.parse().ok()?;
        *self.inner.read().await.names.lookup(&fqdn)
    }
}

fn rebuild_names(inner: &mut RegistryInner) {
    let mut names = FqdnTrieMap::new(None);
    for svc in inner.services.values() {
        let Some(opts) = inner.configs.get(&svc.project) else {
            continue;
        };
        let root = svc.workspace == svc.project;
        let Some(hostname) = opts.render_hostname(&svc.project, &svc.workspace, &svc.service, root)
        else {
            continue;
        };
        let fqdn: FQDN = match hostname.parse() {
            Ok(fqdn) => fqdn,
            Err(e) => {
                tracing::warn!(hostname = %hostname, "invalid hostname, skipping: {e}");
                continue;
            }
        };
        // Keep the first registration on collision (exact-match check, since
        // the trie only materializes nodes for real entries).
        if let Some(Some(existing)) = names.get(&fqdn).copied() {
            if existing != svc.container_ip {
                tracing::warn!(
                    hostname = %hostname,
                    existing = %existing,
                    new = %svc.container_ip,
                    "hostname collision; keeping existing"
                );
            }
            continue;
        }
        names.insert(fqdn, Some(svc.container_ip));
    }
    inner.names = names;
}
