//! Cancellation tokens, the yield callback and the safepoint rule shared by
//! the front end and the VM (sections 11.7 and 12.1).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A flag that a host sets, from any thread, to stop an operation. Clones
/// share the flag.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    /// A new token that is not set.
    #[must_use]
    pub fn new() -> CancellationToken {
        CancellationToken::default()
    }

    /// Sets the token. It cannot be cleared.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// Whether the token is set.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

/// What a yield callback answers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum YieldDecision {
    /// Keep running.
    Continue,
    /// Stop the run with the fault HS1006.
    Cancel,
}

/// A callback that is called at every safepoint so that the host can do its
/// own scheduling work, such as checking a wall-clock deadline. It must not
/// call into the instance that is running (HS1204).
pub trait Yield: Send {
    /// Called at a safepoint after the cancellation token was found not set.
    fn on_yield(&mut self) -> YieldDecision;
}

impl<F: FnMut() -> YieldDecision + Send> Yield for F {
    fn on_yield(&mut self) -> YieldDecision {
        self()
    }
}

/// The operation was cancelled, by the token or by the yield callback.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Cancelled;

/// The token and yield callback of one operation.
#[derive(Default)]
pub struct Control<'a> {
    token: Option<&'a CancellationToken>,
    yielder: Option<&'a mut dyn Yield>,
}

impl std::fmt::Debug for Control<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control")
            .field("token", &self.token.is_some())
            .field("yielder", &self.yielder.is_some())
            .finish()
    }
}

impl<'a> Control<'a> {
    /// No token and no callback.
    #[must_use]
    pub fn none() -> Control<'a> {
        Control::default()
    }

    /// A control with the given token and callback.
    #[must_use]
    pub fn new(
        token: Option<&'a CancellationToken>,
        yielder: Option<&'a mut dyn Yield>,
    ) -> Control<'a> {
        Control { token, yielder }
    }

    /// Whether the token is already set. An operation whose token is set when
    /// it starts faults at once with HS1006, before any script code runs.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_some_and(CancellationToken::is_cancelled)
    }

    /// This control's own cancellation token, cloned (a `CancellationToken`
    /// is a cheaply cloned `Arc`), so that something that outlives this
    /// `Control`'s borrow can still see the same flag: a capability call
    /// that reads a file in chunks needs to keep polling for cancellation
    /// between them, but only ever holds a `HostCall`, not this `Control`
    /// itself.
    #[must_use]
    pub fn cancellation_token(&self) -> Option<CancellationToken> {
        self.token.cloned()
    }

    /// The action at a safepoint, in the order of section 11.7: if the token
    /// is set the operation is cancelled, otherwise the yield callback is
    /// called and may cancel.
    ///
    /// # Errors
    ///
    /// Returns [`Cancelled`] when the token is set or the callback answers cancel.
    pub fn safepoint(&mut self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            return Err(Cancelled);
        }
        let decision = self.yielder.as_mut().map(|y| y.on_yield());
        if decision == Some(YieldDecision::Cancel) {
            return Err(Cancelled);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{CancellationToken, Cancelled, Control, YieldDecision};

    #[test]
    fn clones_share_the_flag() {
        let a = CancellationToken::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
    }

    #[test]
    fn the_token_is_tested_before_the_callback() {
        let token = CancellationToken::new();
        let mut calls = 0;
        let mut callback = || {
            calls += 1;
            YieldDecision::Continue
        };
        {
            let mut control = Control::new(Some(&token), Some(&mut callback));
            assert_eq!(control.safepoint(), Ok(()));
            assert_eq!(control.safepoint(), Ok(()));
            token.cancel();
            assert_eq!(control.safepoint(), Err(Cancelled));
        }
        assert_eq!(calls, 2, "the callback is not called once the token is set");
    }

    #[test]
    fn the_callback_can_cancel() {
        let mut n = 0;
        let mut callback = || {
            n += 1;
            if n == 3 {
                YieldDecision::Cancel
            } else {
                YieldDecision::Continue
            }
        };
        let mut control = Control::new(None, Some(&mut callback));
        assert_eq!(control.safepoint(), Ok(()));
        assert_eq!(control.safepoint(), Ok(()));
        assert_eq!(control.safepoint(), Err(Cancelled));
        assert!(!Control::none().is_cancelled());
    }
}
