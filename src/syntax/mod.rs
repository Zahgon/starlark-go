// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Package syntax provides a Starlark parser and abstract syntax tree
//! (ported from go.starlark.net/syntax).

mod ast;
mod options;
mod parse;
mod position;
mod quote;
mod scan;
mod token;
pub mod utf8;
mod walk;

pub use ast::*;
pub use options::FileOptions;
pub use parse::{parse, parse_compound_stmt, parse_expr, parse_file_portion};
pub use position::{rune_count, Position};
pub use quote::quote;
pub use scan::{Error, FilePortion};
pub use token::Token;
pub use walk::{walk, Node};
