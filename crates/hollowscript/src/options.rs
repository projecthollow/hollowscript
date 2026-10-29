//! What a host supplies for a run (section 9.7 and chapter 12's `options`).

pub use hollowscript_check::CapabilityInterface;
use hollowscript_core::Limits;

/// Why a [`Loader`] could not produce a module's source: [`hollowscript_core::LoadModuleError`]
/// itself, re-exported here under the name this module's documentation has
/// always used it by, so that a [`Loader`] and [`hollowscript_check::check`]'s
/// `load_module` callback share exactly one type. Before this was a re-export,
/// the two disagreed on what a loader failure carries (this type had no
/// `{reason}` for HS0607, and `check`'s own callback signature could not have
/// told a not-found from a failed-to-read import either way), so a host's
/// `Loader::Failed` reason had nowhere to go once it reached the checker.
pub use hollowscript_core::LoadModuleError as LoaderError;

/// A host-supplied loader (section 10.3): resolves an import's canonical path
/// to source text, or reports why it could not.
///
/// `Send`: checking a module's imports (`load_module`, below) happens on
/// [`hollowscript_core::run_on_dedicated_stack`]'s dedicated thread, along
/// with the rest of `check` and `compile`, so that recursion within the
/// structural limits of section 1.10 cannot exhaust a small caller stack
/// (see [`crate::instance::check`]'s own documentation). A loader crossing
/// onto that thread must itself be safe to send there.
pub trait Loader: Send {
    /// The source of the module at `canonical_path`.
    ///
    /// # Errors
    ///
    /// Returns [`LoaderError::NotFound`] when the loader has nothing at that
    /// path (HS0601), or [`LoaderError::Failed`] with the reason when it
    /// exists but could not be read (HS0607).
    fn load(&mut self, canonical_path: &str) -> Result<Vec<u8>, LoaderError>;
}

/// A loader that has nothing: every import fails with
/// [`LoaderError::NotFound`]. Suitable for a single-file program.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoLoader;

impl Loader for NoLoader {
    fn load(&mut self, _canonical_path: &str) -> Result<Vec<u8>, LoaderError> {
        Err(LoaderError::NotFound)
    }
}

/// The options of one run (section 9.7 and chapter 12): the resource limits
/// and the capability interface. The loader and the capability function
/// bindings are supplied separately to [`crate::load`], because a loader
/// often borrows the host's own state and a capability table cannot be
/// shared between instances (it owns boxed closures).
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The resource limits of chapter 11. [`Limits::default`] gives the
    /// specification's own defaults.
    pub limits: Limits,
    /// The capabilities this run offers, with their types (section 9.6).
    pub capabilities: CapabilityInterface,
    /// The host's filter schema (section 9.7, item 2), if it has one, used
    /// to validate a filter literal against it (Appendix A, section A.7) as
    /// well as parse it. `None`, the default, checks only what sections A.2
    /// to A.4 need no schema for.
    pub filter_schema: Option<hollowscript_core::filter::FilterSchema>,
    /// Whether capability calls are batched instead of called synchronously
    /// (see `docs/design/capabilities.md`). A frame-based host, a game
    /// engine that owns the thread between `update` and `draw` and must not
    /// be re-entered from the middle of a script, sets this: a capability
    /// call then queues a [`hollowscript_vm::Command`] and the run pauses,
    /// and the host performs the queued calls at its own frame boundary and
    /// continues with [`crate::Instance::resume`]. `false`, the default, is
    /// the synchronous path, where a capability call runs the host's bound
    /// function there and then.
    pub batch_capability_calls: bool,
    /// Forces a collection at every safepoint this run reaches, including
    /// during the load itself, instead of only when the heap has grown past
    /// its own trigger threshold (`docs/design/performance.md`, "collection
    /// policy"). `false`, the default, is the normal policy. This exists for
    /// the stress-test mode that `crates/hollowscript-conformance` and
    /// `scripts/check-gc-stress` run the corpus and the specification's
    /// examples under, to prove that collecting as often as possible never
    /// changes a program's observable result; an ordinary host has no
    /// reason to set it, since it makes every run far slower.
    pub gc_stress_mode: bool,
}

#[cfg(test)]
mod tests {
    use super::{Loader, LoaderError, NoLoader};

    #[test]
    fn the_no_loader_refuses_every_import() {
        let mut loader = NoLoader;
        assert_eq!(loader.load("./util"), Err(LoaderError::NotFound));
    }
}
