//! Generic code reaches the value in every supported container combination
//! through the same trait methods.

use std::cell::{Cell, RefCell};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use access::{Access, AccessMut, Lock, SharedMut};

fn contents<P>(store: &impl Access<Vec<u32>, P>) -> Vec<u32> {
    store.get_ref().clone()
}

fn push<P>(store: &mut impl AccessMut<Vec<u32>, P>, value: u32) {
    store.get_mut().push(value);
}

#[test]
fn plain_value() {
    let mut store = vec![1];
    push(&mut store, 2);
    assert_eq!(contents(&store), [1, 2]);
}

#[test]
fn arc_mutex_shares_writes_with_its_clones() {
    let mut store = Arc::new(Mutex::new(vec![1]));
    let clone = Arc::clone(&store);
    push(&mut store, 2);
    assert_eq!(contents(&clone), [1, 2]);
}

#[test]
#[allow(clippy::arc_with_non_send_sync)] // the combination itself is under test
fn arc_refcell() {
    let mut store = Arc::new(RefCell::new(vec![1]));
    push(&mut store, 2);
    assert_eq!(contents(&store), [1, 2]);
}

#[test]
fn mutex_box() {
    let mut store = Mutex::new(Box::new(vec![1]));
    push(&mut store, 2);
    assert_eq!(contents(&store), [1, 2]);
}

#[test]
fn rwlock() {
    let mut store = RwLock::new(vec![1]);
    push(&mut store, 2);
    assert_eq!(contents(&store), [1, 2]);
}

#[test]
fn rc_grants_shared_access() {
    let store = Rc::new(vec![1, 2]);
    assert_eq!(contents(&store), [1, 2]);
}

#[test]
fn rc_refcell_box_shares_writes_with_its_clones() {
    let mut store = Rc::new(RefCell::new(Box::new(vec![1])));
    let clone = Rc::clone(&store);
    push(&mut store, 2);
    assert_eq!(contents(&clone), [1, 2]);
}

#[test]
#[allow(clippy::mut_mut)] // `&mut Vec<u32>` itself is the container under test
fn references() {
    let mut value = vec![1];
    let mut exclusive = &mut value;
    push(&mut exclusive, 2);
    assert_eq!(contents(&exclusive), [1, 2]);

    let mutex = Mutex::new(vec![1]);
    let mut shared = &mutex;
    push(&mut shared, 2);
    assert_eq!(contents(&shared), [1, 2]);
}

#[test]
fn deep_nesting() {
    let mut store = Box::new(Arc::new(RwLock::new(Box::new(vec![1]))));
    push(&mut store, 2);
    assert_eq!(contents(&store), [1, 2]);
}

trait Shape {
    fn area(&self) -> f64;
    fn scale(&mut self, factor: f64);
}

struct Square(f64);

impl Shape for Square {
    fn area(&self) -> f64 {
        self.0 * self.0
    }

    fn scale(&mut self, factor: f64) {
        self.0 *= factor;
    }
}

fn area<P>(shape: &impl Access<dyn Shape, P>) -> f64 {
    shape.get_ref().area()
}

fn scale<P>(shape: &mut impl AccessMut<dyn Shape, P>, factor: f64) {
    shape.get_mut().scale(factor);
}

#[test]
fn unsized_values() {
    let mut boxed: Mutex<Box<dyn Shape>> = Mutex::new(Box::new(Square(1.0)));
    let mut shared: Arc<Mutex<dyn Shape>> = Arc::new(Mutex::new(Square(1.0)));
    scale(&mut boxed, 2.0);
    scale(&mut shared, 3.0);
    assert_eq!(area(&boxed), 4.0);
    assert_eq!(area(&shared), 9.0);
}

fn increment<P>(counter: &impl SharedMut<u64, P>) {
    *counter.lock_mut() += 1;
}

#[test]
fn shared_mut_across_threads() {
    let counter = Arc::new(Mutex::new(0_u64));
    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| (0..1000).for_each(|_| increment(&counter)));
        }
    });
    assert_eq!(*counter.lock().unwrap(), 4000);
}

struct Counter<S> {
    store: S,
}

impl<S> Counter<S> {
    fn increment<P>(&mut self)
    where
        S: AccessMut<u64, P>,
    {
        *self.store.get_mut() += 1;
    }

    fn value<P>(&self) -> u64
    where
        S: Access<u64, P>,
    {
        *self.store.get_ref()
    }
}

#[test]
fn generic_struct_over_the_storage() {
    let mut plain = Counter { store: 0_u64 };
    let mut local = Counter {
        store: Rc::new(RefCell::new(0_u64)),
    };
    let mut shared = Counter {
        store: Arc::new(RwLock::new(0_u64)),
    };
    plain.increment();
    local.increment();
    shared.increment();
    shared.increment();
    assert_eq!(plain.value() + local.value() + shared.value(), 4);
}

/// A lock defined outside the crate that counts how often it is acquired for
/// writing.
struct CountingCell<T> {
    value: RefCell<T>,
    writes: Cell<usize>,
}

impl<T> CountingCell<T> {
    fn new(value: T) -> Self {
        Self {
            value: RefCell::new(value),
            writes: Cell::new(0),
        }
    }
}

impl<T> Lock for CountingCell<T> {
    type Inner = T;

    fn read(&self) -> impl Deref<Target = T> {
        self.value.borrow()
    }

    fn write(&self) -> impl DerefMut<Target = T> {
        self.writes.set(self.writes.get() + 1);
        self.value.borrow_mut()
    }

    fn inner_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

#[test]
fn custom_lock_composes_with_std_pointers() {
    let mut store = Rc::new(CountingCell::new(vec![1]));
    push(&mut store, 2);
    assert_eq!(contents(&store), [1, 2]);
    assert_eq!(store.writes.get(), 1);
}

#[test]
fn exclusive_access_to_an_owned_lock_skips_locking() {
    let mut store = CountingCell::new(vec![1]);
    push(&mut store, 2);
    assert_eq!(contents(&store), [1, 2]);
    assert_eq!(store.writes.get(), 0);
}
