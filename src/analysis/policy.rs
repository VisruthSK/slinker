#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryPolicy {
    Reject,
    Internalize,
    ExternalOnly,
}

#[derive(Clone, Copy, Debug)]
pub struct LinkPolicy {
    pub namespace_discovery: DiscoveryPolicy,
}

impl Default for LinkPolicy {
    fn default() -> Self {
        Self {
            namespace_discovery: DiscoveryPolicy::Reject,
        }
    }
}
