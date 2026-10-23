// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Package starlark provides a Starlark interpreter (ported from
//! go.starlark.net/starlark).
//!
//! Starlark values are represented by [`Value`]. Client applications may
//! define new data types by implementing [`StarlarkValue`], and
//! domain-specific functions in Rust as [`Builtin`] values whose
//! implementation may use [`unpack_args`] to make sense of the positional
//! and keyword arguments provided by the caller.
//!
//! To parse and evaluate a Starlark source file, use [`exec_file`]. The
//! [`eval`] function evaluates a single expression. All evaluator
//! functions require a [`Thread`] parameter which defines the
//! "thread-local storage" of a Starlark thread. When evaluation fails it
//! returns an [`Error::Eval`] from which the application may obtain a
//! backtrace of active Starlark calls.

mod eval;
pub mod fmt;
mod hashtable;
mod int;
mod interp;
mod library;
mod unpack;
mod value;

pub use eval::{
    attr_names, binary, call, eval, eval_expr_options, eval_options, exec_file, exec_file_options,
    exec_repl_chunk, file_program, find_bytes, get_attr, get_index, go_path_error, indices,
    interpolate, legacy_file_options, read_source_file, rfind_bytes, set_field, set_index,
    set_legacy_file_options, source_program_options, unary, CallFrame, CallStack, DebugFrame,
    EvalError, LoadFn, PrintFn, Program, StringDict, Thread,
};
pub use hashtable::{hash_string, soft_hash_string, HashTable};
pub use int::{
    as_int32, as_signed, as_unsigned, bigint_to_f64, finite_float_to_int, number_to_int, Int,
};
pub use library::{
    count_bytes, fields, for_each_rune, name_err, replace_bytes, split_bytes, split_n, to_lower,
    to_upper, trim_left_func, trim_right_func, universe, universe_get, universe_has,
    universe_insert, utf8_transcode, write_str_value,
};
pub use unpack::{
    unpack_arg, unpack_args, unpack_positional_args, BytesArg, Callable, FromValue, Iterable,
    Unpacker,
};
pub use value::{
    collect, compare, compare_depth, empty_tuple, equal, equal_depth, float_cmp, float_mod,
    float_to_string, for_each, format_float, iterate, len, threeway, to_string, write_value,
    Builtin, BuiltinFn, Dict, Error, Function, List, Module, Set, Side, StarlarkValue, Value,
    ValueIterator, COMPARE_LIMIT,
};
