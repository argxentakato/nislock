# AGENTS.md

## Rust idioms (mandatory)

Apply these to every Rust change in this repository. Deviate only with a concrete reason, stated in one line in the code or the final answer.
Do not rewrite unrelated existing code just to satisfy these rules.

### Arguments and strings

- Take borrowed/deref-target types in parameters: `&str` over `&String`, `&[T]` over `&Vec<T>`, `&T` over `&Box<T>`.
- Build strings with `format!` when readability matters. In hot paths, use `String::with_capacity` plus `push_str`/`push`.

### Construction

- Provide `new()` as an associated function; there are no language-level constructors.
- If a sensible default exists, implement `Default` (prefer `#[derive(Default)]`). A no-argument `new()` must delegate to `Default`, not duplicate it.
- Use `..Default::default()` for partial initialization. Use the builder pattern only when there are many optional settings.

### Ownership and mutation

- Use `std::mem::take` / `std::mem::replace` to move a value out of `&mut` (including enum state transitions) instead of `clone()`.
- Prefer temporary mutability: finish mutation in a block or shadow with `let x = x;` so the binding is immutable afterwards.
- Variables captured by a closure: rebind/clone/move in a dedicated scope before the closure (`let c = {let a = a.clone(); move || ...}`) rather than cloning inside it.
- A fallible function that consumes its argument should return it in the error type if the caller may want to retry.

### Types and traits

- Collections own data and `Deref` to a borrowed view (`Vec<T>` to `[T]`).
- Mark public enums/structs that may grow with `#[non_exhaustive]` (or a private field) to keep them extensible.
- Prefer `Option`/`Result` iteration and combinators (`iter()`, `extend(opt)`, `flatten`, `chain`) over manual `if let Some` loops when it is clearer.
- Use dynamic dispatch on the stack (`let mut x: &mut dyn Trait = ...`, with each backing value declared beforehand) instead of `Box<dyn Trait>` or duplicated branches when no heap allocation is needed.

### Cleanup

- Use RAII: put finalization in `Drop` of a guard value, not in manual cleanup at every exit path. Do not panic in `Drop`.

### Errors, lints, docs

- Keep rustdoc examples short. Hide setup noise with `# ` lines or a hidden helper fn.

### FFI / `unsafe` (PAM and similar)

- Keep `unsafe` in small modules and expose a safe API.
- Convert errors from C return codes / sentinel values into `Result` at the boundary; never leak raw codes.
- Accept C strings with `CStr` and minimal `unsafe`; pass strings out with `CString` that outlives the call. Do not hold a `CString` temporary's pointer past the statement.
- Every `unsafe` block gets a `// SAFETY:` comment.

## Rust anti-patterns (mandatory)

Apply these to every Rust change; the same exception policy as the idioms above applies.

- **Cloning to satisfy the borrow checker:** Do not clone just to silence a borrow error. First narrow or reorder borrows, adjust ownership, or use `mem::take` / `mem::replace` where appropriate. A value clone creates independent data, so mutations are not shared; use it deliberately when an independent copy is needed. Cloning `Rc` or `Arc` shares ownership rather than cloning the inner value.
- **Denying all warnings in source:** Do not add `#![deny(warnings)]`; new compiler or Clippy warnings can otherwise break builds across toolchain updates. Enforce `-D warnings` in CI or commands instead; deny specific lints in source only when justified.
- **Deref polymorphism:** Do not implement `Deref` merely to reuse another type's methods or imitate inheritance. It introduces implicit method lookup without subtyping or forwarding trait implementations. Prefer composition with explicit delegation or traits; `Deref` remains appropriate for pointer-like types and borrowed collection views.

## Workflow

- Use `nix develop -c cargo ...` (no system pkg-config/clang).
- Before finishing, run `nix develop -c cargo clippy --workspace --all-targets -- -D warnings` and `nix develop -c cargo test --workspace`; fix findings instead of `#[allow]`.
- Add a dependency to `[workspace.dependencies]` only if two or more members use it; otherwise put it in that crate's manifest.
- Code comments: only what the code cannot show, one short line. No narration of edits.
