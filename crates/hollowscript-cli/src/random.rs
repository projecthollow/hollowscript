//! The `random` capability's generator (section 16.4).
//!
//! The specification does not define an algorithm, does not promise
//! cryptographic strength, and does not promise the same sequence on
//! different hosts, so a small, fast, dependency-free generator is enough.
//! This is [SplitMix64](https://xoshiro.di.unimi.it/splitmix64.c) (public
//! domain), seeded by [`Rng::from_entropy`] from process and timing state
//! that is not itself cryptographically strong. A host that must be
//! reproducible constructs a [`Rng`] with [`Rng::from_seed`] instead (as the
//! conformance corpus does, with a fixed seed and its own fixed sequence,
//! per `corpus/FORMAT.md`).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "bit-level mixing and range reduction on generator output; not a value the language observes directly"
)]

/// A `SplitMix64` generator.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rng {
    state: u64,
}

impl Rng {
    /// A generator seeded exactly by `seed`, for a reproducible sequence.
    #[must_use]
    pub(crate) fn from_seed(seed: u64) -> Rng {
        Rng { state: seed }
    }

    /// A generator seeded from the process's own state: the wall clock, the
    /// process id, a stack address and a call counter, mixed together. The
    /// counter is what keeps two calls made within one tick of a
    /// coarse-grained system clock from seeding identically; the rest add
    /// variety across processes and runs. Not cryptographically strong, and
    /// not meant to be: this is what the specification calls "the host's own" generator.
    #[must_use]
    pub(crate) fn from_entropy() -> Rng {
        static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let pid = u64::from(std::process::id());
        let local = 0u8;
        let addr = std::ptr::addr_of!(local) as u64;
        Rng::from_seed(
            wall ^ pid.wrapping_shl(32) ^ addr ^ call.wrapping_mul(0x9E37_79B9_7F4A_7C15),
        )
    }

    /// The next 64 bits of the stream.
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `random.int(min, upTo)`: an integer `n` with `min <= n < upTo`, each
    /// value equally likely, by Lemire's method (no modulo bias).
    ///
    /// # Panics
    ///
    /// If `min >= upTo`, which the caller (the capability wrapper) must
    /// refuse with HS1011 before this is reached, per section 16.4.
    pub(crate) fn int_range(&mut self, min: i64, up_to: i64) -> i64 {
        assert!(min < up_to, "random.int needs min < upTo");
        let range = up_to.wrapping_sub(min) as u64;
        min.wrapping_add(self.bounded_u64(range) as i64)
    }

    fn bounded_u64(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let r = self.next_u64();
            if r >= threshold {
                return r % bound;
            }
        }
    }

    /// `random.float()`: a `Float` `x` with `0.0 <= x < 1.0`, uniformly
    /// distributed over 2^53 equally spaced values, the same construction
    /// most language runtimes use for a double in this range.
    pub(crate) fn float01(&mut self) -> f64 {
        let bits = self.next_u64() >> 11; // 53 significant bits
        (bits as f64) * (1.0 / (1u64 << 53) as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn a_seeded_generator_is_reproducible() {
        let mut a = Rng::from_seed(42);
        let mut b = Rng::from_seed(42);
        let seq_a: Vec<u64> = (0..5).map(|_| a.next_u64()).collect();
        let seq_b: Vec<u64> = (0..5).map(|_| b.next_u64()).collect();
        assert_eq!(seq_a, seq_b);
    }

    #[test]
    fn int_range_stays_in_bounds_and_covers_the_range() {
        let mut rng = Rng::from_seed(7);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..2000 {
            let n = rng.int_range(3, 8);
            assert!((3..8).contains(&n));
            seen.insert(n);
        }
        assert_eq!(seen, (3..8).collect());
    }

    #[test]
    fn float01_is_in_the_half_open_unit_interval() {
        let mut rng = Rng::from_seed(1);
        for _ in 0..1000 {
            let x = rng.float01();
            assert!((0.0..1.0).contains(&x));
        }
    }

    #[test]
    fn entropy_seeded_generators_usually_differ() {
        let a = Rng::from_entropy().next_u64();
        let b = Rng::from_entropy().next_u64();
        // Not a correctness property (they could coincide), but a sanity
        // check that entropy seeding is not a constant.
        assert_ne!(a, b);
    }
}
