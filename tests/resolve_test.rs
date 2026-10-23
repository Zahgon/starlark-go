// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Resolver tests (ported from go.starlark.net/resolve/resolve_test.go).

mod common;

use starlark::resolve;
use starlark::syntax::{parse, FileOptions};

/// A test may enable non-standard options by containing (e.g.) "option:recursion".
fn get_options(src: &str) -> FileOptions {
    FileOptions {
        set: option(src, "set"),
        while_: option(src, "while"),
        top_level_control: option(src, "toplevelcontrol"),
        global_reassign: option(src, "globalreassign"),
        load_binds_globally: option(src, "loadbindsglobally"),
        recursion: option(src, "recursion"),
    }
}

fn option(chunk: &str, name: &str) -> bool {
    chunk.contains(&format!("option:{name}"))
}

fn is_predeclared(name: &str) -> bool {
    name == "M"
}

fn is_universal(name: &str) -> bool {
    name == "U" || name == "float"
}

#[test]
fn test_resolve() {
    let filename = concat!(env!("CARGO_MANIFEST_DIR"), "/resolve/testdata/resolve.star");
    let mut report = common::Reporter::default();
    for mut chunk in common::read(filename, &mut report) {
        // A chunk may set options by containing e.g. "option:recursion".
        let opts = get_options(&chunk.source);

        let mut f = match parse(&opts, filename, chunk.source.as_bytes()) {
            Ok(f) => f,
            Err(e) => {
                report.errorf(e.to_string());
                continue;
            }
        };

        if let Err(errs) = resolve::file(&mut f, &is_predeclared, &is_universal) {
            for err in errs.0 {
                chunk.got_error(&mut report, err.pos.line as usize, &err.msg);
            }
        }
        chunk.done(&mut report);
    }
    report.check();
}
