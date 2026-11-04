//! Self-made pointer and lock types compose with each other and with the
//! standard ones, for sized and unsized values alike.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use access::{
    Access, AccessError, AccessMut, Lock, MakeMut, OwningPointer, RefCounted, SharedMut,
    SharedPointer,
};

/// A reference-counted pointer in the style of `triomphe::Arc`.
struct MyArc<T: ?Sized>(Arc<T>);

impl<T> MyArc<T> {
    fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T: ?Sized> Clone for MyArc<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: ?Sized> Deref for MyArc<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> SharedPointer for MyArc<T> {}

impl<T: ?Sized> RefCounted for MyArc<T> {
    fn get_mut(this: &mut Self) -> Option<&mut T> {
        Arc::get_mut(&mut this.0)
    }

    fn new(target: T) -> Self
    where
        T: Sized,
    {
        Self::new(target)
    }
}

/// An owning pointer in the style of a custom box.
struct MyBox<T: ?Sized>(Box<T>);

impl<T> MyBox<T> {
    fn new(value: T) -> Self {
        Self(Box::new(value))
    }
}

impl<T: ?Sized> Deref for MyBox<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> DerefMut for MyBox<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: ?Sized> OwningPointer for MyBox<T> {}

/// A lock in the style of `parking_lot::Mutex`, without timeouts.
struct MyMutex<T: ?Sized>(Mutex<T>);

impl<T> MyMutex<T> {
    fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }
}

impl<T: ?Sized> Lock for MyMutex<T> {
    type Inner = T;

    fn read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(self.0.lock()?)
    }

    fn write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(self.0.lock()?)
    }

    fn try_read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(self.0.try_lock()?)
    }

    fn try_write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(self.0.try_lock()?)
    }

    fn inner_mut(&mut self) -> Result<&mut T, AccessError> {
        Ok(self.0.get_mut()?)
    }

    fn new(inner: T) -> Self
    where
        T: Sized,
    {
        Self::new(inner)
    }
}

fn contents<P>(store: &impl Access<Vec<u32>, P>) -> Vec<u32> {
    store.must_get_ref().clone()
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

// Unsized containers (`str`, `Mutex<[u32]>`) need an explicit `?Sized`,
// because type parameters and `impl Trait` arguments are `Sized` by default.

fn text<S: Access<str, P> + ?Sized, P>(store: &S) -> String {
    store.must_get_ref().to_owned()
}

fn shout<S: AccessMut<str, P> + ?Sized, P>(store: &mut S) {
    store.must_get_mut().make_ascii_uppercase();
}

fn total<S: Access<[u32], P> + ?Sized, P>(store: &S) -> u32 {
    store.must_get_ref().iter().sum()
}

fn names<S: Access<[String], P> + ?Sized, P>(store: &S) -> Vec<String> {
    store.must_get_ref().to_vec()
}

fn double<S: SharedMut<[u32], P> + ?Sized, P>(store: &S) {
    store
        .must_get_shared_mut()
        .iter_mut()
        .for_each(|value| *value *= 2);
}

#[test]
fn flat_and_single_custom_layers() {
    let mut plain = vec![1];
    let mut locked = MyMutex::new(vec![1]);
    let mut boxed = MyBox::new(vec![1]);
    let mut counted = MyArc::new(vec![1]);

    push(&mut plain, 2);
    push(&mut locked, 2);
    push(&mut boxed, 2);
    push(&mut counted, 2);

    assert_eq!(contents(&plain), [1, 2]);
    assert_eq!(contents(&locked), [1, 2]);
    assert_eq!(contents(&boxed), [1, 2]);
    assert_eq!(contents(&counted), [1, 2]);
}

#[test]
fn custom_ref_counted_pointer_is_exclusive_only_while_unique() {
    let mut store = MyArc::new(vec![1]);
    let clone = store.clone();
    assert_eq!(try_push(&mut store, 2), Err(AccessError::IsShared));
    drop(clone);
    assert_eq!(try_push(&mut store, 2), Ok(()));
}

#[test]
fn nested_custom_layers_share_writes() {
    let mut store = MyArc::new(MyMutex::new(MyBox::new(vec![1])));
    let clone = store.clone();
    push(&mut store, 2);
    assert_eq!(contents(&clone), [1, 2]);
}

#[test]
fn nested_custom_layers_detach_on_make_mut() {
    let mut store = MyArc::new(MyMutex::new(vec![1]));
    let clone = store.clone();
    append(&mut store, 2);
    assert_eq!(contents(&store), [1, 2]);
    assert_eq!(contents(&clone), [1]);
}

#[test]
fn custom_and_std_layers_mix() {
    let mut first = Arc::new(MyMutex::new(Box::new(vec![1])));
    let mut second = MyArc::new(Mutex::new(MyBox::new(vec![1])));
    let mut third = MyBox::new(Arc::new(MyMutex::new(vec![1])));

    push(&mut first, 2);
    push(&mut second, 2);
    push(&mut third, 2);

    assert_eq!(contents(&first), [1, 2]);
    assert_eq!(contents(&second), [1, 2]);
    assert_eq!(contents(&third), [1, 2]);
}

#[test]
fn unsized_values_behind_pointers() {
    let arc_box: Arc<Box<str>> = Arc::new(Box::from("abc"));
    let arc: Arc<str> = Arc::from("abc");
    let custom: MyArc<MyBox<str>> = MyArc::new(MyBox(Box::from("abc")));
    let slices: Arc<Box<[String]>> = Arc::new(vec!["a".to_owned()].into_boxed_slice());

    assert_eq!(text(&arc_box), "abc");
    assert_eq!(text(&arc), "abc");
    assert_eq!(text(&custom), "abc");
    assert_eq!(names(&slices), ["a"]);

    let mut locked = MyMutex::new(Box::<str>::from("abc"));
    let mut boxed = MyBox(Box::<str>::from("abc"));
    let mut unique: Arc<str> = Arc::from("abc");
    shout(&mut locked);
    shout(&mut boxed);
    shout(&mut unique);
    assert_eq!(text(&locked), "ABC");
    assert_eq!(text(&boxed), "ABC");
    assert_eq!(text(&unique), "ABC");
}

#[test]
fn unsized_values_inside_locks() {
    let std_lock: Arc<Mutex<[u32]>> = Arc::new(Mutex::new([1, 2, 3]));
    let custom_lock: Arc<MyMutex<[u32]>> = Arc::new(MyMutex::new([1, 2, 3]));

    double(&std_lock);
    double(&custom_lock);

    assert_eq!(total(&std_lock), 12);
    assert_eq!(total(&custom_lock), 12);
}

#[test]
fn unsized_containers_directly() {
    let plain: &str = "abc";
    let lock: &Mutex<[u32]> = &Mutex::new([1, 2, 3]);

    assert_eq!(text(plain), "abc");
    assert_eq!(total(lock), 6);
}
