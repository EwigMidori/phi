//! Stop-generation numbers for cancel fencing on a [`super::SendQueue`].

/// Opaque monotonic stop-generation number.
///
/// Claim snapshots one; stop bumps the live value; a snapshot that no longer
/// matches live means that claim is void.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Epoch(u64);

impl Epoch {
    pub(crate) const ZERO: Self = Self(0);

    #[must_use]
    pub(crate) fn bump(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    #[must_use]
    pub(crate) fn is_stale(self, live: Epoch) -> bool {
        self != live
    }
}
