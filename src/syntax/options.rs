// Copyright 2023 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Per-file dialect options (ported from go.starlark.net/syntax/options.go).

/// FileOptions specifies various per-file options that affect static
/// aspects of an individual file such as parsing, name resolution, and
/// code generation. (Options that affect global dynamics are typically
/// controlled through the runtime's Thread.)
///
/// The default value of FileOptions is the default behavior.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileOptions {
    // resolver
    /// Allow references to the 'set' built-in function.
    pub set: bool,
    /// Allow 'while' statements.
    pub while_: bool,
    /// Allow if/for/while statements at top-level.
    pub top_level_control: bool,
    /// Allow reassignment to top-level names.
    pub global_reassign: bool,
    /// Load creates global not file-local bindings (deprecated).
    pub load_binds_globally: bool,

    // compiler
    /// Disable recursion check for functions in this file.
    pub recursion: bool,
}

impl FileOptions {
    /// Returns the options corresponding to the values of the original
    /// implementation's legacy global flags (Go's `LegacyFileOptions`):
    /// `allow_set` maps to `set`; `allow_global_reassign` enables `while`
    /// loops, top-level control flow and global reassignment;
    /// `allow_recursion` maps to `recursion`.
    pub fn legacy(allow_set: bool, allow_global_reassign: bool, allow_recursion: bool) -> Self {
        FileOptions {
            set: allow_set,
            while_: allow_global_reassign,
            top_level_control: allow_global_reassign,
            global_reassign: allow_global_reassign,
            load_binds_globally: false,
            recursion: allow_recursion,
        }
    }
}
