//! Uniform, fallible access to a value of type `T`, whether it is stored
//! directly or behind any nesting of smart pointers (`Box`, `Rc`, `Arc`, `&`,
//! `&mut`) and locks or cells (`Mutex`, `RwLock`, `RefCell`).
//!
//! Generic code names the value type `T` and keeps the path `P` generic. The
//! compiler infers `P` from the concrete container type:
//!
//! ```
//! # use std::{cell::RefCell, rc::Rc, sync::{Arc, Mutex, RwLock}};
//! use access::{Access, AccessError, AccessMut};
//!
//! fn sum<P>(numbers: &impl Access<Vec<u32>, P>) -> u32 {
//!     numbers.must_get_ref().iter().sum()
//! }
//!
//! fn push<P>(numbers: &mut impl AccessMut<Vec<u32>, P>, number: u32) {
//!     numbers.must_get_mut().push(number);
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
//!
//! // Without a lock inside, a shared pointer grants exclusive access only
//! // while it is the only pointer to its target.
//! fn try_push<P>(
//!     numbers: &mut impl AccessMut<Vec<u32>, P>,
//!     number: u32,
//! ) -> Result<(), AccessError> {
//!     numbers.try_get_mut()?.push(number);
//!     Ok(())
//! }
//!
//! let mut counted = Rc::new(vec![1]);
//! let other = Rc::clone(&counted);
//! assert_eq!(try_push(&mut counted, 2), Err(AccessError::IsShared));
//! drop(other);
//! assert_eq!(try_push(&mut counted, 2), Ok(()));
//! ```
//!
//! # Method families
//!
//! Every access trait offers four methods: `try_*_locked` takes a
//! [`LockMethod`] and returns a [`Result`]; `try_*` uses [`Block`];
//! `must_*_locked` and `must_*` panic instead of returning an error.
//!
//! # Composition rules
//!
//! - A pointer may contain anything, including further pointers and locks.
//! - A lock may contain the value directly or behind pointers, but no further
//!   lock: the inner guard would have to borrow from the outer one.
//! - Exclusive access through a shared pointer either goes through a lock
//!   further inside or requires the pointer to be unique ([`RefCounted`]).
//!
//! # Extension
//!
//! New pointer types implement [`SharedPointer`] (plus [`RefCounted`]) or
//! [`OwningPointer`], new locks and cells implement [`Lock`]. They then
//! compose with all others. Types from other crates join through a local
//! [`TraitGlue`] type.

use std::cell::{BorrowError, BorrowMutError, RefCell};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError, RwLock, TryLockError};
use std::time::{Duration, Instant};

use unsupported::unsupported;

// Errors ---------------------------------------------------------------------

/// Generalizes errors returned by access primitives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccessError {
    /// The underlying data cannot be obtained because it is already shared
    /// and one tried to get an exclusive reference.
    IsShared,
    /// The underlying data cannot be obtained because it is already locked
    /// and would block, either instantly or by timeout.
    WouldBlock,
    /// The underlying cell is poisoned.
    Poisoned,
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::IsShared => "the value is shared and cannot be accessed exclusively",
            Self::WouldBlock => "the value is locked and access would block",
            Self::Poisoned => "the lock is poisoned",
        })
    }
}

impl std::error::Error for AccessError {}

impl<Guard> From<PoisonError<Guard>> for AccessError {
    fn from(_: PoisonError<Guard>) -> Self {
        Self::Poisoned
    }
}

impl<Guard> From<TryLockError<Guard>> for AccessError {
    fn from(error: TryLockError<Guard>) -> Self {
        match error {
            TryLockError::Poisoned(_) => Self::Poisoned,
            TryLockError::WouldBlock => Self::WouldBlock,
        }
    }
}

impl From<BorrowError> for AccessError {
    fn from(_: BorrowError) -> Self {
        Self::WouldBlock
    }
}

impl From<BorrowMutError> for AccessError {
    fn from(_: BorrowMutError) -> Self {
        Self::WouldBlock
    }
}

/// Unwraps the result of an access for the `must_*` methods.
#[track_caller]
fn must<V>(result: Result<V, AccessError>) -> V {
    match result {
        Ok(value) => value,
        Err(error) => panic!("access failed: {error}"),
    }
}

// Lock methods ---------------------------------------------------------------

/// Waits as long as necessary. All methods without a `_locked` suffix use it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Block;

/// Does not wait: fails with [`AccessError::WouldBlock`] if the lock is taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TryLock;

/// How the locks on the path are acquired: [`Block`], [`TryLock`], a
/// [`Duration`] as timeout or an [`Instant`] as deadline.
///
/// A lock method is an upper bound for waiting. A lock may give up earlier –
/// `RefCell` never waits and fails at once – but it never waits longer.
///
/// The method is dispatched statically to the matching method of [`Lock`].
/// Using a method that a lock on the path does not support fails to compile.
/// The error (E0080) comes from monomorphization, so `cargo build` and
/// `cargo test` report it, `cargo check` does not:
///
/// ```compile_fail,E0080
/// # use std::sync::{Arc, Mutex};
/// # use std::time::Duration;
/// use access::Access;
///
/// fn first<P>(numbers: &impl Access<Vec<u32>, P>) -> Option<u32> {
///     let numbers = numbers.try_get_ref_locked(&Duration::from_millis(10)).ok()?;
///     numbers.first().copied()
/// }
///
/// // `std::sync::Mutex` cannot wait with a timeout.
/// first(&Arc::new(Mutex::new(vec![1])));
/// ```
///
/// The same function works with a `RefCell`, which supports every method:
///
/// ```
/// # use std::{cell::RefCell, rc::Rc, time::Duration};
/// # use access::Access;
/// # fn first<P>(numbers: &impl Access<Vec<u32>, P>) -> Option<u32> {
/// #     let numbers = numbers.try_get_ref_locked(&Duration::from_millis(10)).ok()?;
/// #     numbers.first().copied()
/// # }
/// assert_eq!(first(&Rc::new(RefCell::new(vec![1]))), Some(1));
/// ```
pub trait LockMethod: internal::Dispatch {}

impl LockMethod for Block {}
impl LockMethod for TryLock {}
impl LockMethod for Duration {}
impl LockMethod for Instant {}

// User side ------------------------------------------------------------------

/// Shared access to a `T` stored in `Self`.
///
/// `P` is the path from `Self` to `T`. It is always inferred: generic code
/// declares it as an extra type parameter and never names it.
#[diagnostic::on_unimplemented(
    message = "`{Self}` grants no access to `{T}`",
    note = "`{T}` must be reachable through layers that implement `SharedPointer`, `OwningPointer` or `Lock`; a lock cannot contain another lock"
)]
pub trait Access<T: ?Sized, P> {
    /// Returns a reference or guard that dereferences to the value. A lock on
    /// the path is acquired for reading with `method`.
    ///
    /// # Errors
    ///
    /// [`AccessError::WouldBlock`] if the lock is not available within the
    /// limit of `method`, [`AccessError::Poisoned`] if it is poisoned.
    fn try_get_ref_locked<'a, M: LockMethod>(
        &'a self,
        method: &M,
    ) -> Result<impl Deref<Target = T> + use<'a, Self, T, P, M>, AccessError>;

    /// Like [`try_get_ref_locked`](Self::try_get_ref_locked) with [`Block`].
    ///
    /// # Errors
    ///
    /// See [`try_get_ref_locked`](Self::try_get_ref_locked).
    fn try_get_ref(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Self::try_get_ref_locked(self, &Block)
    }

    /// Like [`try_get_ref_locked`](Self::try_get_ref_locked), but panics on
    /// errors.
    ///
    /// # Panics
    ///
    /// Panics if the access fails.
    #[track_caller]
    fn must_get_ref_locked<'a, M: LockMethod>(
        &'a self,
        method: &M,
    ) -> impl Deref<Target = T> + use<'a, Self, T, P, M> {
        must(Self::try_get_ref_locked(self, method))
    }

    /// Like [`try_get_ref`](Self::try_get_ref), but panics on errors.
    ///
    /// # Panics
    ///
    /// Panics if the access fails.
    #[track_caller]
    fn must_get_ref(&self) -> impl Deref<Target = T> {
        must(Self::try_get_ref(self))
    }
}

/// Exclusive access to a `T` stored in `Self`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` grants no exclusive access to `{T}`",
    note = "behind a shared pointer, exclusive access needs a lock further inside or a `RefCounted` pointer such as `Rc` or `Arc`; `&T` alone is read-only"
)]
pub trait AccessMut<T: ?Sized, P>: Access<T, P> {
    /// Returns a reference or guard that mutably dereferences to the value.
    ///
    /// A lock owned by `Self` is not locked at all, since the exclusive
    /// borrow already rules out other users. A lock behind a shared pointer
    /// is acquired for writing with `method`. A shared pointer without a lock
    /// further inside grants access only while it is unique.
    ///
    /// # Errors
    ///
    /// [`AccessError::IsShared`] if a shared pointer without a lock inside is
    /// not unique, [`AccessError::WouldBlock`] if the lock is not available
    /// within the limit of `method`, [`AccessError::Poisoned`] if it is
    /// poisoned.
    fn try_get_mut_locked<'a, M: LockMethod>(
        &'a mut self,
        method: &M,
    ) -> Result<impl DerefMut<Target = T> + use<'a, Self, T, P, M>, AccessError>;

    /// Like [`try_get_mut_locked`](Self::try_get_mut_locked) with [`Block`].
    ///
    /// # Errors
    ///
    /// See [`try_get_mut_locked`](Self::try_get_mut_locked).
    fn try_get_mut(&mut self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Self::try_get_mut_locked(self, &Block)
    }

    /// Like [`try_get_mut_locked`](Self::try_get_mut_locked), but panics on
    /// errors.
    ///
    /// # Panics
    ///
    /// Panics if the access fails.
    #[track_caller]
    fn must_get_mut_locked<'a, M: LockMethod>(
        &'a mut self,
        method: &M,
    ) -> impl DerefMut<Target = T> + use<'a, Self, T, P, M> {
        must(Self::try_get_mut_locked(self, method))
    }

    /// Like [`try_get_mut`](Self::try_get_mut), but panics on errors.
    ///
    /// # Panics
    ///
    /// Panics if the access fails.
    #[track_caller]
    fn must_get_mut(&mut self) -> impl DerefMut<Target = T> {
        must(Self::try_get_mut(self))
    }
}

/// Exclusive access through a shared reference, available whenever a lock
/// lies on the path, as in `Arc<Mutex<T>>` or `&RefCell<T>`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` grants no exclusive access to `{T}` through a shared reference",
    note = "a lock such as `Mutex`, `RwLock` or `RefCell` must lie on the path"
)]
pub trait SharedMut<T: ?Sized, P>: Access<T, P> {
    /// Acquires the lock on the path for writing with `method`.
    ///
    /// # Errors
    ///
    /// [`AccessError::WouldBlock`] if the lock is not available within the
    /// limit of `method`, [`AccessError::Poisoned`] if it is poisoned.
    fn try_get_shared_mut_locked<'a, M: LockMethod>(
        &'a self,
        method: &M,
    ) -> Result<impl DerefMut<Target = T> + use<'a, Self, T, P, M>, AccessError>;

    /// Like [`try_get_shared_mut_locked`](Self::try_get_shared_mut_locked)
    /// with [`Block`].
    ///
    /// # Errors
    ///
    /// See [`try_get_shared_mut_locked`](Self::try_get_shared_mut_locked).
    fn try_get_shared_mut(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Self::try_get_shared_mut_locked(self, &Block)
    }

    /// Like [`try_get_shared_mut_locked`](Self::try_get_shared_mut_locked),
    /// but panics on errors.
    ///
    /// # Panics
    ///
    /// Panics if the access fails.
    #[track_caller]
    fn must_get_shared_mut_locked<'a, M: LockMethod>(
        &'a self,
        method: &M,
    ) -> impl DerefMut<Target = T> + use<'a, Self, T, P, M> {
        must(Self::try_get_shared_mut_locked(self, method))
    }

    /// Like [`try_get_shared_mut`](Self::try_get_shared_mut), but panics on
    /// errors.
    ///
    /// # Panics
    ///
    /// Panics if the access fails.
    #[track_caller]
    fn must_get_shared_mut(&self) -> impl DerefMut<Target = T> {
        must(Self::try_get_shared_mut(self))
    }
}

/// Copy-on-write access: a plain `&mut T` to a value that `Self` owns alone.
///
/// A shared pointer on the path that is not unique is replaced by a pointer
/// to a fresh copy first, like `Rc::make_mut` does; other pointers keep the
/// old value. Owning pointers pass through unchanged, and locks owned by
/// `Self` are not locked at all. A lock that has to be copied is acquired for
/// reading with the lock method.
///
/// Copying needs `Clone`: the target of a shared pointer must be `Clone`, or
/// a lock whose content is `Clone`. Paths without shared pointers copy
/// nothing and need no `Clone` at all:
///
/// ```compile_fail,E0277
/// use std::rc::Rc;
///
/// use access::MakeMut;
///
/// struct Token(u32);
///
/// fn bump<P>(store: &mut impl MakeMut<Token, P>) {
///     store.must_make_mut().0 += 1;
/// }
///
/// bump(&mut Box::new(Token(1))); // fine: nothing to copy
/// bump(&mut Rc::new(Token(1))); // error: `Token` is not `Clone`
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` grants no copy-on-write access to `{T}`",
    note = "behind `Rc`, `Arc` and other `RefCounted` pointers, the value must be `Clone` and `Sized`, or a lock whose content is `Clone`",
    note = "`&T` cannot be replaced by a copy"
)]
pub trait MakeMut<T: ?Sized, P>: Access<T, P> {
    /// Detaches shared pointers on the path and returns the value.
    ///
    /// # Errors
    ///
    /// [`AccessError::WouldBlock`] if a lock to copy is not available within
    /// the limit of `method`, [`AccessError::Poisoned`] if a lock on the path
    /// is poisoned.
    fn try_make_mut_locked<M: LockMethod>(&mut self, method: &M) -> Result<&mut T, AccessError>;

    /// Like [`try_make_mut_locked`](Self::try_make_mut_locked) with
    /// [`Block`].
    ///
    /// # Errors
    ///
    /// See [`try_make_mut_locked`](Self::try_make_mut_locked).
    fn try_make_mut(&mut self) -> Result<&mut T, AccessError> {
        Self::try_make_mut_locked(self, &Block)
    }

    /// Like [`try_make_mut_locked`](Self::try_make_mut_locked), but panics
    /// on errors.
    ///
    /// # Panics
    ///
    /// Panics if the access fails.
    #[track_caller]
    fn must_make_mut_locked<M: LockMethod>(&mut self, method: &M) -> &mut T {
        must(Self::try_make_mut_locked(self, method))
    }

    /// Like [`try_make_mut`](Self::try_make_mut), but panics on errors.
    ///
    /// # Panics
    ///
    /// Panics if the access fails.
    #[track_caller]
    fn must_make_mut(&mut self) -> &mut T {
        must(Self::try_make_mut(self))
    }
}

// Extension points -----------------------------------------------------------

/// Origin of an implementation of the extension traits.
///
/// The orphan rule forbids implementing a foreign trait for a foreign type,
/// such as [`Lock`] for `parking_lot::Mutex`. Outside this crate, the
/// extension traits are foreign:
///
/// ```compile_fail,E0117
/// // Both the trait and `ManuallyDrop` are foreign here.
/// impl<T> access::OwningPointer for std::mem::ManuallyDrop<T> {}
/// ```
///
/// A local glue type as trait parameter makes such an impl legal:
///
/// ```
/// use std::mem::ManuallyDrop;
///
/// use access::{AccessMut, OwningPointer, TraitGlue};
///
/// struct MyGlue;
///
/// impl TraitGlue for MyGlue {}
///
/// impl<T> OwningPointer<MyGlue> for ManuallyDrop<T> {}
///
/// fn push<P>(numbers: &mut impl AccessMut<Vec<u32>, P>, number: u32) {
///     numbers.must_get_mut().push(number);
/// }
///
/// let mut numbers = ManuallyDrop::new(vec![1]);
/// push(&mut numbers, 2);
/// assert_eq!(*numbers, [1, 2]);
/// # drop(ManuallyDrop::into_inner(numbers));
/// ```
///
/// Register every type under one glue only; a second registration makes
/// the path ambiguous.
///
/// Glue types are markers without data. The `'static` bound lets the
/// compiler conclude that the types of an impl outlive a borrow of the
/// container.
pub trait TraitGlue: 'static {}

/// The glue of the implementations in this crate and the default for own
/// types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Builtin;

impl TraitGlue for Builtin {}

/// A pointer that hands out shared references only, like `Rc`, `Arc` and
/// `&T`.
pub trait SharedPointer<G: TraitGlue = Builtin>: Deref {}

/// A reference-counted [`SharedPointer`] like `Rc` and `Arc`, which grants
/// exclusive access while it is the only pointer to its target.
pub trait RefCounted<G: TraitGlue = Builtin>: SharedPointer<G> {
    /// Returns exclusive access if no other pointer shares the target.
    fn get_mut(this: &mut Self) -> Option<&mut Self::Target>;

    /// Allocates a new pointer to `target`. [`MakeMut`] uses it to detach.
    fn new(target: Self::Target) -> Self
    where
        Self: Sized,
        Self::Target: Sized;
}

/// A pointer that owns its target exclusively, like `Box` and `&mut T`.
pub trait OwningPointer<G: TraitGlue = Builtin>: DerefMut {}

/// A lock or cell with interior mutability, like `Mutex`, `RwLock` and
/// `RefCell`.
///
/// Each acquiring method serves one [`LockMethod`]: `read` and `write` serve
/// [`Block`], `try_*` serve [`TryLock`], `*_for` serve [`Duration`] and
/// `*_until` serve [`Instant`]. A method may give up earlier than its lock
/// method allows, but never wait longer.
///
/// The methods for [`TryLock`] and [`Duration`] are optional. Left out, they
/// fail the build with error E0080 as soon as code that uses them is
/// compiled. The methods for [`Instant`] default to the ones for
/// [`Duration`]. Leave unsupported methods out instead of implementing them
/// with `unsupported!()` yourself: in a non-generic impl, the compiler
/// builds such a method even if it is never called.
pub trait Lock<G: TraitGlue = Builtin> {
    /// The protected value.
    type Inner: ?Sized;

    /// Acquires shared access, waiting as long as necessary.
    ///
    /// # Errors
    ///
    /// [`AccessError::WouldBlock`] if the lock cannot wait at all,
    /// [`AccessError::Poisoned`] if the lock is poisoned.
    fn read(&self) -> Result<impl Deref<Target = Self::Inner>, AccessError>;

    /// Acquires exclusive access, waiting as long as necessary.
    ///
    /// # Errors
    ///
    /// See [`read`](Self::read).
    fn write(&self) -> Result<impl DerefMut<Target = Self::Inner>, AccessError>;

    /// Acquires shared access without waiting.
    ///
    /// # Errors
    ///
    /// [`AccessError::WouldBlock`] if the lock is taken,
    /// [`AccessError::Poisoned`] if it is poisoned.
    fn try_read(&self) -> Result<impl Deref<Target = Self::Inner>, AccessError> {
        try_lock_unsupported::<&Self::Inner>()
    }

    /// Acquires exclusive access without waiting.
    ///
    /// # Errors
    ///
    /// See [`try_read`](Self::try_read).
    fn try_write(&self) -> Result<impl DerefMut<Target = Self::Inner>, AccessError> {
        try_lock_unsupported::<&mut Self::Inner>()
    }

    /// Acquires shared access, waiting at most `timeout`.
    ///
    /// # Errors
    ///
    /// [`AccessError::WouldBlock`] if the lock is still taken after
    /// `timeout`, [`AccessError::Poisoned`] if it is poisoned.
    fn read_for(&self, timeout: Duration) -> Result<impl Deref<Target = Self::Inner>, AccessError> {
        let _ = timeout;
        timeout_unsupported::<&Self::Inner>()
    }

    /// Acquires exclusive access, waiting at most `timeout`.
    ///
    /// # Errors
    ///
    /// See [`read_for`](Self::read_for).
    fn write_for(
        &self,
        timeout: Duration,
    ) -> Result<impl DerefMut<Target = Self::Inner>, AccessError> {
        let _ = timeout;
        timeout_unsupported::<&mut Self::Inner>()
    }

    /// Acquires shared access, waiting at most until `deadline`.
    ///
    /// # Errors
    ///
    /// See [`read_for`](Self::read_for).
    fn read_until(
        &self,
        deadline: Instant,
    ) -> Result<impl Deref<Target = Self::Inner>, AccessError> {
        Self::read_for(self, deadline.saturating_duration_since(Instant::now()))
    }

    /// Acquires exclusive access, waiting at most until `deadline`.
    ///
    /// # Errors
    ///
    /// See [`read_for`](Self::read_for).
    fn write_until(
        &self,
        deadline: Instant,
    ) -> Result<impl DerefMut<Target = Self::Inner>, AccessError> {
        Self::write_for(self, deadline.saturating_duration_since(Instant::now()))
    }

    /// Grants exclusive access without locking, since the exclusive borrow of
    /// the lock already rules out any other user.
    ///
    /// # Errors
    ///
    /// [`AccessError::Poisoned`] if the lock is poisoned.
    fn inner_mut(&mut self) -> Result<&mut Self::Inner, AccessError>;

    /// Creates a lock that protects `inner`. [`MakeMut`] uses it to copy
    /// locks.
    fn new(inner: Self::Inner) -> Self
    where
        Self: Sized,
        Self::Inner: Sized;
}

// Unsupported lock methods. A diverging body gives the opaque return type of
// a `Lock` method no hidden type; these helpers supply one. Being generic,
// they are compiled only when code that uses an unsupported method is.

fn try_lock_unsupported<Guard>() -> Result<Guard, AccessError> {
    unsupported!(
        "this lock does not support `TryLock`: implement `Lock::try_read` and `Lock::try_write`"
    )
}

fn timeout_unsupported<Guard>() -> Result<Guard, AccessError> {
    unsupported!(
        "this lock does not support `Duration` and `Instant`: implement `Lock::read_for` and `Lock::write_for`"
    )
}

fn std_timeout_unsupported<Guard>() -> Result<Guard, AccessError> {
    unsupported!(
        "`std::sync::Mutex` and `std::sync::RwLock` cannot wait with a timeout or deadline"
    )
}

impl<T: ?Sized> SharedPointer for &T {}
impl<T: ?Sized> SharedPointer for Rc<T> {}
impl<T: ?Sized> SharedPointer for Arc<T> {}

impl<T: ?Sized> RefCounted for Rc<T> {
    fn get_mut(this: &mut Self) -> Option<&mut T> {
        Rc::get_mut(this)
    }

    fn new(target: T) -> Self
    where
        T: Sized,
    {
        Rc::new(target)
    }
}

impl<T: ?Sized> RefCounted for Arc<T> {
    fn get_mut(this: &mut Self) -> Option<&mut T> {
        Arc::get_mut(this)
    }

    fn new(target: T) -> Self
    where
        T: Sized,
    {
        Arc::new(target)
    }
}

impl<T: ?Sized> OwningPointer for &mut T {}
impl<T: ?Sized> OwningPointer for Box<T> {}

/// Blocks and tries; timeouts are unsupported.
impl<T: ?Sized> Lock for Mutex<T> {
    type Inner = T;

    fn read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(Mutex::lock(self)?)
    }

    fn write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(Mutex::lock(self)?)
    }

    fn try_read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(Mutex::try_lock(self)?)
    }

    fn try_write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(Mutex::try_lock(self)?)
    }

    fn read_for(&self, _timeout: Duration) -> Result<impl Deref<Target = T>, AccessError> {
        std_timeout_unsupported::<&T>()
    }

    fn write_for(&self, _timeout: Duration) -> Result<impl DerefMut<Target = T>, AccessError> {
        std_timeout_unsupported::<&mut T>()
    }

    fn inner_mut(&mut self) -> Result<&mut T, AccessError> {
        Ok(Mutex::get_mut(self)?)
    }

    fn new(inner: T) -> Self
    where
        T: Sized,
    {
        Mutex::new(inner)
    }
}

/// Blocks and tries; timeouts are unsupported.
impl<T: ?Sized> Lock for RwLock<T> {
    type Inner = T;

    fn read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(RwLock::read(self)?)
    }

    fn write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(RwLock::write(self)?)
    }

    fn try_read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(RwLock::try_read(self)?)
    }

    fn try_write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(RwLock::try_write(self)?)
    }

    fn read_for(&self, _timeout: Duration) -> Result<impl Deref<Target = T>, AccessError> {
        std_timeout_unsupported::<&T>()
    }

    fn write_for(&self, _timeout: Duration) -> Result<impl DerefMut<Target = T>, AccessError> {
        std_timeout_unsupported::<&mut T>()
    }

    fn inner_mut(&mut self) -> Result<&mut T, AccessError> {
        Ok(RwLock::get_mut(self)?)
    }

    fn new(inner: T) -> Self
    where
        T: Sized,
    {
        RwLock::new(inner)
    }
}

/// Supports every lock method, but never waits: waiting for a borrow in the
/// same thread could only deadlock, so a conflicting borrow fails at once
/// with [`AccessError::WouldBlock`] instead of panicking.
impl<T: ?Sized> Lock for RefCell<T> {
    type Inner = T;

    fn read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(RefCell::try_borrow(self)?)
    }

    fn write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(RefCell::try_borrow_mut(self)?)
    }

    fn try_read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(RefCell::try_borrow(self)?)
    }

    fn try_write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(RefCell::try_borrow_mut(self)?)
    }

    fn read_for(&self, _timeout: Duration) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(RefCell::try_borrow(self)?)
    }

    fn write_for(&self, _timeout: Duration) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(RefCell::try_borrow_mut(self)?)
    }

    fn inner_mut(&mut self) -> Result<&mut T, AccessError> {
        Ok(RefCell::get_mut(self))
    }

    fn new(inner: T) -> Self
    where
        T: Sized,
    {
        RefCell::new(inner)
    }
}

// Machinery ------------------------------------------------------------------

/// Path markers, guard-free projections, lock method dispatch and the blanket
/// implementations that compose the layers. The items are `pub` for the trait
/// system only; the private module keeps them unreachable from outside.
mod internal {
    use std::marker::PhantomData;
    use std::ops::{Deref, DerefMut};
    use std::time::{Duration, Instant};

    use super::{
        Access, AccessError, AccessMut, Block, Lock, LockMethod, MakeMut, OwningPointer,
        RefCounted, SharedMut, SharedPointer, TraitGlue, TryLock,
    };

    // Lock method dispatch.

    /// Calls the [`Lock`] method that serves a lock method. Sealing
    /// [`LockMethod`] with it keeps the set of lock methods closed.
    pub trait Dispatch {
        fn read<'l, G: TraitGlue, L: Lock<G> + ?Sized>(
            &self,
            lock: &'l L,
        ) -> Result<impl Deref<Target = L::Inner> + use<'l, G, L, Self>, AccessError>;

        fn write<'l, G: TraitGlue, L: Lock<G> + ?Sized>(
            &self,
            lock: &'l L,
        ) -> Result<impl DerefMut<Target = L::Inner> + use<'l, G, L, Self>, AccessError>;
    }

    impl Dispatch for Block {
        fn read<'l, G: TraitGlue, L: Lock<G> + ?Sized>(
            &self,
            lock: &'l L,
        ) -> Result<impl Deref<Target = L::Inner> + use<'l, G, L>, AccessError> {
            Lock::<G>::read(lock)
        }

        fn write<'l, G: TraitGlue, L: Lock<G> + ?Sized>(
            &self,
            lock: &'l L,
        ) -> Result<impl DerefMut<Target = L::Inner> + use<'l, G, L>, AccessError> {
            Lock::<G>::write(lock)
        }
    }

    impl Dispatch for TryLock {
        fn read<'l, G: TraitGlue, L: Lock<G> + ?Sized>(
            &self,
            lock: &'l L,
        ) -> Result<impl Deref<Target = L::Inner> + use<'l, G, L>, AccessError> {
            Lock::<G>::try_read(lock)
        }

        fn write<'l, G: TraitGlue, L: Lock<G> + ?Sized>(
            &self,
            lock: &'l L,
        ) -> Result<impl DerefMut<Target = L::Inner> + use<'l, G, L>, AccessError> {
            Lock::<G>::try_write(lock)
        }
    }

    impl Dispatch for Duration {
        fn read<'l, G: TraitGlue, L: Lock<G> + ?Sized>(
            &self,
            lock: &'l L,
        ) -> Result<impl Deref<Target = L::Inner> + use<'l, G, L>, AccessError> {
            Lock::<G>::read_for(lock, *self)
        }

        fn write<'l, G: TraitGlue, L: Lock<G> + ?Sized>(
            &self,
            lock: &'l L,
        ) -> Result<impl DerefMut<Target = L::Inner> + use<'l, G, L>, AccessError> {
            Lock::<G>::write_for(lock, *self)
        }
    }

    impl Dispatch for Instant {
        fn read<'l, G: TraitGlue, L: Lock<G> + ?Sized>(
            &self,
            lock: &'l L,
        ) -> Result<impl Deref<Target = L::Inner> + use<'l, G, L>, AccessError> {
            Lock::<G>::read_until(lock, *self)
        }

        fn write<'l, G: TraitGlue, L: Lock<G> + ?Sized>(
            &self,
            lock: &'l L,
        ) -> Result<impl DerefMut<Target = L::Inner> + use<'l, G, L>, AccessError> {
            Lock::<G>::write_until(lock, *self)
        }
    }

    // Paths.

    /// Path to the value itself.
    pub struct Here;

    /// Path through a [`SharedPointer`] with glue `G`, then along `P`.
    pub struct ViaShared<G, P>(PhantomData<(G, P)>);

    /// Path through an [`OwningPointer`] with glue `G`, then along `P`.
    pub struct ViaOwned<G, P>(PhantomData<(G, P)>);

    /// Path through a [`Lock`] with glue `G`, then along `P`.
    pub struct ViaLock<G, P>(PhantomData<(G, P)>);

    /// Type-level answers.
    pub struct Yes;
    pub struct No;

    /// Whether a lock lies on the path.
    pub trait PathInfo {
        type HasLock;
    }

    impl PathInfo for Here {
        type HasLock = No;
    }

    impl<G, P: PathInfo> PathInfo for ViaShared<G, P> {
        type HasLock = P::HasLock;
    }

    impl<G, P: PathInfo> PathInfo for ViaOwned<G, P> {
        type HasLock = P::HasLock;
    }

    impl<G, P> PathInfo for ViaLock<G, P> {
        type HasLock = Yes;
    }

    // Helpers.

    /// Shared access without a guard. A lock needs it to reach the value from
    /// inside its own guard.
    pub trait Project<T: ?Sized, P> {
        fn project(&self) -> &T;
    }

    /// Exclusive access without a guard.
    pub trait ProjectMut<T: ?Sized, P>: Project<T, P> {
        fn project_mut(&mut self) -> &mut T;
    }

    /// Exclusive access through a shared pointer, whose rest of the path
    /// does (`Yes`) or does not (`No`) contain a lock.
    pub trait SharedPointerMut<T: ?Sized, G, P, HasLock> {
        fn shared_pointer_mut<'a, M: LockMethod>(
            &'a mut self,
            method: &M,
        ) -> Result<impl DerefMut<Target = T> + use<'a, Self, T, G, P, HasLock, M>, AccessError>;
    }

    /// Copies the first layer of a path for [`MakeMut`]: a lock is acquired
    /// to clone its content into a new lock, everything else is cloned.
    pub trait Duplicate<P>: Sized {
        fn duplicate<M: LockMethod>(&self, method: &M) -> Result<Self, AccessError>;
    }

    /// A lock guard that dereferences to the value inside the lock.
    struct Projected<Guard, T: ?Sized, P> {
        guard: Guard,
        path: PhantomData<fn() -> (P, *const T)>,
    }

    impl<Guard, T: ?Sized, P> Projected<Guard, T, P> {
        fn new(guard: Guard) -> Self {
            Self {
                guard,
                path: PhantomData,
            }
        }
    }

    impl<Guard, T, P> Deref for Projected<Guard, T, P>
    where
        Guard: Deref,
        Guard::Target: Project<T, P>,
        T: ?Sized,
    {
        type Target = T;

        fn deref(&self) -> &T {
            Project::<T, P>::project(&*self.guard)
        }
    }

    impl<Guard, T, P> DerefMut for Projected<Guard, T, P>
    where
        Guard: DerefMut,
        Guard::Target: ProjectMut<T, P>,
        T: ?Sized,
    {
        fn deref_mut(&mut self) -> &mut T {
            ProjectMut::<T, P>::project_mut(&mut *self.guard)
        }
    }

    // The value itself.

    impl<T: ?Sized> Access<T, Here> for T {
        fn try_get_ref_locked<'a, M: LockMethod>(
            &'a self,
            _method: &M,
        ) -> Result<impl Deref<Target = T> + use<'a, T, M>, AccessError> {
            Ok(self)
        }
    }

    impl<T: ?Sized> AccessMut<T, Here> for T {
        fn try_get_mut_locked<'a, M: LockMethod>(
            &'a mut self,
            _method: &M,
        ) -> Result<impl DerefMut<Target = T> + use<'a, T, M>, AccessError> {
            Ok(self)
        }
    }

    impl<T: ?Sized> MakeMut<T, Here> for T {
        fn try_make_mut_locked<M: LockMethod>(
            &mut self,
            _method: &M,
        ) -> Result<&mut T, AccessError> {
            Ok(self)
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

    impl<T: Clone> Duplicate<Here> for T {
        fn duplicate<M: LockMethod>(&self, _method: &M) -> Result<Self, AccessError> {
            Ok(self.clone())
        }
    }

    // Shared pointers: shared access passes through. Exclusive access goes
    // through a lock further inside or needs a unique pointer.

    impl<X, T, G, P> Access<T, ViaShared<G, P>> for X
    where
        X: SharedPointer<G> + ?Sized,
        X::Target: Access<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_get_ref_locked<'a, M: LockMethod>(
            &'a self,
            method: &M,
        ) -> Result<impl Deref<Target = T> + use<'a, X, T, G, P, M>, AccessError> {
            Access::<T, P>::try_get_ref_locked(&**self, method)
        }
    }

    impl<X, T, G, P> AccessMut<T, ViaShared<G, P>> for X
    where
        X: SharedPointer<G> + SharedPointerMut<T, G, P, P::HasLock> + ?Sized,
        X::Target: Access<T, P>,
        P: PathInfo,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_get_mut_locked<'a, M: LockMethod>(
            &'a mut self,
            method: &M,
        ) -> Result<impl DerefMut<Target = T> + use<'a, X, T, G, P, M>, AccessError> {
            SharedPointerMut::<T, G, P, P::HasLock>::shared_pointer_mut(self, method)
        }
    }

    /// With a lock further inside, writes go through the lock and reach all
    /// pointers to the same target.
    impl<X, T, G, P> SharedPointerMut<T, G, P, Yes> for X
    where
        X: SharedPointer<G> + ?Sized,
        X::Target: SharedMut<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn shared_pointer_mut<'a, M: LockMethod>(
            &'a mut self,
            method: &M,
        ) -> Result<impl DerefMut<Target = T> + use<'a, X, T, G, P, M>, AccessError> {
            SharedMut::<T, P>::try_get_shared_mut_locked(&**self, method)
        }
    }

    /// Without a lock, only a unique pointer grants exclusive access.
    impl<X, T, G, P> SharedPointerMut<T, G, P, No> for X
    where
        X: RefCounted<G> + ?Sized,
        X::Target: AccessMut<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn shared_pointer_mut<'a, M: LockMethod>(
            &'a mut self,
            method: &M,
        ) -> Result<impl DerefMut<Target = T> + use<'a, X, T, G, P, M>, AccessError> {
            let target = RefCounted::<G>::get_mut(self).ok_or(AccessError::IsShared)?;
            AccessMut::<T, P>::try_get_mut_locked(target, method)
        }
    }

    impl<X, T, G, P> SharedMut<T, ViaShared<G, P>> for X
    where
        X: SharedPointer<G> + ?Sized,
        X::Target: SharedMut<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_get_shared_mut_locked<'a, M: LockMethod>(
            &'a self,
            method: &M,
        ) -> Result<impl DerefMut<Target = T> + use<'a, X, T, G, P, M>, AccessError> {
            SharedMut::<T, P>::try_get_shared_mut_locked(&**self, method)
        }
    }

    impl<X, T, G, P> MakeMut<T, ViaShared<G, P>> for X
    where
        X: RefCounted<G>,
        X::Target: MakeMut<T, P> + Duplicate<P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_make_mut_locked<M: LockMethod>(
            &mut self,
            method: &M,
        ) -> Result<&mut T, AccessError> {
            if RefCounted::<G>::get_mut(self).is_none() {
                let copy = Duplicate::<P>::duplicate(&**self, method)?;
                *self = RefCounted::<G>::new(copy);
            }
            let target = RefCounted::<G>::get_mut(self).ok_or(AccessError::IsShared)?;
            MakeMut::<T, P>::try_make_mut_locked(target, method)
        }
    }

    impl<X, T, G, P> Project<T, ViaShared<G, P>> for X
    where
        X: SharedPointer<G> + ?Sized,
        X::Target: Project<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn project(&self) -> &T {
            Project::<T, P>::project(&**self)
        }
    }

    /// Cloning a shared pointer is shallow; [`MakeMut`] detaches the inner
    /// layers afterwards.
    impl<X, G, P> Duplicate<ViaShared<G, P>> for X
    where
        X: SharedPointer<G> + Clone,
        G: TraitGlue,
    {
        fn duplicate<M: LockMethod>(&self, _method: &M) -> Result<Self, AccessError> {
            Ok(self.clone())
        }
    }

    // Owning pointers: every kind of access passes through.

    impl<X, T, G, P> Access<T, ViaOwned<G, P>> for X
    where
        X: OwningPointer<G> + ?Sized,
        X::Target: Access<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_get_ref_locked<'a, M: LockMethod>(
            &'a self,
            method: &M,
        ) -> Result<impl Deref<Target = T> + use<'a, X, T, G, P, M>, AccessError> {
            Access::<T, P>::try_get_ref_locked(&**self, method)
        }
    }

    impl<X, T, G, P> AccessMut<T, ViaOwned<G, P>> for X
    where
        X: OwningPointer<G> + ?Sized,
        X::Target: AccessMut<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_get_mut_locked<'a, M: LockMethod>(
            &'a mut self,
            method: &M,
        ) -> Result<impl DerefMut<Target = T> + use<'a, X, T, G, P, M>, AccessError> {
            AccessMut::<T, P>::try_get_mut_locked(&mut **self, method)
        }
    }

    impl<X, T, G, P> SharedMut<T, ViaOwned<G, P>> for X
    where
        X: OwningPointer<G> + ?Sized,
        X::Target: SharedMut<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_get_shared_mut_locked<'a, M: LockMethod>(
            &'a self,
            method: &M,
        ) -> Result<impl DerefMut<Target = T> + use<'a, X, T, G, P, M>, AccessError> {
            SharedMut::<T, P>::try_get_shared_mut_locked(&**self, method)
        }
    }

    impl<X, T, G, P> MakeMut<T, ViaOwned<G, P>> for X
    where
        X: OwningPointer<G> + ?Sized,
        X::Target: MakeMut<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_make_mut_locked<M: LockMethod>(
            &mut self,
            method: &M,
        ) -> Result<&mut T, AccessError> {
            MakeMut::<T, P>::try_make_mut_locked(&mut **self, method)
        }
    }

    impl<X, T, G, P> Project<T, ViaOwned<G, P>> for X
    where
        X: OwningPointer<G> + ?Sized,
        X::Target: Project<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn project(&self) -> &T {
            Project::<T, P>::project(&**self)
        }
    }

    impl<X, T, G, P> ProjectMut<T, ViaOwned<G, P>> for X
    where
        X: OwningPointer<G> + ?Sized,
        X::Target: ProjectMut<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn project_mut(&mut self) -> &mut T {
            ProjectMut::<T, P>::project_mut(&mut **self)
        }
    }

    impl<X, G, P> Duplicate<ViaOwned<G, P>> for X
    where
        X: OwningPointer<G> + Clone,
        G: TraitGlue,
    {
        fn duplicate<M: LockMethod>(&self, _method: &M) -> Result<Self, AccessError> {
            Ok(self.clone())
        }
    }

    // Locks: the guard is projected to the value, so the content must be
    // reachable without a guard of its own.

    impl<L, T, G, P> Access<T, ViaLock<G, P>> for L
    where
        L: Lock<G> + ?Sized,
        L::Inner: Project<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_get_ref_locked<'a, M: LockMethod>(
            &'a self,
            method: &M,
        ) -> Result<impl Deref<Target = T> + use<'a, L, T, G, P, M>, AccessError> {
            let guard = Dispatch::read::<G, L>(method, self)?;
            Ok(Projected::<_, T, P>::new(guard))
        }
    }

    impl<L, T, G, P> AccessMut<T, ViaLock<G, P>> for L
    where
        L: Lock<G> + ?Sized,
        L::Inner: ProjectMut<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_get_mut_locked<'a, M: LockMethod>(
            &'a mut self,
            _method: &M,
        ) -> Result<impl DerefMut<Target = T> + use<'a, L, T, G, P, M>, AccessError> {
            // A plain `&mut T` would require `T: 'a`, which the trait cannot
            // promise; projecting on each dereference avoids that bound.
            let inner = Lock::<G>::inner_mut(self)?;
            Ok(Projected::<_, T, P>::new(inner))
        }
    }

    impl<L, T, G, P> SharedMut<T, ViaLock<G, P>> for L
    where
        L: Lock<G> + ?Sized,
        L::Inner: ProjectMut<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_get_shared_mut_locked<'a, M: LockMethod>(
            &'a self,
            method: &M,
        ) -> Result<impl DerefMut<Target = T> + use<'a, L, T, G, P, M>, AccessError> {
            let guard = Dispatch::write::<G, L>(method, self)?;
            Ok(Projected::<_, T, P>::new(guard))
        }
    }

    impl<L, T, G, P> MakeMut<T, ViaLock<G, P>> for L
    where
        L: Lock<G> + ?Sized,
        L::Inner: MakeMut<T, P> + Project<T, P>,
        T: ?Sized,
        G: TraitGlue,
    {
        fn try_make_mut_locked<M: LockMethod>(
            &mut self,
            method: &M,
        ) -> Result<&mut T, AccessError> {
            let inner = Lock::<G>::inner_mut(self)?;
            MakeMut::<T, P>::try_make_mut_locked(inner, method)
        }
    }

    impl<L, G, P> Duplicate<ViaLock<G, P>> for L
    where
        L: Lock<G>,
        L::Inner: Clone,
        G: TraitGlue,
    {
        fn duplicate<M: LockMethod>(&self, method: &M) -> Result<Self, AccessError> {
            let guard = Dispatch::read::<G, L>(method, self)?;
            Ok(Lock::<G>::new(L::Inner::clone(&guard)))
        }
    }
}
