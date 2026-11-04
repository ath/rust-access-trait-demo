//! Types of another crate join through a local glue type. Neither newtypes
//! nor features of `access` are needed, and the orphan rule is satisfied.

use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use access::{
    Access, AccessError, AccessMut, Lock, LockMethod, MakeMut, OwningPointer, RefCounted,
    SharedPointer, TraitGlue, TryLock,
};
use somebox::{ArcLikeThing, BoxLikeThing, LockLikeThing};

/// Glue for the types of `somebox`.
struct SomeboxGlue;

impl TraitGlue for SomeboxGlue {}

impl<T: ?Sized> OwningPointer<SomeboxGlue> for BoxLikeThing<T> {}

impl<T: ?Sized> SharedPointer<SomeboxGlue> for ArcLikeThing<T> {}

impl<T: ?Sized> RefCounted<SomeboxGlue> for ArcLikeThing<T> {
    fn get_mut(this: &mut Self) -> Option<&mut T> {
        ArcLikeThing::get_mut(this)
    }

    fn new(target: T) -> Self
    where
        T: Sized,
    {
        ArcLikeThing::new(target)
    }
}

impl<T: ?Sized> Lock<SomeboxGlue> for LockLikeThing<T> {
    type Inner = T;

    fn read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(self.lock())
    }

    fn write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(self.lock())
    }

    fn try_read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        self.try_lock().ok_or(AccessError::WouldBlock)
    }

    fn try_write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        self.try_lock().ok_or(AccessError::WouldBlock)
    }

    fn read_for(&self, timeout: Duration) -> Result<impl Deref<Target = T>, AccessError> {
        self.try_lock_for(timeout).ok_or(AccessError::WouldBlock)
    }

    fn write_for(&self, timeout: Duration) -> Result<impl DerefMut<Target = T>, AccessError> {
        self.try_lock_for(timeout).ok_or(AccessError::WouldBlock)
    }

    fn read_until(&self, deadline: Instant) -> Result<impl Deref<Target = T>, AccessError> {
        self.try_lock_until(deadline).ok_or(AccessError::WouldBlock)
    }

    fn write_until(&self, deadline: Instant) -> Result<impl DerefMut<Target = T>, AccessError> {
        self.try_lock_until(deadline).ok_or(AccessError::WouldBlock)
    }

    fn inner_mut(&mut self) -> Result<&mut T, AccessError> {
        Ok(self.get_mut())
    }

    fn new(inner: T) -> Self
    where
        T: Sized,
    {
        LockLikeThing::new(inner)
    }
}

fn contents<P>(store: &impl Access<Vec<u32>, P>) -> Vec<u32> {
    store.must_get_ref().clone()
}

fn try_contents<P, M: LockMethod>(
    store: &impl Access<Vec<u32>, P>,
    method: &M,
) -> Result<Vec<u32>, AccessError> {
    Ok(store.try_get_ref_locked(method)?.clone())
}

fn push<P>(store: &mut impl AccessMut<Vec<u32>, P>, value: u32) {
    store.must_get_mut().push(value);
}

fn try_push<P>(store: &mut impl AccessMut<Vec<u32>, P>, value: u32) -> Result<(), AccessError> {
    store.try_get_mut()?.push(value);
    Ok(())
}

fn append<P>(store: &mut impl MakeMut<Vec<u32>, P>, value: u32) {
    store.must_make_mut().push(value);
}

#[test]
fn foreign_types_alone() {
    let mut boxed = BoxLikeThing::new(vec![1]);
    let mut counted = ArcLikeThing::new(vec![1]);
    let mut locked = LockLikeThing::new(vec![1]);

    push(&mut boxed, 2);
    push(&mut counted, 2);
    push(&mut locked, 2);

    assert_eq!(contents(&boxed), [1, 2]);
    assert_eq!(contents(&counted), [1, 2]);
    assert_eq!(contents(&locked), [1, 2]);
}

#[test]
fn foreign_and_std_types_mix() {
    let mut first = Arc::new(LockLikeThing::new(BoxLikeThing::new(vec![1])));
    let mut second = ArcLikeThing::new(Mutex::new(vec![1]));
    let mut third = BoxLikeThing::new(Rc::new(RefCell::new(vec![1])));
    let first_clone = Arc::clone(&first);
    let second_clone = second.clone();

    push(&mut first, 2);
    push(&mut second, 2);
    push(&mut third, 2);

    assert_eq!(contents(&first_clone), [1, 2]);
    assert_eq!(contents(&second_clone), [1, 2]);
    assert_eq!(contents(&third), [1, 2]);
}

#[test]
fn foreign_ref_counted_pointer_is_exclusive_only_while_unique() {
    let mut store = ArcLikeThing::new(vec![1]);
    let clone = store.clone();
    assert_eq!(try_push(&mut store, 2), Err(AccessError::IsShared));
    drop(clone);
    assert_eq!(try_push(&mut store, 2), Ok(()));
}

#[test]
fn foreign_layers_detach_on_make_mut() {
    let mut store = ArcLikeThing::new(LockLikeThing::new(BoxLikeThing::new(vec![1])));
    let clone = store.clone();
    append(&mut store, 2);
    assert_eq!(contents(&store), [1, 2]);
    assert_eq!(contents(&clone), [1]);
}

#[test]
fn foreign_lock_gives_up_after_the_timeout() {
    let store = Arc::new(LockLikeThing::new(vec![1]));
    let timeout = Duration::from_millis(30);
    let guard = store.lock();

    let started = Instant::now();
    assert_eq!(try_contents(&store, &timeout), Err(AccessError::WouldBlock));
    assert!(started.elapsed() >= timeout);

    let deadline = Instant::now() + timeout;
    assert_eq!(
        try_contents(&store, &deadline),
        Err(AccessError::WouldBlock)
    );
    assert!(Instant::now() >= deadline);

    assert_eq!(try_contents(&store, &TryLock), Err(AccessError::WouldBlock));

    drop(guard);
    assert_eq!(try_contents(&store, &timeout), Ok(vec![1]));
}

#[test]
fn foreign_lock_waits_for_a_release_within_the_timeout() {
    let store = Arc::new(LockLikeThing::new(vec![1]));
    let (locked, wait_for_lock) = mpsc::channel();

    thread::scope(|scope| {
        scope.spawn(|| {
            let guard = store.lock();
            locked.send(()).unwrap();
            thread::sleep(Duration::from_millis(20));
            drop(guard);
        });

        wait_for_lock.recv().unwrap();
        assert_eq!(try_contents(&store, &Duration::from_secs(10)), Ok(vec![1]));
    });
}
