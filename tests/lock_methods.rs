//! Lock methods are dispatched statically to the matching `Lock` method and
//! bound how long an access may wait.

use std::cell::{Cell, RefCell};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use access::{Access, AccessError, AccessMut, Block, Lock, LockMethod, SharedMut, TryLock};

/// A cell that records which of its methods was called last.
struct RecordingCell<T> {
    value: RefCell<T>,
    last: Cell<&'static str>,
}

impl<T> RecordingCell<T> {
    fn new(value: T) -> Self {
        Self {
            value: RefCell::new(value),
            last: Cell::new("none"),
        }
    }

    fn last(&self) -> &'static str {
        self.last.get()
    }

    fn reading(&self, method: &'static str) -> Result<impl Deref<Target = T>, AccessError> {
        self.last.set(method);
        Ok(self.value.try_borrow()?)
    }

    fn writing(&self, method: &'static str) -> Result<impl DerefMut<Target = T>, AccessError> {
        self.last.set(method);
        Ok(self.value.try_borrow_mut()?)
    }
}

impl<T> Lock for RecordingCell<T> {
    type Inner = T;

    fn read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        self.reading("read")
    }

    fn write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        self.writing("write")
    }

    fn try_read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        self.reading("try_read")
    }

    fn try_write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        self.writing("try_write")
    }

    fn read_for(&self, _timeout: Duration) -> Result<impl Deref<Target = T>, AccessError> {
        self.reading("read_for")
    }

    fn write_for(&self, _timeout: Duration) -> Result<impl DerefMut<Target = T>, AccessError> {
        self.writing("write_for")
    }

    fn read_until(&self, _deadline: Instant) -> Result<impl Deref<Target = T>, AccessError> {
        self.reading("read_until")
    }

    fn write_until(&self, _deadline: Instant) -> Result<impl DerefMut<Target = T>, AccessError> {
        self.writing("write_until")
    }

    fn inner_mut(&mut self) -> Result<&mut T, AccessError> {
        self.last.set("inner_mut");
        Ok(self.value.get_mut())
    }

    fn new(inner: T) -> Self {
        Self::new(inner)
    }
}

fn read<P, M: LockMethod>(store: &impl Access<u32, P>, method: &M) -> u32 {
    *store.must_get_ref_locked(method)
}

fn try_read<P, M: LockMethod>(store: &impl Access<u32, P>, method: &M) -> Result<u32, AccessError> {
    Ok(*store.try_get_ref_locked(method)?)
}

fn write<P, M: LockMethod>(store: &impl SharedMut<u32, P>, method: &M, value: u32) {
    *store.must_get_shared_mut_locked(method) = value;
}

fn bump<P, M: LockMethod>(store: &mut impl AccessMut<u32, P>, method: &M) {
    *store.must_get_mut_locked(method) += 1;
}

#[test]
fn every_lock_method_reaches_its_own_lock_method() {
    let store = Rc::new(RecordingCell::new(0));
    let deadline = Instant::now() + Duration::from_secs(1);
    let timeout = Duration::from_secs(1);

    read(&store, &Block);
    assert_eq!(store.last(), "read");
    read(&store, &TryLock);
    assert_eq!(store.last(), "try_read");
    read(&store, &timeout);
    assert_eq!(store.last(), "read_for");
    read(&store, &deadline);
    assert_eq!(store.last(), "read_until");

    write(&store, &Block, 1);
    assert_eq!(store.last(), "write");
    write(&store, &TryLock, 2);
    assert_eq!(store.last(), "try_write");
    write(&store, &timeout, 3);
    assert_eq!(store.last(), "write_for");
    write(&store, &deadline, 4);
    assert_eq!(store.last(), "write_until");

    assert_eq!(read(&store, &Block), 4);
}

#[test]
fn methods_without_suffix_block() {
    fn read_default<P>(store: &impl Access<u32, P>) -> u32 {
        *store.must_get_ref()
    }

    let mut store = Rc::new(RecordingCell::new(0));
    read_default(&store);
    assert_eq!(store.last(), "read");
    *store.must_get_shared_mut() += 1;
    assert_eq!(store.last(), "write");
    bump(&mut store, &Block);
    assert_eq!(store.last(), "write");
}

#[test]
fn exclusive_access_to_an_owned_lock_ignores_the_method() {
    let mut store = RecordingCell::new(0);
    bump(&mut store, &TryLock);
    assert_eq!(store.last(), "inner_mut");
}

#[test]
fn try_lock_fails_at_once_while_the_lock_is_taken() {
    let mutex = Arc::new(Mutex::new(1));
    let rwlock = Arc::new(RwLock::new(1));

    let mutex_guard = mutex.lock().unwrap();
    let rwlock_guard = rwlock.write().unwrap();
    assert_eq!(try_read(&mutex, &TryLock), Err(AccessError::WouldBlock));
    assert_eq!(try_read(&rwlock, &TryLock), Err(AccessError::WouldBlock));

    drop(mutex_guard);
    drop(rwlock_guard);
    assert_eq!(try_read(&mutex, &TryLock), Ok(1));
    assert_eq!(try_read(&rwlock, &TryLock), Ok(1));
}

#[test]
fn rwlock_readers_do_not_exclude_each_other() {
    let rwlock = Arc::new(RwLock::new(1));
    let _reader = rwlock.read().unwrap();
    assert_eq!(try_read(&rwlock, &TryLock), Ok(1));
}

#[test]
fn refcell_never_waits() {
    let store = Rc::new(RefCell::new(1));
    let writer = store.borrow_mut();
    let started = Instant::now();

    assert_eq!(try_read(&store, &Block), Err(AccessError::WouldBlock));
    assert_eq!(try_read(&store, &TryLock), Err(AccessError::WouldBlock));
    assert_eq!(
        try_read(&store, &Duration::from_secs(10)),
        Err(AccessError::WouldBlock)
    );
    assert_eq!(
        try_read(&store, &(Instant::now() + Duration::from_secs(10))),
        Err(AccessError::WouldBlock)
    );
    assert!(started.elapsed() < Duration::from_secs(5));

    drop(writer);
    assert_eq!(try_read(&store, &Duration::ZERO), Ok(1));
}
