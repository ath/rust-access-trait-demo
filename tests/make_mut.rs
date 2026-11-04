//! Copy-on-write access detaches shared pointers, passes through owning
//! pointers and does not lock owned locks.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, RwLock};

use access::{AccessError, Block, LockMethod, MakeMut, TryLock};

fn append<P>(store: &mut impl MakeMut<Vec<u32>, P>, value: u32) {
    store.must_make_mut().push(value);
}

fn try_append<P, M: LockMethod>(
    store: &mut impl MakeMut<Vec<u32>, P>,
    method: &M,
    value: u32,
) -> Result<(), AccessError> {
    store.try_make_mut_locked(method)?.push(value);
    Ok(())
}

#[test]
fn shared_pointer_is_detached_from_its_clones() {
    let mut store = Rc::new(vec![1]);
    let clone = Rc::clone(&store);
    append(&mut store, 2);
    assert_eq!(*store, [1, 2]);
    assert_eq!(*clone, [1]);
}

#[test]
fn unique_pointer_is_not_copied() {
    let mut store = Arc::new(vec![1]);
    let address = Arc::as_ptr(&store);
    append(&mut store, 2);
    assert_eq!(Arc::as_ptr(&store), address);
}

#[test]
fn lock_behind_a_shared_pointer_is_copied_under_its_lock() {
    let mut store = Arc::new(Mutex::new(vec![1]));
    let clone = Arc::clone(&store);
    append(&mut store, 2);
    assert_eq!(*store.lock().unwrap(), [1, 2]);
    assert_eq!(*clone.lock().unwrap(), [1]);
}

#[test]
fn nested_shared_pointers_are_all_detached() {
    let inner = Rc::new(vec![1]);
    let mut store = Rc::new(Rc::clone(&inner));
    let clone = Rc::clone(&store);
    append(&mut store, 2);
    assert_eq!(**store, [1, 2]);
    assert_eq!(**clone, [1]);
    assert_eq!(*inner, [1]);
}

#[test]
fn shared_pointer_inside_an_owned_lock_is_detached() {
    let shared = Rc::new(vec![1]);
    let mut store = RefCell::new(Rc::clone(&shared));
    append(&mut store, 2);
    assert_eq!(**store.borrow(), [1, 2]);
    assert_eq!(*shared, [1]);
}

/// A value that cannot be cloned.
#[derive(Debug, PartialEq)]
struct Token(u32);

fn bump<P>(store: &mut impl MakeMut<Token, P>) {
    store.must_make_mut().0 += 1;
}

#[test]
fn paths_without_shared_pointers_need_no_clone() {
    let mut plain = Token(1);
    let mut boxed = Box::new(Token(1));
    let mut locked = Mutex::new(Box::new(Token(1)));
    let mut cell = RefCell::new(Token(1));

    bump(&mut plain);
    bump(&mut boxed);
    bump(&mut locked);
    bump(&mut cell);

    assert_eq!(plain, Token(2));
    assert_eq!(*boxed, Token(2));
    assert_eq!(**locked.lock().unwrap(), Token(2));
    assert_eq!(*cell.borrow(), Token(2));
}

#[test]
fn copying_a_taken_lock_fails_with_try_lock() {
    let mut store = Arc::new(RwLock::new(vec![1]));
    let clone = Arc::clone(&store);

    let writer = clone.write().unwrap();
    assert_eq!(
        try_append(&mut store, &TryLock, 2),
        Err(AccessError::WouldBlock)
    );
    drop(writer);

    assert_eq!(try_append(&mut store, &TryLock, 2), Ok(()));
    assert_eq!(*store.read().unwrap(), [1, 2]);
    assert_eq!(*clone.read().unwrap(), [1]);
}

#[test]
fn copying_a_mutably_borrowed_cell_would_block() {
    let mut store = Rc::new(RefCell::new(vec![1]));
    let clone = Rc::clone(&store);
    let writer = clone.borrow_mut();
    assert_eq!(
        try_append(&mut store, &Block, 2),
        Err(AccessError::WouldBlock)
    );
    drop(writer);
    assert_eq!(try_append(&mut store, &Block, 2), Ok(()));
}

#[test]
fn unsized_value_behind_an_owning_pointer() {
    fn shout<P>(store: &mut impl MakeMut<str, P>) {
        store.must_make_mut().make_ascii_uppercase();
    }

    let mut store: Rc<Box<str>> = Rc::new(Box::from("abc"));
    let clone = Rc::clone(&store);
    shout(&mut store);
    assert_eq!(&**store, "ABC");
    assert_eq!(&**clone, "abc");
}
