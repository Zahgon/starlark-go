<!-- This file is the project homepage for the Rust port of go.starlark.net -->

# Starlark in Rust

[![Tests](https://github.com/google/starlark-go/actions/workflows/tests.yml/badge.svg)](https://github.com/google/starlark-go/actions/workflows/tests.yml)

This is the home of the _Starlark in Rust_ project, a port of
[Starlark in Go](https://github.com/google/starlark-go) to Rust.
It provides an interpreter for Starlark as a library crate, `starlark`,
and a command-line tool of the same name.
Starlark was formerly known as Skylark.

Starlark is a dialect of Python intended for use as a configuration language.
Like Python, it is an untyped dynamic language with high-level data
types, first-class functions with lexical scope, and garbage collection.
Unlike CPython, Starlark has no global interpreter lock, so independent
Starlark threads can execute in parallel and Starlark workloads scale
well on parallel machines.
Starlark is a small and simple language with a familiar and highly
readable syntax. You can use it as an expressive notation for
structured data, defining functions to eliminate repetition, or you
can use it to add scripting capabilities to an existing application.

A Starlark interpreter is typically embedded within a larger
application, and the application may define additional domain-specific
functions and data types beyond those provided by the core language.
For example, Starlark was originally developed for the
[Bazel build tool](https://bazel.build).
Bazel uses Starlark as the notation both for its BUILD files (like
Makefiles, these declare the executables, libraries, and tests in a
directory) and for [its macro
language](https://docs.bazel.build/versions/master/skylark/language.html),
through which Bazel is extended with custom logic to support new
languages and compilers.


## Documentation

* Language definition: [doc/spec.md](doc/spec.md)

* About the Rust implementation: [doc/impl.md](doc/impl.md)

* API documentation: run `cargo doc --open`

* Mailing list: [starlark-go](https://groups.google.com/forum/#!forum/starlark-go)

* Issue tracker: [https://github.com/google/starlark-go/issues](https://github.com/google/starlark-go/issues)

### Getting started

Build the interpreter (this requires a recent stable Rust toolchain):

```shell
$ git clone https://github.com/google/starlark-go
$ cd starlark-go
$ cargo build --release
```

The executable is `./target/release/starlark`.
Use `cargo install --path .` to install it in `~/.cargo/bin`.

Run the interpreter:

```console
$ cat coins.star
coins = {
  'dime': 10,
  'nickel': 5,
  'penny': 1,
  'quarter': 25,
}
print('By name:\t' + ', '.join(sorted(coins.keys())))
print('By value:\t' + ', '.join(sorted(coins.keys(), key=coins.get)))

$ ./target/release/starlark coins.star
By name:	dime, nickel, penny, quarter
By value:	penny, nickel, dime, quarter
```

Interact with the read-eval-print loop (REPL):

```pycon
$ ./target/release/starlark
>>> def fibonacci(n):
...    res = list(range(n))
...    for i in res[2:]:
...        res[i] = res[i-2] + res[i-1]
...    return res
...
>>> fibonacci(10)
[0, 1, 1, 2, 3, 5, 8, 13, 21, 34]
>>>
```

When you have finished, type `Ctrl-D` to close the REPL's input stream.

The command accepts these flags:

- `-c prog`: execute the program `prog` instead of a file.
- `-showenv`: on success, print the final global environment.
- `-recursion`: allow `while` statements and recursive functions.
- `-globalreassign`: allow reassignment of globals, and `if`/`for`/`while`
  statements at top level.
- `-set`: allow the `set` built-in (enabled by default; use `-set=false`
  to disable it).

The `json`, `math` and `time` modules are predeclared in the command-line
tool, and a `load` statement executes the named file (relative to the
current directory) and caches its globals.

Run the tests:

```shell
$ cargo test
```

Embed the interpreter in your Rust program by adding the crate as a
dependency (for example, `starlark = { path = "../starlark-go" }` in
`Cargo.toml`):

```rust
use starlark::starlark::{call, exec_file, Error, StringDict, Thread, Value};

fn main() -> Result<(), Error> {
    // Execute a Starlark program in a file.
    let mut thread = Thread::new("my thread");
    let globals = exec_file(&mut thread, "fibonacci.star", None, &StringDict::new())?;

    // Retrieve a module global.
    let fibonacci = globals.get("fibonacci").expect("fibonacci is not defined");

    // Call the Starlark function from Rust.
    let v = call(&mut thread, fibonacci, &[Value::int(10)], &[])?;
    println!("fibonacci(10) = {}", v); // fibonacci(10) = [0, 1, 1, 2, 3, 5, 8, 13, 21, 34]
    Ok(())
}
```

`exec_file` reads the file itself when its `src` argument is `None`, and
uses the same dialect options as the command-line tool; use
`exec_file_options` with a `syntax::FileOptions` to choose them per file.
The last argument holds predeclared names, such as built-in functions
created with `Builtin::value`. Set `thread.print` and `thread.load` to
control the behavior of `print` and `load`. When evaluation fails, the
error is an `Error::Eval`; its `EvalError::backtrace` method describes
the active Starlark calls. See [src/bin/starlark.rs](src/bin/starlark.rs) and
[tests/eval_test.rs](tests/eval_test.rs) for more examples.

### Bounding time, memory, and other resources

It is trivial for a Starlark program to consume all available time and
memory. Even without recursion, a program can get the interpreter stuck
in an endless loop, for example by applying a recursive built-in operator
to a cyclic value such as a list that contains itself.
So if you evaluate Starlark code from an untrusted source in the same
address space as your application, you are trusting the health of your
application to that source.

The interpreter provides two mechanisms for bounding computation:

- `Thread::set_max_execution_steps` limits the number of abstract
  computation steps a thread may take. The measure is deterministic and
  reproducible, but it does not correspond to CPU time: not all steps are
  equal, and a single step may be a call to a built-in function or
  operator that is very slow or allocates a lot of memory (e.g.
  `string * int`). When the limit is reached, evaluation fails with a
  "too many steps" error.

- `Thread::cancel` makes the next step of evaluation fail with an error
  that includes the given reason. Built-in functions receive the
  `&mut Thread` and may call it, for example after checking a deadline or
  a cancellation flag stored with `Thread::set_local`.
  `Thread::uncancel` clears the cancellation state.

There is no way to bound the memory used by a Starlark thread. The
interpreter rejects single allocations that are unreasonably large, but a
thread can allocate an unbounded amount of memory in smaller steps.
Similarly, the interpreter limits the depth of Starlark calls when
recursion is enabled, but deep recursion needs a large native stack:
the command-line tool runs the interpreter on a thread with a large
stack, and embedding applications should do the same.

If you need to defend against denial of service, the only reliable
approach is to let the operating system help you: evaluate Starlark in a
separate process with a tight limit on memory (e.g. using ulimit,
setrlimit, or cgroups), terminate it if it takes too much wall or CPU
time, and handle OOM failures, timeouts, and crashes in the parent. The
interpreter starts very quickly, so the overhead is usually modest.

For past discussions of this topic, see
[these issues](https://github.com/google/starlark-go/issues?q=is:issue+(160+OR+236+OR+252+OR+410+OR+470+OR+498+OR+606+OR+617+OR+621+OR+661)+NOT+PEP+NOT+"byte+code").

### Contributing

We welcome submissions but please let us know what you're working on
if you want to change or add to the Starlark repository.

Before undertaking to write something new for the Starlark project,
please file an issue or claim an existing issue.
All significant changes to the language or to the interpreter's Rust
API must be discussed before they can be accepted.
This gives all participants a chance to validate the design and to
avoid duplication of effort.

Despite some differences, this implementation of Starlark strives to
match the behavior of [the Java implementation](https://github.com/bazelbuild/bazel)
used by Bazel and maintained by the Bazel team.
For that reason, proposals to change the language itself should
generally be directed to [the Starlark site](
https://github.com/bazelbuild/starlark/), not to the maintainers of this
project.
Only once there is consensus that a language change is desirable may
its implementation proceed.

We use GitHub pull requests for contributions.

Please complete Google's contributor license agreement (CLA) before
sending your first change to the project.  If you are the copyright
holder, you will need to agree to the
[individual contributor license agreement](https://cla.developers.google.com/about/google-individual),
which can be completed online.
If your organization is the copyright holder, the organization will
need to agree to the [corporate contributor license agreement](https://cla.developers.google.com/about/google-corporate).
If the copyright holder for your contribution has already completed
the agreement in connection with another Google open source project,
it does not need to be completed again.

### Stability

We reserve the right to make breaking language and API changes at this
stage in the project, although we will endeavor to keep them to a minimum.
Once the Bazel team has finalized the version 1 language specification,
we will be more rigorous with interface stability.

The crate requires the Rust version given by `rust-version` in
[Cargo.toml](Cargo.toml) or later, and is tested with the current
stable toolchain.

### Credits

Starlark was designed and implemented in Java by
Jon Brandvein,
Alan Donovan,
Laurent Le Brun,
Dmitry Lomov,
Vladimir Moskva,
François-René Rideau,
Gergely Svigruha, and
Florian Weikert,
standing on the shoulders of the Python community.
The Go implementation was written by Alan Donovan and Jay Conrod;
its scanner was derived from one written by Russ Cox.
This crate is a port of the Go implementation to Rust.

### Legal

Starlark in Go is Copyright (c) 2018 The Bazel Authors.
All rights reserved.

It is provided under a 3-clause BSD license:
[LICENSE](https://github.com/google/starlark-go/blob/master/LICENSE).
This Rust port is distributed under the same license. The modules under
`src/lib/gomath`, `src/lib/gotime` and `src/lib/json` port parts of the Go
standard library and keep its notices; they are governed by Go's BSD-style
license (https://go.dev/LICENSE).

Starlark in Rust is not an official Google product.
