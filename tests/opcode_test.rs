// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Tests of the opcode names (ported from internal/compile/opcode_test.go).

use starlark::compile::Opcode;

/// Every opcode, in declaration (numeric) order.
const ALL: [Opcode; 68] = [
    Opcode::Nop,
    Opcode::Dup,
    Opcode::Dup2,
    Opcode::Pop,
    Opcode::Exch,
    Opcode::Lt,
    Opcode::Gt,
    Opcode::Ge,
    Opcode::Le,
    Opcode::Eql,
    Opcode::Neq,
    Opcode::Plus,
    Opcode::Minus,
    Opcode::Star,
    Opcode::Slash,
    Opcode::Slashslash,
    Opcode::Percent,
    Opcode::Amp,
    Opcode::Pipe,
    Opcode::Circumflex,
    Opcode::Ltlt,
    Opcode::Gtgt,
    Opcode::In,
    Opcode::Uplus,
    Opcode::Uminus,
    Opcode::Tilde,
    Opcode::None,
    Opcode::True,
    Opcode::False,
    Opcode::Mandatory,
    Opcode::Iterpush,
    Opcode::Iterpop,
    Opcode::Not,
    Opcode::Return,
    Opcode::Setindex,
    Opcode::Index,
    Opcode::Setdict,
    Opcode::Setdictuniq,
    Opcode::Append,
    Opcode::Slice,
    Opcode::InplaceAdd,
    Opcode::InplacePipe,
    Opcode::Makedict,
    Opcode::Jmp,
    Opcode::Cjmp,
    Opcode::Iterjmp,
    Opcode::Constant,
    Opcode::Maketuple,
    Opcode::Makelist,
    Opcode::Makefunc,
    Opcode::Load,
    Opcode::Setlocal,
    Opcode::Setglobal,
    Opcode::Local,
    Opcode::Free,
    Opcode::Freecell,
    Opcode::Localcell,
    Opcode::Setlocalcell,
    Opcode::Global,
    Opcode::Predeclared,
    Opcode::Universal,
    Opcode::Attr,
    Opcode::Setfield,
    Opcode::Unpack,
    Opcode::Call,
    Opcode::CallVar,
    Opcode::CallKw,
    Opcode::CallVarKw,
];

/// Checks that every opcode, including the highest-numbered one, has a
/// name. (The Go test also checks that an out-of-range opcode is reported
/// as illegal; a Rust Opcode cannot hold an out-of-range value.)
#[test]
fn test_opcode_names() {
    for (i, op) in ALL.iter().enumerate() {
        assert_eq!(*op as usize, i, "opcode {:?} is out of order", op);
        let name = op.name();
        assert!(
            !name.is_empty() && !name.starts_with("illegal op"),
            "opcode {} has no name",
            i
        );
    }
    assert_eq!(
        ALL.len(),
        Opcode::MAX as usize + 1,
        "the list of opcodes is incomplete"
    );
}
