use std::sync::{
  Arc,
  atomic::{AtomicBool, Ordering},
};

use crate::{RawlerError, Result};

/// Cheap, cloneable cooperative cancellation shared by callers and RAW work.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn from_atomic(flag: Arc<AtomicBool>) -> Self {
    Self(flag)
  }

  pub fn cancel(&self) {
    self.0.store(true, Ordering::Release);
  }

  pub fn is_cancelled(&self) -> bool {
    self.0.load(Ordering::Acquire)
  }

  pub fn check(&self) -> Result<()> {
    if self.is_cancelled() {
      Err(RawlerError::DecoderFailed("decode cancelled".into()))
    } else {
      Ok(())
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn clones_observe_cancellation() {
    let token = CancellationToken::new();
    let worker = token.clone();
    assert!(worker.check().is_ok());
    token.cancel();
    assert!(worker.is_cancelled());
    assert!(worker.check().is_err());
  }
}
