#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryPolicy {
    Reject,
    Internalize,
    ExternalOnly,
}

#[derive(Clone, Copy, Debug)]
pub struct LinkPolicy {
    pub namespace_discovery: DiscoveryPolicy,
    pub strict: bool,
}

impl Default for LinkPolicy {
    fn default() -> Self {
        Self {
            namespace_discovery: DiscoveryPolicy::Reject,
            strict: true,
        }
    }
}
