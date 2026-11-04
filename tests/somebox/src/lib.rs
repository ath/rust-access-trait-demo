//! Pointer and lock types of a third-party crate that knows nothing about
//! `access`, in the style of `smox::SmoxBox`, `triomphe::Arc` and
//! `parking_lot::Mutex`.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

/// An owning pointer.
#[derive(Clone)]
pub struct BoxLikeThing<T: ?Sized>(Box<T>);

impl<T> BoxLikeThing<T> {
    pub fn new(value: T) -> Self {
        Self(Box::new(value))
    }
}

impl<T: ?Sized> Deref for BoxLikeThing<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> DerefMut for BoxLikeThing<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// A reference-counted pointer.
pub struct ArcLikeThing<T: ?Sized>(Arc<T>);

impl<T> ArcLikeThing<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T: ?Sized> ArcLikeThing<T> {
    /// Returns exclusive access if no other pointer shares the value.
    pub fn get_mut(this: &mut Self) -> Option<&mut T> {
        Arc::get_mut(&mut this.0)
    }
}

impl<T: ?Sized> Clone for ArcLikeThing<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: ?Sized> Deref for ArcLikeThing<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// A mutual exclusion lock that can wait with a timeout and never poisons.
pub struct LockLikeThing<T: ?Sized>(Mutex<T>);

impl<T> LockLikeThing<T> {
    pub fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }
}

impl<T: ?Sized> LockLikeThing<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        match self.0.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    pub fn try_lock_for(&self, timeout: Duration) -> Option<MutexGuard<'_, T>> {
        match Instant::now().checked_add(timeout) {
            Some(deadline) => self.try_lock_until(deadline),
            None => Some(self.lock()),
        }
    }

    pub fn try_lock_until(&self, deadline: Instant) -> Option<MutexGuard<'_, T>> {
        loop {
            if let Some(guard) = self.try_lock() {
                return Some(guard);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().unwrap_or_else(PoisonError::into_inner)
    }
}
