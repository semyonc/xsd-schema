//! Particle occurrence handling with threshold optimization
//!
//! This module implements occurrence constraint handling for XSD particles
//! (minOccurs/maxOccurs) with optimization for large maxOccurs values.
//!
//! Following the Delphi implementation, maxOccurs values greater than
//! MAX_OCCURS_LIMIT are treated as unbounded to avoid NFA state explosion.

use super::fragment::NfaFragment;
use super::nfa::NfaTerm;

/// Maximum maxOccurs value before treating as unbounded.
///
/// Matches the Delphi implementation threshold (MaxOccursLimit = 100).
/// For maxOccurs > 100, the NFA compilation treats it as unbounded
/// to avoid creating excessive states.
pub const MAX_OCCURS_LIMIT: u32 = 100;

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
    /// - The bounded value exceeds MAX_OCCURS_LIMIT
    ///
    /// This optimization prevents NFA state explosion for large maxOccurs.
    pub fn is_effectively_unbounded(&self) -> bool {
        match self {
            MaxOccurs::Unbounded => true,
            MaxOccurs::Bounded(n) => *n > MAX_OCCURS_LIMIT,
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

/// Apply occurrence constraints with threshold optimization.
///
/// For maxOccurs > MAX_OCCURS_LIMIT, treats as unbounded to avoid state explosion.
/// This matches the Delphi implementation behavior.
///
/// # Arguments
///
/// * `frag` - The NFA fragment to apply occurrence constraints to
/// * `min` - Minimum occurrences (minOccurs)
/// * `max` - Maximum occurrences (maxOccurs)
///
/// # Returns
///
/// A new NfaFragment with occurrence constraints applied
pub fn apply_occurs(frag: NfaFragment, min: u32, max: MaxOccurs) -> NfaFragment {
    let effective_max = if max.is_effectively_unbounded() {
        None // Treat as unbounded
    } else {
        max.to_option()
    };

    frag.repeat_range(min, effective_max)
}

/// Counter-based particle matcher for validation.
///
/// Used during validation to track occurrence counts, especially for
/// particles where maxOccurs was treated as unbounded during NFA compilation
/// but strict enforcement is still needed.
///
/// This struct is prepared for Task 4.7 (NFA Validation Helpers).
#[derive(Debug, Clone)]
pub struct CountedParticle {
    /// The term being matched
    pub term: NfaTerm,
    /// Minimum required occurrences
    pub min_occurs: u32,
    /// Maximum allowed occurrences
    pub max_occurs: MaxOccurs,
    /// Current occurrence count
    current_count: u32,
}

impl CountedParticle {
    /// Create a new counted particle
    pub fn new(term: NfaTerm, min_occurs: u32, max_occurs: MaxOccurs) -> Self {
        Self {
            term,
            min_occurs,
            max_occurs,
            current_count: 0,
        }
    }

    /// Check if the particle can accept another occurrence.
    ///
    /// Returns true if we haven't reached the maximum yet.
    pub fn can_accept(&self) -> bool {
        match self.max_occurs {
            MaxOccurs::Unbounded => true,
            MaxOccurs::Bounded(max) => self.current_count < max,
        }
    }

    /// Check if the minimum occurrence requirement is satisfied.
    ///
    /// Returns true if we've matched at least minOccurs times.
    pub fn is_satisfied(&self) -> bool {
        self.current_count >= self.min_occurs
    }

    /// Accept one occurrence, incrementing the counter.
    pub fn accept(&mut self) {
        self.current_count += 1;
    }

    /// Get the current occurrence count.
    pub fn count(&self) -> u32 {
        self.current_count
    }

    /// Reset the counter for a new validation run.
    pub fn reset(&mut self) {
        self.current_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::NameId;

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
        // Below threshold - NOT effectively unbounded
        assert!(!MaxOccurs::Bounded(50).is_effectively_unbounded());
        assert!(!MaxOccurs::Bounded(100).is_effectively_unbounded());

        // Above threshold - effectively unbounded
        assert!(MaxOccurs::Bounded(101).is_effectively_unbounded());
        assert!(MaxOccurs::Bounded(1000).is_effectively_unbounded());

        // Explicitly unbounded
        assert!(MaxOccurs::Unbounded.is_effectively_unbounded());
    }

    #[test]
    fn test_max_occurs_default() {
        assert_eq!(MaxOccurs::default(), MaxOccurs::Bounded(1));
    }

    #[test]
    fn test_counted_particle_new() {
        let term = NfaTerm::element(NameId(1), None, None);
        let cp = CountedParticle::new(term, 2, MaxOccurs::Bounded(5));

        assert_eq!(cp.min_occurs, 2);
        assert_eq!(cp.max_occurs, MaxOccurs::Bounded(5));
        assert_eq!(cp.count(), 0);
    }

    #[test]
    fn test_counted_particle_can_accept() {
        let term = NfaTerm::element(NameId(1), None, None);
        let mut cp = CountedParticle::new(term, 1, MaxOccurs::Bounded(3));

        assert!(cp.can_accept()); // 0 < 3
        cp.accept();
        assert!(cp.can_accept()); // 1 < 3
        cp.accept();
        assert!(cp.can_accept()); // 2 < 3
        cp.accept();
        assert!(!cp.can_accept()); // 3 >= 3
    }

    #[test]
    fn test_counted_particle_unbounded_always_accepts() {
        let term = NfaTerm::element(NameId(1), None, None);
        let mut cp = CountedParticle::new(term, 1, MaxOccurs::Unbounded);

        for _ in 0..1000 {
            assert!(cp.can_accept());
            cp.accept();
        }
        assert!(cp.can_accept()); // Still accepting
    }

    #[test]
    fn test_counted_particle_is_satisfied() {
        let term = NfaTerm::element(NameId(1), None, None);
        let mut cp = CountedParticle::new(term, 2, MaxOccurs::Bounded(5));

        assert!(!cp.is_satisfied()); // 0 < 2
        cp.accept();
        assert!(!cp.is_satisfied()); // 1 < 2
        cp.accept();
        assert!(cp.is_satisfied()); // 2 >= 2
        cp.accept();
        assert!(cp.is_satisfied()); // 3 >= 2
    }

    #[test]
    fn test_counted_particle_reset() {
        let term = NfaTerm::element(NameId(1), None, None);
        let mut cp = CountedParticle::new(term, 2, MaxOccurs::Bounded(5));

        cp.accept();
        cp.accept();
        assert_eq!(cp.count(), 2);

        cp.reset();
        assert_eq!(cp.count(), 0);
        assert!(!cp.is_satisfied());
    }
}
