//! Uniform access to a value of type `T`, whether it is stored directly or
//! behind any nesting of smart pointers (`Box`, `Rc`, `Arc`, `&`, `&mut`) and
//! locks or cells (`Mutex`, `RwLock`, `RefCell`).
//!
//! Generic code names the value type `T` and keeps the path `P` generic. The
//! compiler infers `P` from the concrete container type:
//!
//! ```
//! # use std::{cell::RefCell, rc::Rc, sync::{Arc, Mutex, RwLock}};
//! use access::{Access, AccessMut};
//!
//! fn sum<P>(numbers: &impl Access<Vec<u32>, P>) -> u32 {
//!     numbers.get_ref().iter().sum()
//! }
//!
//! fn push<P>(numbers: &mut impl AccessMut<Vec<u32>, P>, number: u32) {
//!     numbers.get_mut().push(number);
//! }
//!
//! let mut plain = vec![1];
//! let mut shared = Arc::new(Mutex::new(vec![1]));
//! let mut boxed = Mutex::new(Box::new(vec![1]));
//! let mut cell = Rc::new(RefCell::new(vec![1]));
//! let mut locked = RwLock::new(vec![1]);
//!
//! push(&mut plain, 2);
//! push(&mut shared, 2);
//! push(&mut boxed, 2);
//! push(&mut cell, 2);
//! push(&mut locked, 2);
//!
//! assert_eq!(sum(&plain) + sum(&shared) + sum(&boxed), 9);
//! assert_eq!(sum(&cell) + sum(&locked), 6);
//! assert_eq!(sum(&Rc::new(vec![1, 2])), 3); // shared access only
//! ```
//!
//! Composition rules:
//!
//! - A pointer may contain anything, including further pointers and locks.
//! - A lock may contain the value directly or behind pointers, but no further
//!   lock: the inner guard would have to borrow from the outer one.
//! - Exclusive access through a shared pointer (`Rc`, `Arc`, `&`) needs a lock
//!   further inside.
//!
//! New pointer types implement [`SharedPointer`] or [`OwningPointer`], new
//! locks and cells implement [`Lock`]. They then compose with all others.

use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::{Arc, LockResult, Mutex, RwLock};

// User side ------------------------------------------------------------------

/// Shared access to a `T` stored in `Self`.
///
/// `P` is the path from `Self` to `T`. It is always inferred: generic code
/// declares it as an extra type parameter and never names it.
pub trait Access<T: ?Sized, P> {
    /// Returns a reference or guard that dereferences to the value.
    ///
    /// # Panics
    ///
    /// Panics if a lock on the path is poisoned or a `RefCell` on the path is
    /// mutably borrowed.
    fn get_ref(&self) -> impl Deref<Target = T>;
}

/// Exclusive access to a `T` stored in `Self`.
pub trait AccessMut<T: ?Sized, P>: Access<T, P> {
    /// Returns a reference or guard that mutably dereferences to the value.
    ///
    /// A lock owned by `Self` is bypassed through its `get_mut`; a lock behind
    /// a shared pointer is acquired.
    ///
    /// # Panics
    ///
    /// Panics if a lock on the path is poisoned or a `RefCell` on the path is
    /// borrowed.
    fn get_mut(&mut self) -> impl DerefMut<Target = T>;
}

/// Exclusive access through a shared reference, available whenever a lock
/// lies on the path, as in `Arc<Mutex<T>>` or `&RefCell<T>`.
pub trait SharedMut<T: ?Sized, P>: Access<T, P> {
    /// Acquires the lock on the path for writing.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned or the `RefCell` is borrowed.
    fn lock_mut(&self) -> impl DerefMut<Target = T>;
}

// Extension points -----------------------------------------------------------

/// A pointer that hands out shared references only, like `Rc`, `Arc` and
/// `&T`.
pub trait SharedPointer: Deref {}

/// A pointer that owns its target exclusively, like `Box` and `&mut T`.
pub trait OwningPointer: DerefMut {}

/// A lock or cell with interior mutability, like `Mutex`, `RwLock` and
/// `RefCell`.
pub trait Lock {
    /// The protected value.
    type Inner: ?Sized;

    /// Acquires shared access.
    fn read(&self) -> impl Deref<Target = Self::Inner>;

    /// Acquires exclusive access.
    fn write(&self) -> impl DerefMut<Target = Self::Inner>;

    /// Grants exclusive access without locking, since the exclusive borrow of
    /// the lock already rules out any other user.
    fn inner_mut(&mut self) -> &mut Self::Inner;
}

impl<T: ?Sized> SharedPointer for &T {}
impl<T: ?Sized> SharedPointer for Rc<T> {}
impl<T: ?Sized> SharedPointer for Arc<T> {}

impl<T: ?Sized> OwningPointer for &mut T {}
impl<T: ?Sized> OwningPointer for Box<T> {}

impl<T: ?Sized> Lock for Mutex<T> {
    type Inner = T;

    fn read(&self) -> impl Deref<Target = T> {
        unpoisoned(Mutex::lock(self))
    }

    fn write(&self) -> impl DerefMut<Target = T> {
        unpoisoned(Mutex::lock(self))
    }

    fn inner_mut(&mut self) -> &mut T {
        unpoisoned(Mutex::get_mut(self))
    }
}

impl<T: ?Sized> Lock for RwLock<T> {
    type Inner = T;

    fn read(&self) -> impl Deref<Target = T> {
        unpoisoned(RwLock::read(self))
    }

    fn write(&self) -> impl DerefMut<Target = T> {
        unpoisoned(RwLock::write(self))
    }

    fn inner_mut(&mut self) -> &mut T {
        unpoisoned(RwLock::get_mut(self))
    }
}

impl<T: ?Sized> Lock for RefCell<T> {
    type Inner = T;

    fn read(&self) -> impl Deref<Target = T> {
        RefCell::borrow(self)
    }

    fn write(&self) -> impl DerefMut<Target = T> {
        RefCell::borrow_mut(self)
    }

    fn inner_mut(&mut self) -> &mut T {
        RefCell::get_mut(self)
    }
}

/// Propagates a panic that happened while the lock was held, like the usual
/// `lock().unwrap()`. Use `unwrap_or_else(PoisonError::into_inner)` to ignore
/// poisoning instead.
fn unpoisoned<G>(result: LockResult<G>) -> G {
    result.expect("lock poisoned by a panic while it was held")
}

// Machinery ------------------------------------------------------------------

/// Path markers, guard-free projections and the blanket implementations that
/// compose the layers. The items are `pub` for the trait system only; the
/// private module keeps them unreachable from outside.
mod internal {
    use std::marker::PhantomData;
    use std::ops::{Deref, DerefMut};

    use super::{Access, AccessMut, Lock, OwningPointer, SharedMut, SharedPointer};

    /// Path to the value itself.
    pub struct Here;

    /// Path through a [`SharedPointer`], then along `P`.
    pub struct ViaShared<P>(PhantomData<P>);

    /// Path through an [`OwningPointer`], then along `P`.
    pub struct ViaOwned<P>(PhantomData<P>);

    /// Path through a [`Lock`], then along `P`.
    pub struct ViaLock<P>(PhantomData<P>);

    /// Shared access without a guard. A lock needs it to reach the value from
    /// inside its own guard.
    pub trait Project<T: ?Sized, P> {
        fn project(&self) -> &T;
    }

    /// Exclusive access without a guard.
    pub trait ProjectMut<T: ?Sized, P>: Project<T, P> {
        fn project_mut(&mut self) -> &mut T;
    }

    /// A lock guard that dereferences to the value inside the lock.
    struct Projected<G, T: ?Sized, P> {
        guard: G,
        path: PhantomData<fn() -> (P, *const T)>,
    }

    impl<G, T: ?Sized, P> Projected<G, T, P> {
        fn new(guard: G) -> Self {
            Self {
                guard,
                path: PhantomData,
            }
        }
    }

    impl<G, T, P> Deref for Projected<G, T, P>
    where
        G: Deref,
        G::Target: Project<T, P>,
        T: ?Sized,
    {
        type Target = T;

        fn deref(&self) -> &T {
            Project::<T, P>::project(&*self.guard)
        }
    }

    impl<G, T, P> DerefMut for Projected<G, T, P>
    where
        G: DerefMut,
        G::Target: ProjectMut<T, P>,
        T: ?Sized,
    {
        fn deref_mut(&mut self) -> &mut T {
            ProjectMut::<T, P>::project_mut(&mut *self.guard)
        }
    }

    // The value itself.

    impl<T: ?Sized> Access<T, Here> for T {
        fn get_ref(&self) -> impl Deref<Target = T> {
            self
        }
    }

    impl<T: ?Sized> AccessMut<T, Here> for T {
        fn get_mut(&mut self) -> impl DerefMut<Target = T> {
            self
        }
    }

    impl<T: ?Sized> Project<T, Here> for T {
        fn project(&self) -> &T {
            self
        }
    }

    impl<T: ?Sized> ProjectMut<T, Here> for T {
        fn project_mut(&mut self) -> &mut T {
            self
        }
    }

    // Shared pointers: shared access passes through, exclusive access needs a
    // lock further inside.

    impl<X, T, P> Access<T, ViaShared<P>> for X
    where
        X: SharedPointer + ?Sized,
        X::Target: Access<T, P>,
        T: ?Sized,
    {
        fn get_ref(&self) -> impl Deref<Target = T> {
            Access::<T, P>::get_ref(&**self)
        }
    }

    impl<X, T, P> AccessMut<T, ViaShared<P>> for X
    where
        X: SharedPointer + ?Sized,
        X::Target: SharedMut<T, P>,
        T: ?Sized,
    {
        fn get_mut(&mut self) -> impl DerefMut<Target = T> {
            SharedMut::<T, P>::lock_mut(&**self)
        }
    }

    impl<X, T, P> SharedMut<T, ViaShared<P>> for X
    where
        X: SharedPointer + ?Sized,
        X::Target: SharedMut<T, P>,
        T: ?Sized,
    {
        fn lock_mut(&self) -> impl DerefMut<Target = T> {
            SharedMut::<T, P>::lock_mut(&**self)
        }
    }

    impl<X, T, P> Project<T, ViaShared<P>> for X
    where
        X: SharedPointer + ?Sized,
        X::Target: Project<T, P>,
        T: ?Sized,
    {
        fn project(&self) -> &T {
            Project::<T, P>::project(&**self)
        }
    }

    // Owning pointers: every kind of access passes through.

    impl<X, T, P> Access<T, ViaOwned<P>> for X
    where
        X: OwningPointer + ?Sized,
        X::Target: Access<T, P>,
        T: ?Sized,
    {
        fn get_ref(&self) -> impl Deref<Target = T> {
            Access::<T, P>::get_ref(&**self)
        }
    }

    impl<X, T, P> AccessMut<T, ViaOwned<P>> for X
    where
        X: OwningPointer + ?Sized,
        X::Target: AccessMut<T, P>,
        T: ?Sized,
    {
        fn get_mut(&mut self) -> impl DerefMut<Target = T> {
            AccessMut::<T, P>::get_mut(&mut **self)
        }
    }

    impl<X, T, P> SharedMut<T, ViaOwned<P>> for X
    where
        X: OwningPointer + ?Sized,
        X::Target: SharedMut<T, P>,
        T: ?Sized,
    {
        fn lock_mut(&self) -> impl DerefMut<Target = T> {
            SharedMut::<T, P>::lock_mut(&**self)
        }
    }

    impl<X, T, P> Project<T, ViaOwned<P>> for X
    where
        X: OwningPointer + ?Sized,
        X::Target: Project<T, P>,
        T: ?Sized,
    {
        fn project(&self) -> &T {
            Project::<T, P>::project(&**self)
        }
    }

    impl<X, T, P> ProjectMut<T, ViaOwned<P>> for X
    where
        X: OwningPointer + ?Sized,
        X::Target: ProjectMut<T, P>,
        T: ?Sized,
    {
        fn project_mut(&mut self) -> &mut T {
            ProjectMut::<T, P>::project_mut(&mut **self)
        }
    }

    // Locks: the guard is projected to the value, so the content must be
    // reachable without a guard of its own.

    impl<L, T, P> Access<T, ViaLock<P>> for L
    where
        L: Lock + ?Sized,
        L::Inner: Project<T, P>,
        T: ?Sized,
    {
        fn get_ref(&self) -> impl Deref<Target = T> {
            Projected::<_, T, P>::new(Lock::read(self))
        }
    }

    impl<L, T, P> AccessMut<T, ViaLock<P>> for L
    where
        L: Lock + ?Sized,
        L::Inner: ProjectMut<T, P>,
        T: ?Sized,
    {
        fn get_mut(&mut self) -> impl DerefMut<Target = T> {
            // A plain `&mut T` would require `T: 'a`, which the trait cannot
            // promise; projecting on each dereference avoids that bound.
            Projected::<_, T, P>::new(Lock::inner_mut(self))
        }
    }

    impl<L, T, P> SharedMut<T, ViaLock<P>> for L
    where
        L: Lock + ?Sized,
        L::Inner: ProjectMut<T, P>,
        T: ?Sized,
    {
        fn lock_mut(&self) -> impl DerefMut<Target = T> {
            Projected::<_, T, P>::new(Lock::write(self))
        }
    }
}
