//! Particle occurrence handling with threshold optimization
//!
//! This module implements occurrence constraint handling for XSD particles
//! (minOccurs/maxOccurs) with optimization for large maxOccurs values.
//!
//! For small maxOccurs (≤ COUNTED_THRESHOLD), NFA fragments are unrolled
//! (cloned). For larger values (up to MAX_COUNTED_OCCURS), counted NFA
//! transitions are used for O(1) state overhead. Values above
//! MAX_COUNTED_OCCURS are treated as unbounded.

use super::fragment::NfaFragment;

/// Threshold above which counted NFA is used instead of unrolling.
///
/// Values ≤ this threshold are unrolled (cloned fragments).
/// Values above use counted transitions with O(1) extra states.
pub const COUNTED_THRESHOLD: u32 = 16;

/// Maximum maxOccurs value before treating as unbounded.
///
/// Values above this cap fall back to unbounded treatment to bound the
/// O(maxOccurs) cost of epsilon closure for nullable loop bodies.
/// Raises the correctness ceiling from the old limit of 100 to 10000.
pub const MAX_COUNTED_OCCURS: u32 = 10_000;

/// MaxOccurs value representation
///
/// Represents the maxOccurs constraint from XSD, which can be either
/// a bounded positive integer or unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxOccurs {
    /// Unbounded (no maximum limit)
    Unbounded,
    /// Bounded to a specific maximum value
    Bounded(u32),
}

impl MaxOccurs {
    /// Create from an Option, where None means unbounded
    pub fn from_option(max: Option<u32>) -> Self {
        match max {
            Some(n) => MaxOccurs::Bounded(n),
            None => MaxOccurs::Unbounded,
        }
    }

    /// Convert to Option for compatibility with fragment methods
    pub fn to_option(&self) -> Option<u32> {
        match self {
            MaxOccurs::Unbounded => None,
            MaxOccurs::Bounded(n) => Some(*n),
        }
    }

    /// Check if this value is effectively unbounded.
    ///
    /// Returns true if:
    /// - The value is explicitly Unbounded, or
    /// - The bounded value exceeds MAX_COUNTED_OCCURS
    ///
    /// Values up to MAX_COUNTED_OCCURS are handled exactly by counted NFA.
    /// Values above fall back to unbounded to bound runtime cost.
    pub fn is_effectively_unbounded(&self) -> bool {
        match self {
            MaxOccurs::Unbounded => true,
            MaxOccurs::Bounded(n) => *n > MAX_COUNTED_OCCURS,
        }
    }

    /// Check if this is explicitly unbounded
    pub fn is_unbounded(&self) -> bool {
        matches!(self, MaxOccurs::Unbounded)
    }
}

impl Default for MaxOccurs {
    fn default() -> Self {
        MaxOccurs::Bounded(1)
    }
}

/// Apply occurrence constraints with threshold-based dispatch.
///
/// - Small bounded (≤ COUNTED_THRESHOLD): unroll via repeat_range
/// - Large bounded (≤ MAX_COUNTED_OCCURS): counted NFA via repeat_counted
/// - Unbounded or > MAX_COUNTED_OCCURS: Kleene star via repeat_range
pub fn apply_occurs(frag: NfaFragment, min: u32, max: MaxOccurs) -> NfaFragment {
    let effective_max = if max.is_effectively_unbounded() {
        None // Treat as unbounded
    } else {
        max.to_option()
    };

    match effective_max {
        // Unbounded with large min → counted exact prefix + star tail
        None if min > COUNTED_THRESHOLD => {
            frag.clone().repeat_counted(min, min).concat(frag.repeat_star())
        }
        // Unbounded with small min → existing unroll via repeat_range (star/plus)
        None => frag.repeat_range(min, None),
        // Small bounded → existing unroll via repeat_range
        Some(m) if m <= COUNTED_THRESHOLD => frag.repeat_range(min, Some(m)),
        // Large bounded → counted construction
        Some(m) if min == 0 => frag.repeat_counted(0, m),
        Some(m) if min <= COUNTED_THRESHOLD => {
            frag.clone().repeat_exact(min).concat(frag.repeat_counted(0, m - min))
        }
        Some(m) => frag.repeat_counted(min, m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_occurs_from_option() {
        assert_eq!(MaxOccurs::from_option(Some(5)), MaxOccurs::Bounded(5));
        assert_eq!(MaxOccurs::from_option(None), MaxOccurs::Unbounded);
    }

    #[test]
    fn test_max_occurs_to_option() {
        assert_eq!(MaxOccurs::Bounded(5).to_option(), Some(5));
        assert_eq!(MaxOccurs::Unbounded.to_option(), None);
    }

    #[test]
    fn test_max_occurs_effectively_unbounded() {
        // Below MAX_COUNTED_OCCURS - NOT effectively unbounded
        assert!(!MaxOccurs::Bounded(50).is_effectively_unbounded());
        assert!(!MaxOccurs::Bounded(100).is_effectively_unbounded());
        assert!(!MaxOccurs::Bounded(1000).is_effectively_unbounded());
        assert!(!MaxOccurs::Bounded(MAX_COUNTED_OCCURS).is_effectively_unbounded());

        // Above MAX_COUNTED_OCCURS - effectively unbounded
        assert!(MaxOccurs::Bounded(MAX_COUNTED_OCCURS + 1).is_effectively_unbounded());

        // Explicitly unbounded
        assert!(MaxOccurs::Unbounded.is_effectively_unbounded());
    }

    #[test]
    fn test_max_occurs_default() {
        assert_eq!(MaxOccurs::default(), MaxOccurs::Bounded(1));
    }
}
