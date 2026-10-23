// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! An interpreter for the Starlark configuration language, ported from
//! go.starlark.net.

pub mod compile;
#[path = "lib/mod.rs"]
pub mod lib;
pub mod repl;
pub mod resolve;
pub mod spell;
pub mod starlark;
pub mod starlarkstruct;
pub mod syntax;
pub mod unicode;
