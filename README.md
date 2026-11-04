# rust-access-trait-demo

Uniform shared and exclusive access to a value of type `T`, whether it is
stored directly or behind any nesting of smart pointers and locks:
`T`, `Box<T>`, `Rc<T>`, `Arc<Mutex<T>>`, `Rc<RefCell<Box<T>>>`,
`Mutex<Box<dyn Trait>>`, `Arc<RwLock<[u32]>>` and so on.

Generic code only sees two trait methods and needs no macros:

```rust
use access::{Access, AccessMut};

fn sum<P>(numbers: &impl Access<Vec<u32>, P>) -> u32 {
    numbers.get_ref().iter().sum()
}

fn push<P>(numbers: &mut impl AccessMut<Vec<u32>, P>, number: u32) {
    numbers.get_mut().push(number);
}

push(&mut vec![1], 2);
push(&mut Arc::new(Mutex::new(vec![1])), 2);
push(&mut Mutex::new(Box::new(vec![1])), 2);
sum(&Rc::new(vec![1, 2])); // shared access only
```

## Traits

| Trait             | Method                                            | Implemented when                     |
| ----------------- | ------------------------------------------------- | ------------------------------------ |
| `Access<T, P>`    | `get_ref(&self) -> impl Deref<Target = T>`        | always                               |
| `AccessMut<T, P>` | `get_mut(&mut self) -> impl DerefMut<Target = T>` | no read-only pointer without a lock  |
| `SharedMut<T, P>` | `lock_mut(&self) -> impl DerefMut<Target = T>`    | a lock lies on the path              |

`P` is the path from the container to the value. The compiler always infers
it; generic code declares it as an extra type parameter and never names it.
In a generic struct, put it on the method:

```rust
impl<S> Counter<S> {
    fn increment<P>(&mut self) where S: AccessMut<u64, P> {
        *self.store.get_mut() += 1;
    }
}
```

## Why a path parameter

A blanket `impl<T> Access<T> for T` conflicts with any recursive
`impl Access<T> for Box<X> where X: Access<T>` (E0119). An associated
`Target` with an opt-in marker for leaf types fails for `Box`, which is
`#[fundamental]`. The path parameter keeps the identity case and the composed
cases disjoint, so plain values need no opt-in and every layer composes.

## Composition rules

- A pointer may contain anything, including further pointers and locks.
- A lock may contain the value directly or behind pointers, but no further
  lock: the inner guard would have to borrow from the outer one.
- Exclusive access through a shared pointer (`Rc`, `Arc`, `&`) needs a lock
  further inside.
- `get_mut` on an owned lock uses the lock’s own `get_mut` and does not lock.
- Poisoned locks and conflicting `RefCell` borrows panic, like
  `lock().unwrap()`.

## Own pointer and lock types

Implement one of three traits; the new type then composes with all others:

```rust
impl<T: ?Sized> SharedPointer for MyArc<T> {}   // read-only pointer, needs Deref
impl<T: ?Sized> OwningPointer for MyBox<T> {}   // owning pointer, needs DerefMut

impl<T: ?Sized> Lock for MyMutex<T> {
    type Inner = T;
    fn read(&self) -> impl Deref<Target = T> { self.0.lock().unwrap() }
    fn write(&self) -> impl DerefMut<Target = T> { self.0.lock().unwrap() }
    fn inner_mut(&mut self) -> &mut T { self.0.get_mut().unwrap() }
}
```

For types from other crates, such as `triomphe::Arc` or
`parking_lot::Mutex`, the orphan rule only allows these impls in the crate
that defines the traits. Copy `src/lib.rs` into your own crate as a module,
or wrap the foreign type in a newtype.

## Unsized values

All impls accept unsized values: `Arc<Box<str>>`, `Arc<str>`,
`Mutex<Box<dyn Trait>>` and `Arc<Mutex<[u32]>>` work as they are. When the
container itself is unsized (`str`, `Mutex<[u32]>`), the generic function
must allow it explicitly, because type parameters are `Sized` by default:

```rust
fn text<S: Access<str, P> + ?Sized, P>(store: &S) -> String {
    store.get_ref().to_owned()
}
```

## Limitations

- Calls on concrete types without a target are ambiguous, since
  `Arc<Mutex<u32>>` can also be accessed as `Mutex<u32>` or as itself. Use
  `Access::<u32, _>::get_ref(&arc)`. Generic code is not affected.
- On concrete types, inherent methods win: `mutex.get_mut()` calls
  `Mutex::get_mut`.
- Return-position `impl Trait` makes the traits not dyn-compatible.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
