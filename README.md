# rust-access-trait-demo

Uniform, fallible shared and exclusive access to a value of type `T`,
whether it is stored directly or behind any nesting of smart pointers and
locks: `T`, `Box<T>`, `Rc<T>`, `Arc<Mutex<T>>`, `Rc<RefCell<Box<T>>>`,
`Mutex<Box<dyn Trait>>`, `Arc<RwLock<[u32]>>` and so on. Self-made pointer
and lock types plug in through small traits and then compose with all
others; types from other crates join through a local glue type.

Generic code only sees trait methods; there are no macros on the user side.
The crate uses no `unsafe` code.

```rust
use access::{Access, AccessError, AccessMut};

fn sum<P>(numbers: &impl Access<Vec<u32>, P>) -> u32 {
    numbers.must_get_ref().iter().sum()
}

fn try_push<P>(
    numbers: &mut impl AccessMut<Vec<u32>, P>,
    number: u32,
) -> Result<(), AccessError> {
    numbers.try_get_mut()?.push(number);
    Ok(())
}

try_push(&mut vec![1], 2)?;
try_push(&mut Arc::new(Mutex::new(vec![1])), 2)?;
try_push(&mut Mutex::new(Box::new(vec![1])), 2)?;
try_push(&mut Rc::new(RefCell::new(vec![1])), 2)?;
try_push(&mut RwLock::new(vec![1]), 2)?;
sum(&Rc::new(vec![1, 2]));

let mut counted = Rc::new(vec![1]);
let other = Rc::clone(&counted);
assert_eq!(try_push(&mut counted, 2), Err(AccessError::IsShared));
```

## Public API

```rust
// Access traits. `P` is the path to `T` and always inferred.
pub trait Access<T: ?Sized, P> {
    fn try_get_ref_locked<'a, M: LockMethod>(&'a self, method: &M)
        -> Result<impl Deref<Target = T> + use<'a, Self, T, P, M>, AccessError>;
    fn try_get_ref(&self) -> Result<impl Deref<Target = T>, AccessError>;
    fn must_get_ref_locked<'a, M: LockMethod>(&'a self, method: &M)
        -> impl Deref<Target = T> + use<'a, Self, T, P, M>;
    fn must_get_ref(&self) -> impl Deref<Target = T>;
}
pub trait AccessMut<T: ?Sized, P>: Access<T, P> { /* {try,must}_get_mut[_locked] */ }
pub trait SharedMut<T: ?Sized, P>: Access<T, P> { /* {try,must}_get_shared_mut[_locked] */ }
pub trait MakeMut<T: ?Sized, P>: Access<T, P> { /* {try,must}_make_mut[_locked] -> &mut T */ }

pub enum AccessError { IsShared, WouldBlock, Poisoned }

// Lock methods. `LockMethod` is sealed.
pub struct Block;
pub struct TryLock;
pub trait LockMethod {} // Block, TryLock, std::time::Duration, std::time::Instant

// Extension points.
pub trait TraitGlue: 'static {}
pub struct Builtin;
pub trait SharedPointer<G: TraitGlue = Builtin>: Deref {}      // Rc, Arc, &T
pub trait RefCounted<G: TraitGlue = Builtin>: SharedPointer<G> { // Rc, Arc
    fn get_mut(this: &mut Self) -> Option<&mut Self::Target>;
    fn new(target: Self::Target) -> Self where Self: Sized, Self::Target: Sized;
}
pub trait OwningPointer<G: TraitGlue = Builtin>: DerefMut {}   // Box, &mut T
pub trait Lock<G: TraitGlue = Builtin> { /* see below */ }       // Mutex, RwLock, RefCell
```

Every access trait has one required method and three provided ones:

| Method                | Lock method | On failure                 |
| --------------------- | ----------- | -------------------------- |
| `try_get_ref_locked`  | argument    | returns `Err(AccessError)` |
| `try_get_ref`         | `Block`     | returns `Err(AccessError)` |
| `must_get_ref_locked` | argument    | panics                     |
| `must_get_ref`        | `Block`     | panics                     |

| Trait             | Grants                               | Available for                                       |
| ----------------- | ------------------------------------ | --------------------------------------------------- |
| `Access<T, P>`    | shared access, guard                 | every supported combination                         |
| `AccessMut<T, P>` | exclusive access via `&mut`, guard   | all but `&T` without a lock inside, `Rc` in a lock  |
| `SharedMut<T, P>` | exclusive access via `&`, guard      | combinations with a lock on the path                |
| `MakeMut<T, P>`   | copy-on-write access, plain `&mut T` | paths without `&T`; copied values must be `Clone`   |

The `must_*` methods panic with “access failed: …” and report the location
of their caller (`#[track_caller]`).

Everything else lives in a private module: the path markers, the guard-free
helper traits, the guard wrapper, the lock method dispatch and the blanket
impls. These items cannot be named from outside, and compiler errors explain
themselves through `#[diagnostic::on_unimplemented]`:

```text
error[E0277]: `Rc<Token>` grants no copy-on-write access to `Token`
  = note: behind `Rc`, `Arc` and other `RefCounted` pointers, the value must be
          `Clone` and `Sized`, or a lock whose content is `Clone`
```

## Errors

| Error        | Cause                                                                        |
| ------------ | ---------------------------------------------------------------------------- |
| `IsShared`   | exclusive access through `Rc` without a lock while other `Rc`/`Weak` live    |
| `WouldBlock` | a lock stays taken beyond the limit of the lock method; a `RefCell` conflict |
| `Poisoned`   | a `Mutex` or `RwLock` was poisoned by a panic                                |

`AccessError` implements `Display` and `Error`, plus `From` for
`PoisonError`, `TryLockError`, `BorrowError` and `BorrowMutError`, so that
`Lock` impls can use `?`.

Exclusive access through a shared pointer depends on the rest of the path:

- With a lock further inside (`Arc<Mutex<T>>`, `Rc<RefCell<T>>`), the lock
  is acquired, and writes reach all pointers to the same target.
- Without a lock (`Rc<T>`, `Arc<Box<T>>`), access is granted while the
  pointer is unique (`Rc::get_mut`); otherwise the result is `IsShared`.

## Lock methods

A lock method sets an upper bound for waiting:

| Lock method          | Waits                      | `Lock` methods              |
| -------------------- | -------------------------- | --------------------------- |
| `Block`              | as long as necessary       | `read`, `write`             |
| `TryLock`            | not at all                 | `try_read`, `try_write`     |
| `Duration` (timeout) | at most the timeout        | `read_for`, `write_for`     |
| `Instant` (deadline) | at most until the deadline | `read_until`, `write_until` |

A lock may give up earlier, but it never waits longer. All methods without
the `_locked` suffix use `Block`. Exclusive access through `&mut` to an owned
lock (`Mutex<T>`, `Box<RwLock<T>>`) uses the lock’s own `get_mut`: it never
waits and ignores the method.

| Lock              | `Block`     | `TryLock`   | `Duration`, `Instant` |
| ----------------- | ----------- | ----------- | --------------------- |
| `Mutex`, `RwLock` | waits       | never waits | build error (E0080)   |
| `RefCell`         | never waits | never waits | never waits           |

`RefCell` never waits: waiting for a borrow in the same thread could only
deadlock, so a conflicting borrow returns `WouldBlock` at once instead of
panicking like `borrow_mut`.

The dispatch is static. `LockMethod` is sealed by a private supertrait whose
impls for `Block`, `TryLock`, `Duration` and `Instant` each call their own
`Lock` method; monomorphization selects it at compile time.

### Unsupported lock methods fail the build

`std::sync::Mutex` and `RwLock` cannot wait with a timeout. Their
`read_for`/`write_for` are tagged with `unsupported!()` from the
[`unsupported`](https://crates.io/crates/unsupported) crate, which panics in
a `const` block inside a generic `const fn`. The panic is evaluated only
when the code that reaches it is monomorphized, so a program that uses
`Duration` with a `Mutex` fails to compile, while all other programs compile
cleanly:

```text
error[E0080]: evaluation panicked: "`std::sync::Mutex` and `std::sync::RwLock`
              cannot wait with a timeout or deadline"
```

The optional methods of `Lock` (`try_*`, `*_for`) default to the same kind
of error, so a custom lock implements only what it supports.

Trait bounds cannot express this check. The access methods accept any
`M: LockMethod`, independent of the container; a bound such as “every lock
on the path supports `M`” would have to appear in every generic signature
and would expose the path. Post-monomorphization errors come with caveats:

- `cargo check` and rust-analyzer do not report them; `cargo build` and
  `cargo test` do.
- The error names the unsupported lock method, not the call site in user
  code.
- Builds with `-C link-dead-code`, as used by some coverage setups, compile
  default methods of non-generic `Lock` impls even if nothing calls them.
  Such impls must implement all optional methods there.
- In a non-generic impl, an explicit method body with `unsupported!()` is
  compiled even if never called. Leave unsupported methods out and rely on
  the defaults instead.

## Copy-on-write access

`MakeMut` returns a plain `&mut T` to a value that the container owns
alone, like `Rc::make_mut`:

| Layer on the path              | `try_make_mut`                                                     |
| ------------------------------ | ------------------------------------------------------------------ |
| the value                      | returns it                                                         |
| owning pointer (`Box`, `&mut`) | passes through, copies nothing                                     |
| lock reached through `&mut`    | `inner_mut`, no locking                                            |
| unique `RefCounted` pointer    | passes through (`get_mut`)                                         |
| shared `RefCounted` pointer    | replaced by a pointer to a copy; other pointers keep the old value |
| `&T`                           | not supported: it cannot point to a copy                           |

Copies are made with `Clone`, and only where a shared pointer may need one:
the target of a shared pointer must be `Clone`, or a lock whose content is
`Clone`. That lock is acquired with the lock method to read the value, so
`try_make_mut_locked(&TryLock)` fails with `WouldBlock` instead of waiting.
Nested shared pointers are cloned shallowly and detached in turn. Paths
without shared pointers (`T`, `Box<T>`, `Mutex<T>`) need no `Clone` at all.

```rust
fn append<P>(numbers: &mut impl MakeMut<Vec<u32>, P>, number: u32) {
    numbers.must_make_mut().push(number);
}

let mut shared = Arc::new(Mutex::new(vec![1]));
let other = Arc::clone(&shared);
append(&mut shared, 2); // locks the shared mutex to copy the vector
assert_eq!(*other.lock().unwrap(), [1]);
```

Unsized values behind a shared pointer (`Rc<str>`, `Arc<[u32]>`) have no
`MakeMut`, since generic code cannot copy them. `Rc<Box<str>>` works.

## The path parameter `P`

`P` describes how `T` is reached inside the container. For
`Arc<Mutex<Box<T>>>` it is
`ViaShared<Builtin, ViaLock<Builtin, ViaOwned<Builtin, Here>>>`. The compiler
always infers it; generic code declares it as an extra type parameter and
never names a concrete path.

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
        *self.store.must_get_mut() += 1;
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

### Why `T` is not inferred

Inferring `T` from the container by ordered rules (`Pointer<Lock<T>>`, then
`Lock<Pointer<T>>`, …, then `T`) needs overlapping impls with priorities,
which is specialization. Autoref-based specialization does not help: it
ranks method candidates for a concrete receiver expression, typically inside
a macro, and generic code resolves methods through its bounds alone. It can
neither run in generic functions nor compute a type such as the `T` of a
bound. Generic code knows the type it works on, so it names `T`.

### Selecting by the rest of the path

Exclusive access through a shared pointer locks a lock further inside or
checks uniqueness. Both impls would overlap on `AccessMut<T, ViaShared<G, P>>`.
A type-level function on the path (`PathInfo::HasLock`, `Yes` or `No`)
selects one of two impls of a private helper trait that differ in that
parameter, so they are disjoint without specialization.

### No lock inside a lock

A guard for an inner lock would have to borrow from the guard of the outer
lock, which is self-referential and impossible in safe Rust. Hence the
composition rules:

- A pointer may contain anything, including further pointers and locks.
- A lock may contain the value directly or behind pointers (`T`, `Box<T>`,
  for reading also `Rc<T>` and `Arc<T>`), but no further lock.
- Exclusive access through a shared pointer either goes through a lock
  further inside or requires a unique `RefCounted` pointer.

Inside a lock, the value is reached through guard-free projections that
return plain references; the guard wrapper projects on every dereference.

### Precise capturing

The guards borrow the container, not the lock method:
`impl Deref<Target = T> + use<'a, Self, T, P, M>`. Callers can therefore pass
temporaries such as `&Duration::from_millis(10)` and keep the guard. `use<..>`
must name every type parameter, and argument-position `impl Trait` has no
name, so the methods take `method: &M` with `M: LockMethod` instead of
`&impl LockMethod`. Precise capturing in traits requires Rust 1.87.

## Own pointer and lock types

Implement the extension traits with the default glue `Builtin`; the new type
then composes with all others, including the standard ones
(`Arc<MyMutex<Box<T>>>`, `MyArc<Mutex<MyBox<T>>>`, …):

```rust
impl<T: ?Sized> OwningPointer for MyBox<T> {} // needs DerefMut
impl<T: ?Sized> SharedPointer for MyArc<T> {} // needs Deref

impl<T: ?Sized> RefCounted for MyArc<T> {     // exclusive access, MakeMut
    fn get_mut(this: &mut Self) -> Option<&mut T> {
        Arc::get_mut(&mut this.0)
    }

    fn new(target: T) -> Self
    where
        T: Sized,
    {
        Self(Arc::new(target))
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

    // `read_for`/`write_for` left out: `Duration` and `Instant` fail the build.
    // `read_until`/`write_until` default to `read_for`/`write_for`.

    fn inner_mut(&mut self) -> Result<&mut T, AccessError> {
        Ok(self.0.get_mut()?)
    }

    fn new(inner: T) -> Self
    where
        T: Sized,
    {
        Self(Mutex::new(inner))
    }
}
```

The `?Sized` in these impls is what allows the type to hold unsized values.

## Types from other crates

For types such as `triomphe::Arc` or `parking_lot::Mutex`, neither the trait
nor the type belongs to the user’s crate, and the orphan rule rejects the
impl (E0117). A local glue type as trait parameter makes the impl local:

```rust
use access::{AccessError, Lock, RefCounted, SharedPointer, TraitGlue};

struct MyGlue;

impl TraitGlue for MyGlue {}

impl<T: ?Sized> SharedPointer<MyGlue> for triomphe::Arc<T> {}

impl<T: ?Sized> RefCounted<MyGlue> for triomphe::Arc<T> {
    fn get_mut(this: &mut Self) -> Option<&mut T> {
        triomphe::Arc::get_mut(this)
    }

    fn new(target: T) -> Self
    where
        T: Sized,
    {
        triomphe::Arc::new(target)
    }
}

impl<T: ?Sized> Lock<MyGlue> for parking_lot::Mutex<T> {
    type Inner = T;

    fn read(&self) -> Result<impl Deref<Target = T>, AccessError> {
        Ok(self.lock())
    }

    fn write(&self) -> Result<impl DerefMut<Target = T>, AccessError> {
        Ok(self.lock())
    }

    fn read_for(&self, timeout: Duration) -> Result<impl Deref<Target = T>, AccessError> {
        self.try_lock_for(timeout).ok_or(AccessError::WouldBlock)
    }

    // … `write_for`, `try_*`, `*_until`, `inner_mut` and `new` alike
}
```

The rule behind it: an impl of a foreign trait is allowed if one of its
types is local and no uncovered type parameter comes before it. In
`Lock<MyGlue> for parking_lot::Mutex<T>`, `MyGlue` is local and `T` is
covered by `parking_lot::Mutex`.

The glue travels inside the path (`ViaLock<MyGlue, …>`), so the access
traits need no glue parameter of their own, and types under different glues
mix freely: `Arc<parking_lot::Mutex<Box<T>>>` works. Register every type
under one glue only; a second registration makes the path ambiguous.

## Unsized values

All impls accept unsized values, so sized and unsized values need no
separate treatment. `Arc<Box<str>>`, `Arc<str>`, `Arc<Box<[String]>>`,
`Mutex<Box<dyn Trait>>` and `Arc<Mutex<[u32]>>` work as they are:

```rust
fn area<P>(shape: &impl Access<dyn Shape, P>) -> f64 {
    shape.must_get_ref().area()
}

let boxed: Mutex<Box<dyn Shape>> = Mutex::new(Box::new(Square(2.0)));
area(&boxed);
```

Only when the container itself is unsized (`str`, `Mutex<[u32]>`) must the
generic function allow it explicitly, because type parameters and
`impl Trait` arguments are `Sized` by default:

```rust
fn text<S: Access<str, P> + ?Sized, P>(store: &S) -> String {
    store.must_get_ref().to_owned()
}

text("abc");
```

`[str]` is not a valid type: slice elements must be `Sized`. Use `Box<str>`
or `[String]` instead.

## Limitations

- Calls on concrete types without a target are ambiguous (E0282), since
  `Arc<Mutex<u32>>` can also be accessed as `Mutex<u32>` or as itself. Name
  the target explicitly: `Access::<u32, _>::must_get_ref(&arc)`. Generic code
  is not affected.
- Unsupported lock methods are reported by `cargo build`, not by
  `cargo check`, and without the call site; see above.
- Return-position `impl Trait` makes the traits not dyn-compatible; there is
  no `dyn Access<…>`.
- `Arc<RefCell<T>>` works but is neither `Send` nor `Sync`, so it behaves
  like `Rc<RefCell<T>>`.
- With `Block`, exclusive access to a clone of an `Arc<Mutex<T>>` while a
  guard of the same mutex is alive in the same thread deadlocks or panics,
  as with plain `lock()` calls. `TryLock` returns `WouldBlock` instead.

## Tests

```bash
cargo test
```

- `tests/acceptance.rs`: all standard combinations, references, deep
  nesting, `dyn` values, `SharedMut` across threads, a generic struct and a
  custom lock with the required methods only.
- `tests/custom_layers.rs`: self-made pointer and lock types alone, nested
  and mixed with standard ones, plus unsized values.
- `tests/errors.rs`: `IsShared`, `WouldBlock` and `Poisoned` in all their
  causes, and the panics of `must_*`.
- `tests/lock_methods.rs`: static dispatch of every lock method, `TryLock`
  on taken locks, `RefCell` that never waits.
- `tests/make_mut.rs`: detaching shared pointers, copying locks under their
  lock, no `Clone` needed without shared pointers.
- `tests/foreign_glue.rs`: types of the stand-in crate `tests/somebox`
  joined through a glue type, including a lock with timeouts.
- Doctests, among them `compile_fail` checks: `Duration` on a `Mutex`
  (E0080), `MakeMut` without `Clone` (E0277) and the orphan rule without
  glue (E0117).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
