// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Library modules for the command-line tool (ported from
//! go.starlark.net/lib): json, math and time.

pub mod json;
pub mod math;
pub mod time;

use crate::starlark::Value;

/// Returns the library modules, as (name, module value) pairs, that the
/// command-line tool adds to the universe.
pub fn modules() -> Vec<(&'static str, Value)> {
    vec![
        ("json", json::module()),
        ("math", math::module()),
        ("time", time::module()),
    ]
}
