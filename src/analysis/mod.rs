pub mod diagnostic;
pub mod engine;
pub mod graph;
pub mod need;
pub mod policy;

pub use diagnostic::{Diagnostic, RejectCode};
pub use engine::{LinkPlan, Linker};
pub use graph::{Edge, EdgeKind, Graph, Node, NodeId, NodeKind};
pub use need::{LifecycleId, NativeId, Need, ResourceId, S3Id};
pub use policy::{DiscoveryPolicy, LinkPolicy};

// Compatibility re-exports for callers that used the old module path while the
// linker now owns syntax under `syntax/*`.
pub mod parser {
    pub use crate::syntax::air::*;
}
pub mod source {
    pub use crate::syntax::source::*;
}
