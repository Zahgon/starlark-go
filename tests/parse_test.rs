// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Parser tests (ported from go.starlark.net/syntax/parse_test.go).

mod common;

use starlark::syntax::*;

/// Builds the parenthesized tree notation of the Go test: structs are
/// printed as (type name=value ...), showing only non-empty fields.
struct Tree(String);

impl Tree {
    fn new(name: &str) -> Tree {
        Tree(format!("({name}"))
    }

    fn token(mut self, name: &str, tok: Token) -> Tree {
        self.0.push_str(&format!(" {name}={}", tok.name()));
        self
    }

    fn flag(mut self, name: &str, b: bool) -> Tree {
        if b {
            self.0.push_str(&format!(" {name}"));
        }
        self
    }

    fn field(mut self, name: &str, value: Option<String>) -> Tree {
        if let Some(v) = value {
            self.0.push_str(&format!(" {name}={v}"));
        }
        self
    }

    fn expr(self, name: &str, e: &Expr) -> Tree {
        self.field(name, Some(expr_tree(e)))
    }

    fn opt(self, name: &str, e: &Option<Box<Expr>>) -> Tree {
        self.field(name, e.as_deref().map(expr_tree))
    }

    fn list(mut self, name: &str, items: Vec<String>) -> Tree {
        if !items.is_empty() {
            self.0.push_str(&format!(" {name}=({})", items.join(" ")));
        }
        self
    }

    fn exprs(self, name: &str, items: &[Expr]) -> Tree {
        self.list(name, items.iter().map(expr_tree).collect())
    }

    fn stmts(self, name: &str, items: &[Stmt]) -> Tree {
        self.list(name, items.iter().map(stmt_tree).collect())
    }

    fn end(mut self) -> String {
        self.0.push(')');
        self.0
    }
}

/// Like Go's %q for the ASCII strings used in these tests.
fn go_quote(s: &[u8]) -> String {
    let mut out = String::from("\"");
    for &b in s {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out.push('"');
    out
}

fn literal_tree(lit: &Literal) -> String {
    match (&lit.value, lit.token) {
        (LiteralValue::String(s), Token::String) => go_quote(s),
        (LiteralValue::String(s), Token::Bytes) => format!("b{}", go_quote(s)),
        (LiteralValue::Int(i), _) => i.to_string(),
        (LiteralValue::BigInt(b), _) => b.to_string(),
        _ => String::new(),
    }
}

fn clause_tree(c: &Clause) -> String {
    match c {
        Clause::For(c) => Tree::new("ForClause")
            .expr("Vars", &c.vars)
            .expr("X", &c.x)
            .end(),
        Clause::If(c) => Tree::new("IfClause").expr("Cond", &c.cond).end(),
    }
}

fn dict_entry_tree(e: &DictEntry) -> String {
    Tree::new("DictEntry")
        .expr("Key", &e.key)
        .expr("Value", &e.value)
        .end()
}

fn expr_tree(e: &Expr) -> String {
    match e {
        Expr::Ident(id) => id.name.clone(),
        Expr::Literal(lit) => literal_tree(lit),
        Expr::Binary(e) => Tree::new("BinaryExpr")
            .expr("X", &e.x)
            .token("Op", e.op)
            .expr("Y", &e.y)
            .end(),
        Expr::Call(e) => Tree::new("CallExpr")
            .expr("Fn", &e.func)
            .exprs("Args", &e.args)
            .end(),
        Expr::Comprehension(e) => Tree::new("Comprehension")
            .flag("Curly", e.curly)
            .expr("Body", &e.body)
            .list("Clauses", e.clauses.iter().map(clause_tree).collect())
            .end(),
        Expr::Cond(e) => Tree::new("CondExpr")
            .expr("Cond", &e.cond)
            .expr("True", &e.true_)
            .expr("False", &e.false_)
            .end(),
        Expr::DictEntry(e) => dict_entry_tree(e),
        Expr::Dict(e) => Tree::new("DictExpr")
            .list("List", e.list.iter().map(dict_entry_tree).collect())
            .end(),
        Expr::Dot(e) => Tree::new("DotExpr")
            .expr("X", &e.x)
            .field("Name", Some(e.name.name.clone()))
            .end(),
        Expr::Index(e) => Tree::new("IndexExpr").expr("X", &e.x).expr("Y", &e.y).end(),
        Expr::Lambda(e) => Tree::new("LambdaExpr")
            .exprs("Params", &e.params)
            .expr("Body", &e.body)
            .end(),
        Expr::List(e) => Tree::new("ListExpr").exprs("List", &e.list).end(),
        Expr::Paren(e) => Tree::new("ParenExpr").expr("X", &e.x).end(),
        Expr::Slice(e) => Tree::new("SliceExpr")
            .expr("X", &e.x)
            .opt("Lo", &e.lo)
            .opt("Hi", &e.hi)
            .opt("Step", &e.step)
            .end(),
        Expr::Tuple(e) => Tree::new("TupleExpr").exprs("List", &e.list).end(),
        Expr::Unary(e) => Tree::new("UnaryExpr")
            .token("Op", e.op)
            .opt("X", &e.x)
            .end(),
    }
}

fn stmt_tree(s: &Stmt) -> String {
    match s {
        Stmt::Assign(s) => Tree::new("AssignStmt")
            .token("Op", s.op)
            .expr("LHS", &s.lhs)
            .expr("RHS", &s.rhs)
            .end(),
        Stmt::Branch(s) => Tree::new("BranchStmt").token("Token", s.token).end(),
        Stmt::Def(s) => Tree::new("DefStmt")
            .field("Name", Some(s.name.name.clone()))
            .exprs("Params", &s.params)
            .stmts("Body", &s.body)
            .end(),
        Stmt::Expr(s) => Tree::new("ExprStmt").expr("X", &s.x).end(),
        Stmt::For(s) => Tree::new("ForStmt")
            .expr("Vars", &s.vars)
            .expr("X", &s.x)
            .stmts("Body", &s.body)
            .end(),
        Stmt::While(s) => Tree::new("WhileStmt")
            .expr("Cond", &s.cond)
            .stmts("Body", &s.body)
            .end(),
        Stmt::If(s) => Tree::new("IfStmt")
            .expr("Cond", &s.cond)
            .stmts("True", &s.true_)
            .stmts("False", &s.false_)
            .end(),
        Stmt::Load(s) => Tree::new("LoadStmt")
            .field("Module", Some(literal_tree(&s.module)))
            .list("From", s.from.iter().map(|id| id.name.clone()).collect())
            .list("To", s.to.iter().map(|id| id.name.clone()).collect())
            .end(),
        Stmt::Return(s) => Tree::new("ReturnStmt")
            .field("Result", s.result.as_ref().map(expr_tree))
            .end(),
    }
}

fn strip_pos(err: &Error) -> String {
    let s = err.to_string();
    match s.find(": ") {
        Some(i) => s[i + 2..].to_string(),
        None => s,
    }
}

fn opts() -> FileOptions {
    FileOptions::default()
}

#[test]
fn test_expr_parse_trees() {
    let tests: &[(&str, &str)] = &[
        ("print(1)", "(CallExpr Fn=print Args=(1))"),
        ("print(1)\n", "(CallExpr Fn=print Args=(1))"),
        ("x + 1", "(BinaryExpr X=x Op=+ Y=1)"),
        ("[x for x in y]", "(Comprehension Body=x Clauses=((ForClause Vars=x X=y)))"),
        (
            "[x for x in (a if b else c)]",
            "(Comprehension Body=x Clauses=((ForClause Vars=x X=(ParenExpr X=(CondExpr Cond=b True=a False=c)))))",
        ),
        ("x[i].f(42)", "(CallExpr Fn=(DotExpr X=(IndexExpr X=x Y=i) Name=f) Args=(42))"),
        ("x.f()", "(CallExpr Fn=(DotExpr X=x Name=f))"),
        ("x+y*z", "(BinaryExpr X=x Op=+ Y=(BinaryExpr X=y Op=* Y=z))"),
        ("x%y-z", "(BinaryExpr X=(BinaryExpr X=x Op=% Y=y) Op=- Y=z)"),
        ("a + b not in c", "(BinaryExpr X=(BinaryExpr X=a Op=+ Y=b) Op=not in Y=c)"),
        (
            "lambda x, *args, **kwargs: None",
            "(LambdaExpr Params=(x (UnaryExpr Op=* X=args) (UnaryExpr Op=** X=kwargs)) Body=None)",
        ),
        ("{\"one\": 1}", "(DictExpr List=((DictEntry Key=\"one\" Value=1)))"),
        ("a[i]", "(IndexExpr X=a Y=i)"),
        ("a[i:]", "(SliceExpr X=a Lo=i)"),
        ("a[:j]", "(SliceExpr X=a Hi=j)"),
        ("a[::]", "(SliceExpr X=a)"),
        ("a[::k]", "(SliceExpr X=a Step=k)"),
        ("[]", "(ListExpr)"),
        ("[1]", "(ListExpr List=(1))"),
        ("[1,]", "(ListExpr List=(1))"),
        ("[1, 2]", "(ListExpr List=(1 2))"),
        ("()", "(TupleExpr)"),
        ("(4,)", "(ParenExpr X=(TupleExpr List=(4)))"),
        ("(4)", "(ParenExpr X=4)"),
        ("(4, 5)", "(ParenExpr X=(TupleExpr List=(4 5)))"),
        ("1, 2, 3", "(TupleExpr List=(1 2 3))"),
        ("1, 2,", "unparenthesized tuple with trailing comma"),
        ("{}", "(DictExpr)"),
        ("{\"a\": 1}", "(DictExpr List=((DictEntry Key=\"a\" Value=1)))"),
        ("{\"a\": 1,}", "(DictExpr List=((DictEntry Key=\"a\" Value=1)))"),
        (
            "{\"a\": 1, \"b\": 2}",
            "(DictExpr List=((DictEntry Key=\"a\" Value=1) (DictEntry Key=\"b\" Value=2)))",
        ),
        (
            "{x: y for (x, y) in z}",
            "(Comprehension Curly Body=(DictEntry Key=x Value=y) Clauses=((ForClause Vars=(ParenExpr X=(TupleExpr List=(x y))) X=z)))",
        ),
        (
            "{x: y for a in b if c}",
            "(Comprehension Curly Body=(DictEntry Key=x Value=y) Clauses=((ForClause Vars=a X=b) (IfClause Cond=c)))",
        ),
        ("-1 + +2", "(BinaryExpr X=(UnaryExpr Op=- X=1) Op=+ Y=(UnaryExpr Op=+ X=2))"),
        ("\"foo\" + \"bar\"", "(BinaryExpr X=\"foo\" Op=+ Y=\"bar\")"),
        ("-1 * 2", "(BinaryExpr X=(UnaryExpr Op=- X=1) Op=* Y=2)"),
        ("-x[i]", "(UnaryExpr Op=- X=(IndexExpr X=x Y=i))"),
        (
            "a | b & c | d",
            "(BinaryExpr X=(BinaryExpr X=a Op=| Y=(BinaryExpr X=b Op=& Y=c)) Op=| Y=d)",
        ),
        (
            "a or b and c or d",
            "(BinaryExpr X=(BinaryExpr X=a Op=or Y=(BinaryExpr X=b Op=and Y=c)) Op=or Y=d)",
        ),
        (
            "a and b or c and d",
            "(BinaryExpr X=(BinaryExpr X=a Op=and Y=b) Op=or Y=(BinaryExpr X=c Op=and Y=d))",
        ),
        ("f(1, x=y)", "(CallExpr Fn=f Args=(1 (BinaryExpr X=x Op== Y=y)))"),
        (
            "f(*args, **kwargs)",
            "(CallExpr Fn=f Args=((UnaryExpr Op=* X=args) (UnaryExpr Op=** X=kwargs)))",
        ),
        (
            "lambda *args, *, x=1, **kwargs: 0",
            "(LambdaExpr Params=((UnaryExpr Op=* X=args) (UnaryExpr Op=*) (BinaryExpr X=x Op== Y=1) (UnaryExpr Op=** X=kwargs)) Body=0)",
        ),
        (
            "lambda *, a, *b: 0",
            "(LambdaExpr Params=((UnaryExpr Op=*) a (UnaryExpr Op=* X=b)) Body=0)",
        ),
        ("a if b else c", "(CondExpr Cond=b True=a False=c)"),
        ("a and not b", "(BinaryExpr X=a Op=and Y=(UnaryExpr Op=not X=b))"),
        (
            "[e for x in y if cond1 if cond2]",
            "(Comprehension Body=e Clauses=((ForClause Vars=x X=y) (IfClause Cond=cond1) (IfClause Cond=cond2)))",
        ),
    ];
    let mut failures = Vec::new();
    for &(input, want) in tests {
        let got = match parse_expr(&opts(), "foo.star", input.as_bytes()) {
            Ok(e) => expr_tree(&e),
            Err(e) => strip_pos(&e),
        };
        if got != want {
            failures.push(format!("parse `{input}` = {got}, want {want}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn test_stmt_parse_trees() {
    let tests: &[(&str, &str)] = &[
        ("print(1)", "(ExprStmt X=(CallExpr Fn=print Args=(1)))"),
        ("return 1, 2", "(ReturnStmt Result=(TupleExpr List=(1 2)))"),
        ("return", "(ReturnStmt)"),
        ("for i in \"abc\": break", "(ForStmt Vars=i X=\"abc\" Body=((BranchStmt Token=break)))"),
        (
            "for i in \"abc\": continue",
            "(ForStmt Vars=i X=\"abc\" Body=((BranchStmt Token=continue)))",
        ),
        (
            "for x, y in z: pass",
            "(ForStmt Vars=(TupleExpr List=(x y)) X=z Body=((BranchStmt Token=pass)))",
        ),
        ("if True: pass", "(IfStmt Cond=True True=((BranchStmt Token=pass)))"),
        ("if True: break", "(IfStmt Cond=True True=((BranchStmt Token=break)))"),
        ("if True: continue", "(IfStmt Cond=True True=((BranchStmt Token=continue)))"),
        (
            "if True: pass\nelse:\n\tpass",
            "(IfStmt Cond=True True=((BranchStmt Token=pass)) False=((BranchStmt Token=pass)))",
        ),
        (
            "if a: pass\nelif b: pass\nelse: pass",
            "(IfStmt Cond=a True=((BranchStmt Token=pass)) False=((IfStmt Cond=b True=((BranchStmt Token=pass)) False=((BranchStmt Token=pass)))))",
        ),
        (
            "x, y = 1, 2",
            "(AssignStmt Op== LHS=(TupleExpr List=(x y)) RHS=(TupleExpr List=(1 2)))",
        ),
        ("x[i] = 1", "(AssignStmt Op== LHS=(IndexExpr X=x Y=i) RHS=1)"),
        ("x.f = 1", "(AssignStmt Op== LHS=(DotExpr X=x Name=f) RHS=1)"),
        (
            "(x, y) = 1",
            "(AssignStmt Op== LHS=(ParenExpr X=(TupleExpr List=(x y))) RHS=1)",
        ),
        ("load(\"\", \"a\", b=\"c\")", "(LoadStmt Module=\"\" From=(a c) To=(a b))"),
        (
            "if True: load(\"\", \"a\", b=\"c\")",
            "(IfStmt Cond=True True=((LoadStmt Module=\"\" From=(a c) To=(a b))))",
        ),
        (
            "def f(x, *args, **kwargs):\n\tpass",
            "(DefStmt Name=f Params=(x (UnaryExpr Op=* X=args) (UnaryExpr Op=** X=kwargs)) Body=((BranchStmt Token=pass)))",
        ),
        (
            "def f(**kwargs, *args): pass",
            "(DefStmt Name=f Params=((UnaryExpr Op=** X=kwargs) (UnaryExpr Op=* X=args)) Body=((BranchStmt Token=pass)))",
        ),
        (
            "def f(a, b, c=d): pass",
            "(DefStmt Name=f Params=(a b (BinaryExpr X=c Op== Y=d)) Body=((BranchStmt Token=pass)))",
        ),
        (
            "def f(a, b=c, d): pass",
            "(DefStmt Name=f Params=(a (BinaryExpr X=b Op== Y=c) d) Body=((BranchStmt Token=pass)))",
        ),
        (
            "def f():\n\tdef g():\n\t\tpass\n\tpass\ndef h():\n\tpass",
            "(DefStmt Name=f Body=((DefStmt Name=g Body=((BranchStmt Token=pass))) (BranchStmt Token=pass)))",
        ),
        ("f();g()", "(ExprStmt X=(CallExpr Fn=f))"),
        ("f();", "(ExprStmt X=(CallExpr Fn=f))"),
        ("f();g()\n", "(ExprStmt X=(CallExpr Fn=f))"),
        ("f();\n", "(ExprStmt X=(CallExpr Fn=f))"),
    ];
    let mut failures = Vec::new();
    for &(input, want) in tests {
        match parse(&opts(), "foo.star", input.as_bytes()) {
            Ok(f) => {
                let got = stmt_tree(&f.stmts[0]);
                if got != want {
                    failures.push(format!("parse `{input}` = {got}, want {want}"));
                }
            }
            Err(e) => failures.push(format!("parse `{input}` failed: {}", strip_pos(&e))),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Tests sequences of statements, and particularly
/// handling of indentation, newlines, line continuations, and blank lines.
#[test]
fn test_file_parse_trees() {
    let tests: &[(&str, &str)] = &[
        (
            "x = 1\nprint(x)",
            "(AssignStmt Op== LHS=x RHS=1)\n(ExprStmt X=(CallExpr Fn=print Args=(x)))",
        ),
        ("if cond:\n\tpass", "(IfStmt Cond=cond True=((BranchStmt Token=pass)))"),
        (
            "if cond:\n\tpass\nelse:\n\tpass",
            "(IfStmt Cond=cond True=((BranchStmt Token=pass)) False=((BranchStmt Token=pass)))",
        ),
        (
            "def f():\n\tpass\npass\n\npass",
            "(DefStmt Name=f Body=((BranchStmt Token=pass)))\n(BranchStmt Token=pass)\n(BranchStmt Token=pass)",
        ),
        ("pass; pass", "(BranchStmt Token=pass)\n(BranchStmt Token=pass)"),
        ("pass\npass", "(BranchStmt Token=pass)\n(BranchStmt Token=pass)"),
        ("pass\n\npass", "(BranchStmt Token=pass)\n(BranchStmt Token=pass)"),
        (
            "x = (1 +\n2)",
            "(AssignStmt Op== LHS=x RHS=(ParenExpr X=(BinaryExpr X=1 Op=+ Y=2)))",
        ),
        ("x = 1 \\\n+ 2", "(AssignStmt Op== LHS=x RHS=(BinaryExpr X=1 Op=+ Y=2))"),
    ];
    let mut failures = Vec::new();
    for &(input, want) in tests {
        match parse(&opts(), "foo.star", input.as_bytes()) {
            Ok(f) => {
                let got: Vec<String> = f.stmts.iter().map(stmt_tree).collect();
                let got = got.join("\n");
                if got != want {
                    failures.push(format!("parse `{input}` = {got}, want {want}"));
                }
            }
            Err(e) => failures.push(format!("parse `{input}` failed: {}", strip_pos(&e))),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Tests handling of REPL-style compound statements.
#[test]
fn test_compound_stmt() {
    let tests: &[(&str, &str)] = &[
        // blank lines
        ("\n", ""),
        ("   \n", ""),
        ("# comment\n", ""),
        // simple statement
        ("1\n", "(ExprStmt X=1)"),
        ("print(1)\n", "(ExprStmt X=(CallExpr Fn=print Args=(1)))"),
        ("1;2;3;\n", "(ExprStmt X=1)(ExprStmt X=2)(ExprStmt X=3)"),
        (
            "f();g()\n",
            "(ExprStmt X=(CallExpr Fn=f))(ExprStmt X=(CallExpr Fn=g))",
        ),
        ("f();\n", "(ExprStmt X=(CallExpr Fn=f))"),
        ("f(\n\n\n\n\n\n\n)\n", "(ExprStmt X=(CallExpr Fn=f))"),
        // complex statements
        (
            "def f():\n  pass\n\n",
            "(DefStmt Name=f Body=((BranchStmt Token=pass)))",
        ),
        (
            "if cond:\n  pass\n\n",
            "(IfStmt Cond=cond True=((BranchStmt Token=pass)))",
        ),
        // Even as a 1-liner, the following blank line is required.
        (
            "if cond: pass\n\n",
            "(IfStmt Cond=cond True=((BranchStmt Token=pass)))",
        ),
        // github.com/google/starlark-go/issues/121
        ("a; b; c\n", "(ExprStmt X=a)(ExprStmt X=b)(ExprStmt X=c)"),
        ("a; b c\n", "invalid syntax"),
    ];
    let mut failures = Vec::new();
    for &(input, want) in tests {
        // Fake readline input from string.
        // The ! suffix, which would cause a parse error,
        // tests that the parser doesn't read more than necessary.
        let text = format!("{input}!");
        let mut lines = text.lines().map(|l| format!("{l}\n").into_bytes());
        let mut readline = || Ok(lines.next());
        let got = match parse_compound_stmt(&opts(), "foo.star", &mut readline) {
            Ok(f) => f.stmts.iter().map(stmt_tree).collect::<String>(),
            Err(e) => strip_pos(&e),
        };
        if got != want {
            failures.push(format!("parse `{input}` = {got}, want {want}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn test_compound_stmt_eof() {
    let mut readline = || Ok(None);
    let err = parse_compound_stmt(&opts(), "<stdin>", &mut readline).unwrap_err();
    assert_eq!(err.msg, "EOF");
}

#[test]
fn test_parse_errors() {
    let filename = concat!(env!("CARGO_MANIFEST_DIR"), "/syntax/testdata/errors.star");
    let mut report = common::Reporter::default();
    for mut chunk in common::read(filename, &mut report) {
        if let Err(err) = parse(&opts(), filename, chunk.source.as_bytes()) {
            chunk.got_error(&mut report, err.pos.line as usize, &err.msg);
        }
        chunk.done(&mut report);
    }
    report.check();
}

#[test]
fn test_parse_scan_star() {
    let filename = concat!(env!("CARGO_MANIFEST_DIR"), "/syntax/testdata/scan.star");
    let data = std::fs::read(filename).unwrap();
    parse(&opts(), filename, &data).unwrap();
}

#[test]
fn test_file_portion() {
    // Imagine that the Starlark file or expression print(x.f) is extracted
    // from the middle of a file in some hypothetical template language;
    // see https://github.com/google/starlark-go/issues/346. For example:
    // --
    // {{loop x seq}}
    //   {{print(x.f)}}
    // {{end}}
    // --
    let fp = FilePortion {
        content: b"print(x.f)",
        first_line: 2,
        first_col: 4,
    };
    let file = parse_file_portion(&opts(), "foo.template", &fp).unwrap_or_else(|e| panic!("{e}"));
    let (start, end) = file.stmts[0].span();
    let span = format!("{start} {end}");
    assert_eq!(span, "foo.template:2:4 foo.template:2:14", "wrong span");
}

// TestParserCallGraphCycles is not ported: it statically analyses the Go
// source of parse.go (using go/ast) to check that every recursive cycle
// of parser functions passes through the depth-limiting enter/leave
// mechanism. TestDepthLimit checks the resulting behaviour.

#[test]
fn test_span() {
    let f = parse(&opts(), "foo.star", b"print(x.f)").unwrap();
    let (start, end) = f.stmts[0].span();
    assert_eq!(format!("{start} {end}"), "foo.star:1:1 foo.star:1:11");
}

/// Rather than exceed the stack during parsing, deeply nested
/// expressions and statements are rejected by the recursion
/// counters in the scanner and/or parser.
#[test]
fn test_depth_limit() {
    let n = 10_000_000;
    let srcs = [
        "(".repeat(n), // (rejected in scanner)
        "not ".repeat(n) + "0",
        "-".repeat(n) + "0",
        "0 if a else ".repeat(n) + "0",
        "lambda: ".repeat(n) + "0",
    ];
    // Run on a thread with a stack the size of a typical main thread.
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            for src in &srcs {
                match parse(&opts(), "limit.star", src.as_bytes()) {
                    Ok(_) => panic!("Parse succeeded unexpectedly"),
                    Err(e) => assert!(
                        e.to_string().contains("excessive nesting"),
                        "got error {e}, want excessive nesting error"
                    ),
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
