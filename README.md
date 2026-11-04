# rust-access-trait-demo

Uniform shared and exclusive access to a value of type `T`, whether it is
stored directly or behind any nesting of smart pointers and locks: `T`,
`Box<T>`, `Rc<T>`, `Arc<Mutex<T>>`, `Rc<RefCell<Box<T>>>`,
`Mutex<Box<dyn Trait>>`, `Arc<RwLock<[u32]>>` and so on. Self-made pointer
and lock types plug in through three small traits and then compose with all
others.

Generic code only sees trait methods; there are no macros, neither on the
user side nor internally. The crate uses no `unsafe` code.

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
push(&mut Rc::new(RefCell::new(vec![1])), 2);
push(&mut RwLock::new(vec![1]), 2);
sum(&Rc::new(vec![1, 2])); // shared access only
```

## Public API

```rust
pub trait Access<T: ?Sized, P> {
    fn get_ref(&self) -> impl Deref<Target = T>;
}

pub trait AccessMut<T: ?Sized, P>: Access<T, P> {
    fn get_mut(&mut self) -> impl DerefMut<Target = T>;
}

/// Exclusive access through `&self` whenever a lock lies on the path.
pub trait SharedMut<T: ?Sized, P>: Access<T, P> {
    fn lock_mut(&self) -> impl DerefMut<Target = T>;
}

// Extension points
pub trait SharedPointer: Deref {}    // Rc, Arc, &T
pub trait OwningPointer: DerefMut {} // Box, &mut T
pub trait Lock {                     // Mutex, RwLock, RefCell
    type Inner: ?Sized;
    fn read(&self) -> impl Deref<Target = Self::Inner>;
    fn write(&self) -> impl DerefMut<Target = Self::Inner>;
    fn inner_mut(&mut self) -> &mut Self::Inner;
}
```

| Trait             | Available for                                         |
| ----------------- | ----------------------------------------------------- |
| `Access<T, P>`    | every supported combination                           |
| `AccessMut<T, P>` | all except read-only pointers without a lock inside   |
| `SharedMut<T, P>` | combinations with a lock on the path                  |

Everything else lives in a private module: the path markers, the guard-free
helper traits `Project` and `ProjectMut`, the guard wrapper and the blanket
impls. These items cannot be named from outside, and compiler errors only
mention `Access<u32, _>` and the like.

## The path parameter `P`

`P` describes how `T` is reached inside the container. For
`Arc<Mutex<Box<T>>>` it is `ViaShared<ViaLock<ViaOwned<Here>>>`. The
compiler always infers it; generic code declares it as an extra type
parameter and never names a concrete path.

In a generic struct, put `P` on the method. On the impl block it would be
unconstrained (E0207):

```rust
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
}
```

## Design

### Plain values versus composition

Supporting a plain `T` next to composed containers is the core problem.
Without specialization, a blanket `impl<T> Access<T> for T` conflicts with
any recursive impl such as
`impl<T, X: Access<T>> Access<T> for Box<X>` (E0119, for `Box` as well as
for `Arc`).

The usual alternative, an associated `Target` type plus an opt-in marker
trait for leaf types, fails as well: `Box` is `#[fundamental]`, so the
compiler reports “downstream crates may implement trait `Leaf` for type
`Box<_>`”. It would also force every leaf type to opt in.

The path parameter solves both. The identity impl uses the path `Here`, every
layer kind uses its own marker (`ViaShared`, `ViaOwned`, `ViaLock`), so all
impls are disjoint. Since a finite type contains a given `T` at only one
depth, the compiler infers a unique path whenever `T` is known. The price is
the extra type parameter in generic bounds.

### Two traits instead of one

`Rc<T>` and `Arc<T>` without a lock inside only grant shared access, so
`get_ref` and `get_mut` live in separate traits: `Access` and
`AccessMut: Access`. `Rc<T>` implements only `Access`, `Rc<RefCell<T>>`
both. Writing through `Rc<T>` would be possible with `Rc::make_mut`
(copy-on-write, `T: Clone`), but that changes the semantics and is
therefore left out.

### No lock inside a lock

A guard for an inner lock would have to borrow from the guard of the outer
lock, which is self-referential and impossible in safe Rust. Hence the
composition rules:

- A pointer may contain anything, including further pointers and locks.
- A lock may contain the value directly or behind pointers (`T`, `Box<T>`,
  for reading also `Rc<T>` and `Arc<T>`), but no further lock.
- Exclusive access through a shared pointer (`Rc`, `Arc`, `&`) needs a lock
  further inside.

Inside a lock, the value is reached through `Project`/`ProjectMut`, which
return plain references; the guard wrapper projects on every dereference.

### Locking behavior

- `get_mut(&mut self)` on a lock the container owns (`Mutex<T>`,
  `Box<RwLock<T>>`) uses the lock’s own `get_mut` and does not lock at all.
  Behind `Arc` or `Rc` the lock is acquired.
- `get_ref` on an `RwLock` takes a read lock; on a `Mutex` the full lock.
- Poisoned locks panic, like the usual `lock().unwrap()`. The policy sits in
  a single helper (`unpoisoned`); `unwrap_or_else(PoisonError::into_inner)`
  ignores poisoning instead.
- Conflicting `RefCell` borrows panic as usual.

## Own pointer and lock types

Implement one of the extension traits; the new type then composes with all
others, including the standard ones (`Arc<MyMutex<Box<T>>>`,
`MyArc<Mutex<MyBox<T>>>`, …):

```rust
impl<T: ?Sized> SharedPointer for MyArc<T> {} // read-only pointer, needs Deref
impl<T: ?Sized> OwningPointer for MyBox<T> {} // owning pointer, needs DerefMut

impl<T: ?Sized> Lock for MyMutex<T> {
    type Inner = T;

    fn read(&self) -> impl Deref<Target = T> {
        self.0.lock().unwrap()
    }

    fn write(&self) -> impl DerefMut<Target = T> {
        self.0.lock().unwrap()
    }

    fn inner_mut(&mut self) -> &mut T {
        self.0.get_mut().unwrap()
    }
}
```

The `?Sized` in these impls is what allows the type to hold unsized values.

### Types from other crates

For types such as `triomphe::Arc` or `parking_lot::Mutex`, neither the trait
nor the type belongs to the user’s crate, and the orphan rule rejects the
impl (E0117). This holds for every trait-based design in Rust. There are
three ways out:

1. Copy `src/lib.rs` into your own crate as a module. The traits are then
   local, and the impl works:

   ```rust
   impl<T: ?Sized> Lock for parking_lot::Mutex<T> {
       type Inner = T;
       fn read(&self) -> impl Deref<Target = T> { self.lock() }
       fn write(&self) -> impl DerefMut<Target = T> { self.lock() }
       fn inner_mut(&mut self) -> &mut T { self.get_mut() }
   }
   ```

2. Add optional features to this crate (for example `triomphe`,
   `parking_lot`) that provide the impls.
3. Wrap the foreign type in a newtype.

## Unsized values

All impls accept unsized values, so sized and unsized values need no
separate treatment. `Arc<Box<str>>`, `Arc<str>`, `Arc<Box<[String]>>`,
`Mutex<Box<dyn Trait>>` and `Arc<Mutex<[u32]>>` work as they are:

```rust
fn area<P>(shape: &impl Access<dyn Shape, P>) -> f64 {
    shape.get_ref().area()
}

let boxed: Mutex<Box<dyn Shape>> = Mutex::new(Box::new(Square(2.0)));
area(&boxed);
```

Only when the container itself is unsized (`str`, `Mutex<[u32]>`) must the
generic function allow it explicitly, because type parameters and
`impl Trait` arguments are `Sized` by default:

```rust
fn text<S: Access<str, P> + ?Sized, P>(store: &S) -> String {
    store.get_ref().to_owned()
}

text("abc");
```

`[str]` is not a valid type: slice elements must be `Sized`. Use `Box<str>`
or `[String]` instead.

## Limitations

- Calls on concrete types without a target are ambiguous (E0282), since
  `Arc<Mutex<u32>>` can also be accessed as `Mutex<u32>` or as itself. Name
  the target explicitly: `Access::<u32, _>::get_ref(&arc)`. Generic code is
  not affected.
- On concrete types, inherent methods win: `mutex.get_mut()` calls
  `Mutex::get_mut`, and `map.get_mut()` on a `HashMap` fails with E0061.
  Again, generic code is not affected.
- Return-position `impl Trait` makes the traits not dyn-compatible; there is
  no `dyn Access<…>`.
- `Arc<RefCell<T>>` works but is neither `Send` nor `Sync`, so it behaves
  like `Rc<RefCell<T>>`.
- Calling `get_mut` on a clone of an `Arc<Mutex<T>>` while a `get_ref`
  guard of the same mutex is alive in the same thread deadlocks or panics,
  as with plain `lock()` calls.

## Tests

```bash
cargo test
```

- `tests/acceptance.rs`: all standard combinations, references, deep
  nesting, `dyn` values, `SharedMut` across threads, a generic struct and a
  custom lock, including that an owned lock is not locked by `get_mut`.
- `tests/custom_layers.rs`: self-made pointer and lock types alone, nested
  and mixed with standard ones, plus unsized values behind pointers, inside
  locks and as containers.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
