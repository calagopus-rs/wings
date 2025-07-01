use hickory_resolver::{TokioResolver, config::LookupIpStrategy, lookup_ip::LookupIpIntoIter};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::{net::SocketAddr, sync::Arc};

#[derive(Clone)]
pub struct DnsResolver {
    config: Arc<crate::config::Config>,
    state: Arc<TokioResolver>,
}

impl DnsResolver {
    pub fn new(config: &Arc<crate::config::Config>) -> Self {
        let mut builder = TokioResolver::builder_tokio().unwrap();
        builder.options_mut().ip_strategy = LookupIpStrategy::Ipv4AndIpv6;

        Self {
            config: Arc::clone(config),
            state: Arc::new(builder.build()),
        }
    }
}

impl Resolve for DnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.clone();

        Box::pin(async move {
            let lookup = resolver.state.lookup_ip(name.as_str()).await?;
            let addrs: Addrs = Box::new(SocketAddrs {
                config: Arc::clone(&resolver.config),
                iter: lookup.into_iter(),
            });

            Ok(addrs)
        })
    }
}

struct SocketAddrs {
    config: Arc<crate::config::Config>,
    iter: LookupIpIntoIter,
}

impl Iterator for SocketAddrs {
    type Item = SocketAddr;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self
            .iter
            .next()
            .map(|ip_addr| SocketAddr::new(ip_addr, 0))?;

        for cidr in self.config.api.remote_download_blocked_cidrs.iter() {
            if cidr.contains(&next.ip()) {
                tracing::warn!("blocking internal IP address in pull: {}", next.ip());
                return self.next();
            }
        }

        Some(next)
    }
}
