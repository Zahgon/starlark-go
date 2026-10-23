# Starlark in Rust: Implementation

This document describes some of the design choices of the Rust
implementation of Starlark, a port of the Go implementation
(go.starlark.net). The crate is organized like the Go original:

| Module | Contents |
| --- | --- |
| `syntax` | scanner, parser, syntax trees and `FileOptions` |
| `resolve` | name resolution and static checks |
| `compile` | bytecode compiler and compiled program representation |
| `starlark` | values, the bytecode interpreter, and the built-in functions |
| `starlarkstruct` | the non-standard `struct` and `module` types |
| `lib::json`, `lib::math`, `lib::time` | library modules used by the command-line tool |
| `repl` | the read-eval-print loop and a simple `load` implementation |

  * [Scanner](#scanner)
  * [Parser](#parser)
  * [Resolver](#resolver)
  * [Evaluator](#evaluator)
    * [Data types](#data-types)
    * [Freezing](#freezing)
    * [Fail-fast iterators](#fail-fast-iterators)
    * [Evaluation strategy](#evaluation-strategy)
    * [Threads, errors, print and load](#threads-errors-print-and-load)
  * [Testing](#testing)


## Scanner

The scanner is derived from Russ Cox's
[buildifier](https://github.com/bazelbuild/buildtools/tree/master/buildifier)
tool, which pretty-prints Bazel BUILD files.

Most of the work happens in `Scanner::next_token` in `syntax/scan.rs`.

## Parser

The parser is hand-written recursive-descent parser. It uses the
technique of [precedence
climbing](http://www.engr.mun.ca/~theo/Misc/exp_parsing.htm#climbing)
to reduce the number of productions (see `parse_test_prec` and
`precedence` in `syntax/parse.rs`).

In some places the parser accepts a larger set of programs than are
strictly valid, leaving the task of rejecting them to the subsequent
resolver pass. For example, in the function call `f(a, b=c)` the
parser accepts any expression for `a` and `b`, even though `b` may
legally be only an identifier. For the parser to distinguish these
cases would require additional lookahead.

## Resolver

The resolver reports structural errors in the program, such as the use
of `break` and `continue` outside of a loop.

Starlark has stricter syntactic limitations than Python. For example,
it does not permit `for` loops or `if` statements at top level, nor
does it permit global variables to be bound more than once.
These limitations come from the Bazel project's desire to make it easy
to identify the sole statement that defines each global, permitting
accurate cross-reference documentation.

In addition, the resolver validates all variable names, classifying
them as references to universal, predeclared, global, local, cell, or
free variables (`resolve::Scope`). Local and free variables are mapped
to a small integer, allowing the evaluator to use an efficient (flat)
representation for the environment.

Not all features of this implementation are "standard" (that is,
supported by Bazel's Java implementation), so non-standard features
such as `set`, `while` loops, top-level control flow, global
reassignment and recursion are controlled by the fields of
`syntax::FileOptions`. The resolver reports any uses of dialect
features that have not been enabled.


## Evaluator

### Data types

Starlark values are represented by the `starlark::Value` enum. Common
types (`None`, `bool`, `int`, `float`, `string`, `bytes`, `list`,
`tuple`, `dict`, `set`, functions and built-ins) are variants of the
enum; all other types, including application-defined ones, are held in
`Value::Other` as an `Rc<dyn StarlarkValue>`. The required methods of the
`StarlarkValue` trait describe every value (`type_name`, `repr`, `hash`);
the methods with default implementations correspond to optional
capabilities such as attributes, indexing, iteration and operators.
Values are reference-counted, so cloning a `Value` is cheap.

<b>Integers:</b> Integers have arbitrary precision. This was chosen
because, for many applications, Starlark must be able to handle without
loss protocol buffer values containing signed and unsigned 64-bit
integers, which requires 65 bits of precision.
The `Int` type holds values in the signed 32-bit range inline
(`Int::Small`) and all others as a reference-counted `num_bigint::BigInt`
(`Int::Big`). Using a 32-bit range for "small" numbers makes it cheap to
detect overflow, since the sum or product of two such numbers always
fits in an `i64`; only results outside the range allocate.

<b>Floating point</b>:
Floating point numbers are represented using Rust's `f64`.
Again, `float` support is required to support protocol buffers. The
existence of floating-point NaN and its infamous comparison behavior
(`NaN != NaN`) had many ramifications for the API, since we cannot
assume the result of an ordered comparison is either less than,
greater than, or equal: it may also fail.

<b>Strings</b>:
A Starlark string is an immutable sequence of bytes (`Rc<[u8]>`),
conventionally UTF-8 but not required to be valid, so strings
read from files or produced by slicing at arbitrary byte offsets are
represented without loss. Indexing and `len` operate on bytes. The
`elems`, `elem_ords`, `codepoints` and `codepoint_ords` methods iterate
over a string's bytes or Unicode code points; when decoding, invalid
bytes are treated as U+FFFD. The separate `bytes` type holds binary data.

<b>Dictionaries and sets</b>:
Starlark dictionaries have predictable iteration order.
Furthermore, the hash of a Starlark value is defined by Starlark
(`Value::hash`), not by Rust: big integers that equal small ones, for
example, must hash alike, and some values are unhashable.
Consequently, we do not use Rust's `HashMap` keyed by `Value`.

`Dict` and `Set` are built on `HashTable`, which stores entries in a
slab, indexes them by their 32-bit Starlark hash, and links them into a
doubly-linked list that maintains the insertion order of the elements
for iteration.

<b>Struct:</b>
The `starlarkstruct` module provides a non-standard Starlark
extension data type, `struct`, that maps field identifiers to
arbitrary values. Fields are accessed using dot notation: `y = s.f`.
This data type is extensively used in Bazel, but its specification is
currently evolving.

Starlark has no `class` mechanism, nor equivalent of Python's
`namedtuple`, though it is likely that future versions will support
some way to define a record data type of several fields, with a
representation more efficient than a hash table.


### Freezing

All mutable values created during module initialization are _frozen_
upon its completion. It is this property that permits a module to be
loaded once and its values shared by every module that loads it,
without any of them being able to observe another's mutations.

The implementation supports freezing by storing an additional
"frozen" flag in each mutable object. Once this flag is set,
all subsequent attempts at mutation fail. `Value::freeze` sets the
frozen flag of a value if not already set, and freezes each value that
it contains; `StarlarkValue::freeze` does the same for other types.
For example, when a list is frozen, it freezes each of its elements;
when a dictionary is frozen, it freezes each of its keys and values;
and when a function value is frozen, it freezes each of the free
variables and parameter default values implicitly referenced by its closure.
Application-defined types must also follow this discipline.
`exec_file` freezes the module's globals (`StringDict::freeze`) before
returning them.

The freeze mechanism is finer grained than in the Java implementation:
in effect, the latter has one "frozen" flag per module, and every value
holds a reference to the frozen flag of its module. This makes setting
the frozen flag more efficient---a simple bit flip, no need to traverse
the object graph---but coarser grained. Also, it complicates the API
slightly because to construct a list, say, requires a reference to the
frozen flag it should use.

The per-value flag also permits the freeze operation to be exposed to
the program, for example as a built-in function. The test suite does
this to test the freeze mechanism itself.


### Fail-fast iterators

In some languages, a program may mutate a data structure while
iterating over it; for example, a Go range loop over a map may delete
map elements. In other languages (such as Java), iterators do extra
bookkeeping so that modification of the underlying collection
invalidates the iterator, and the next attempt to use it fails.
This often helps to detect subtle mistakes.

Starlark takes this a step further. Instead of mutation of the
collection invalidating the iterator, the act of iterating makes the
collection temporarily immutable, so that an attempt to, say, delete a
dict element while looping over the dict, will fail. The error is
reported against the delete operation, not the iteration.

This is implemented by having each mutable iterable value record a
counter of active iterators. Starting a loop increments this counter,
and completing a loop decrements it. A collection with a nonzero
counter behaves as if frozen. If the collection is actually frozen,
the counter bookkeeping is unnecessary.

Iterators implement the `ValueIterator` trait and are returned by
`iterate` as a `Box<dyn ValueIterator>`. The counter is decremented when
the iterator is dropped, so Rust's ownership rules ensure that every
iterator is released, even when a loop exits early or fails.

### Evaluation strategy

The `compile` module translates each resolved file into a `Program`:
a table of constants and names, plus a `Funcode` for the module's
top-level code and for each function. A `Funcode` is a vector of
instructions (`Insn`), each an `Opcode` with an optional `u32` operand
and the source position current when it was emitted, for a stack
machine with an operand stack, a stack of active iterators, and arrays
of locals, cells, free variables and globals. The compiler computes the
maximum operand stack depth of each function, so the interpreter
allocates its stack once per call.

The interpreter (`starlark/interp.rs`) executes a function's code with
a loop around a `match` on the opcode. Rust typically compiles such a
`match` to a jump table, and the operand stack holds intermediate results
without garbage-collection overhead.

Built-in functions are `Builtin` values wrapping a Rust closure that
receives the thread, the positional arguments and the keyword
arguments. `unpack_args` and `unpack_positional_args` convert those
arguments into Rust variables, reporting errors in a uniform style.

### Threads, errors, print and load

All evaluation takes place in a `starlark::Thread`, which holds the
call stack, the step counter used by `Thread::set_max_execution_steps`
and `Thread::cancel`, thread-local values (`Thread::set_local`), and the
client's implementations of `print` (`Thread::print`) and `load`
(`Thread::load`). If `print` is not set, messages are written to
standard error.

Values use `Rc` and interior mutability (`Cell`, `RefCell`), so they
are not `Send`: a `Thread` and the values it creates belong to a
single operating-system thread. Independent threads, each with its own
values, may run in parallel on different operating-system threads.

When a call fails, the error is wrapped in an `Error::Eval` holding an
`EvalError`, which records the `CallStack` at the point of failure;
`EvalError::backtrace` formats it like a Python traceback. Each
`CallFrame` has a function name and the source `Position` of the
current instruction.

Starlark recursion is disabled by default and detected at call time.
When `FileOptions::recursion` is set, the interpreter instead limits
the depth of the call stack; because each Starlark call uses native
stack space, the command-line tool runs the interpreter on a thread
with a large stack.

## Testing

The integration tests in `tests/` are run with `cargo test`:

* `tests/parse_test.rs` checks the scanner and parser, including the
  expected errors in `syntax/testdata`.
* `tests/resolve_test.rs` checks the resolver against `resolve/testdata`.
* `tests/eval_test.rs` runs the Starlark programs in `starlark/testdata`,
  with a few extra built-ins and value types defined in Rust to
  exercise the embedding API.

Test files are split into chunks by `---` lines, and a `###` comment
gives a regular expression that the error reported on that line must
match (see `tests/common/mod.rs`). Starlark test programs use the
`assert` module defined in `starlarktest/assert.star`, whose
`assert.eq`, `assert.fails` and related functions are built on a few
Rust built-ins provided by the test harness.
