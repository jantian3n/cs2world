use ipnet::IpNet;
use std::io;
use std::net::{IpAddr, SocketAddr};
use tokio::net::lookup_host;

pub use cs2world_proto::mux::Target;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Direct,
    Proxy,
    Block,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Network {
    Tcp,
    Udp,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub networks: Vec<Network>,
    pub domains: Vec<String>,
    pub suffixes: Vec<String>,
    pub cidrs: Vec<IpNet>,
    pub private: bool,
    pub action: Action,
}

#[derive(Clone, Debug)]
pub struct RouteTable {
    pub rules: Vec<Rule>,
    pub fallback: Action,
}

fn normalized(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

fn private(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(a) => {
            a.is_private()
                || a.is_loopback()
                || a.is_link_local()
                || a.is_multicast()
                || a.is_unspecified()
        }
        IpAddr::V6(a) => {
            a.is_unique_local()
                || a.is_unicast_link_local()
                || a.is_loopback()
                || a.is_multicast()
                || a.is_unspecified()
        }
    }
}

impl Rule {
    fn matches(&self, host: &str, net: Network) -> bool {
        if !self.networks.is_empty() && !self.networks.contains(&net) {
            return false;
        }
        if self.domains.is_empty()
            && self.suffixes.is_empty()
            && self.cidrs.is_empty()
            && !self.private
        {
            return true;
        }
        if let Ok(ip) = host.parse::<IpAddr>() {
            let ip = ip.to_canonical();
            return (self.private && private(ip)) || self.cidrs.iter().any(|c| c.contains(&ip));
        }
        let host = normalized(host);
        self.domains.iter().any(|d| normalized(d) == host)
            || self.suffixes.iter().any(|suffix| {
                let suffix = normalized(suffix.trim_start_matches('.'));
                host == suffix || host.strip_suffix(&suffix).is_some_and(|p| p.ends_with('.'))
            })
    }
}

impl RouteTable {
    pub fn direct() -> Self {
        Self {
            rules: vec![],
            fallback: Action::Direct,
        }
    }

    pub fn action(&self, target: &Target, net: Network) -> Action {
        self.action_with_name(target, net, None)
    }
    pub(crate) fn action_with_name(
        &self,
        target: &Target,
        net: Network,
        name: Option<&str>,
    ) -> Action {
        self.rules
            .iter()
            .find(|r| r.matches(&target.host, net) || name.is_some_and(|s| r.matches(s, net)))
            .map_or(self.fallback, |r| r.action)
    }

    /// Resolve once and dial one of these exact endpoints. Server access rules apply to
    /// each datagram and each resolved address, including a domain's private results.
    pub(crate) async fn server_endpoints(
        &self,
        target: &Target,
        net: Network,
    ) -> io::Result<Vec<SocketAddr>> {
        let denied = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "destination blocked by server route",
            )
        };
        if target.port == 0 || self.action(target, net) != Action::Direct {
            return Err(denied());
        }
        let mut out = Vec::new();
        for addr in lookup_host((target.host.as_str(), target.port)).await? {
            let addr = SocketAddr::new(addr.ip().to_canonical(), addr.port());
            let ip = addr.ip().to_string();
            if self
                .rules
                .iter()
                .filter(|r| r.private || !r.cidrs.is_empty())
                .find(|r| r.matches(&ip, net))
                .is_none_or(|r| r.action == Action::Direct)
                && !out.contains(&addr)
            {
                out.push(addr);
            }
        }
        if out.is_empty() {
            Err(denied())
        } else {
            Ok(out)
        }
    }
}
