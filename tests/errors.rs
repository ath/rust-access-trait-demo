//! A failed access reports why it failed, and the `must_*` methods turn the
//! error into a panic.

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{Arc, Mutex, RwLock};

use access::{Access, AccessError, AccessMut, SharedMut, TryLock};

fn try_contents<P>(store: &impl Access<Vec<u32>, P>) -> Result<Vec<u32>, AccessError> {
    Ok(store.try_get_ref()?.clone())
}

fn try_push<P>(store: &mut impl AccessMut<Vec<u32>, P>, value: u32) -> Result<(), AccessError> {
    store.try_get_mut()?.push(value);
    Ok(())
}

fn push<P>(store: &mut impl AccessMut<Vec<u32>, P>, value: u32) {
    store.must_get_mut().push(value);
}

/// Panics while the lock is held for writing, which poisons it.
fn poison<P>(store: &impl SharedMut<Vec<u32>, P>) {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let _guard = store.must_get_shared_mut();
        panic!("poisoning the lock");
    }));
    assert!(result.is_err());
}

#[test]
fn shared_pointer_without_lock_is_exclusive_only_while_unique() {
    let mut store = Rc::new(vec![1]);
    assert_eq!(try_push(&mut store, 2), Ok(()));

    let clone = Rc::clone(&store);
    assert_eq!(try_push(&mut store, 3), Err(AccessError::IsShared));

    drop(clone);
    assert_eq!(try_push(&mut store, 3), Ok(()));
    assert_eq!(*store, [1, 2, 3]);
}

#[test]
fn weak_pointers_count_as_sharing() {
    let mut store = Arc::new(vec![1]);
    let weak = Arc::downgrade(&store);
    assert_eq!(try_push(&mut store, 2), Err(AccessError::IsShared));

    drop(weak);
    assert_eq!(try_push(&mut store, 2), Ok(()));
}

#[test]
fn shared_pointer_with_lock_inside_is_never_shared() {
    let mut store = Rc::new(RefCell::new(vec![1]));
    let clone = Rc::clone(&store);
    assert_eq!(try_push(&mut store, 2), Ok(()));
    assert_eq!(try_contents(&clone), Ok(vec![1, 2]));
}

#[test]
fn conflicting_refcell_borrows_would_block_instead_of_panicking() {
    let mut store = Rc::new(RefCell::new(vec![1]));
    let clone = Rc::clone(&store);

    let reader = clone.borrow();
    assert_eq!(try_push(&mut store, 2), Err(AccessError::WouldBlock));
    assert_eq!(try_contents(&store), Ok(vec![1]));
    drop(reader);

    let writer = clone.borrow_mut();
    assert_eq!(try_contents(&store), Err(AccessError::WouldBlock));
    drop(writer);

    assert_eq!(try_push(&mut store, 2), Ok(()));
}

#[test]
fn poisoned_locks_report_poisoned() {
    let mut mutex = Arc::new(Mutex::new(vec![1]));
    let mut rwlock = Arc::new(RwLock::new(vec![1]));
    poison(&mutex);
    poison(&rwlock);

    assert_eq!(try_contents(&mutex), Err(AccessError::Poisoned));
    assert_eq!(try_contents(&rwlock), Err(AccessError::Poisoned));
    assert_eq!(try_push(&mut mutex, 2), Err(AccessError::Poisoned));
    assert_eq!(try_push(&mut rwlock, 2), Err(AccessError::Poisoned));
    assert_eq!(
        mutex.try_get_shared_mut_locked(&TryLock).err(),
        Some(AccessError::Poisoned)
    );
}

#[test]
fn poisoned_owned_lock_reports_poisoned_without_locking() {
    let mut store = Mutex::new(vec![1]);
    poison(&store);
    assert_eq!(try_push(&mut store, 2), Err(AccessError::Poisoned));
}

#[test]
#[should_panic(expected = "access failed: the value is shared and cannot be accessed exclusively")]
fn must_panics_with_the_error() {
    let mut store = Rc::new(vec![1]);
    let _clone = Rc::clone(&store);
    push(&mut store, 2);
}

#[test]
fn errors_are_displayed_as_sentences() {
    let error: Box<dyn std::error::Error> = Box::new(AccessError::WouldBlock);
    assert_eq!(
        error.to_string(),
        "the value is locked and access would block"
    );
}
