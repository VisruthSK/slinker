#[derive(Clone, Copy, Debug)]
pub struct LinkPolicy {
    pub strict: bool,
}

impl Default for LinkPolicy {
    fn default() -> Self {
        Self { strict: true }
    }
}
