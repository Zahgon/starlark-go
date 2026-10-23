// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Runs the evaluator tests in starlark/testdata (ported from
//! starlark/eval_test.go, internal/chunkedfile and starlarktest).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use regex::bytes::Regex;

use starlark::starlark::{
    as_signed, as_unsigned, equal, eval, exec_file, exec_file_options, exec_repl_chunk,
    legacy_file_options, source_program_options, unpack_args, Builtin, Dict, Error, EvalError, Int,
    StarlarkValue, StringDict, Thread, Unpacker, Value, ValueIterator,
};
use starlark::starlarkstruct;
use starlark::syntax::{self, FileOptions, Token};

#[path = "common/starlarktest.rs"]
mod starlarktest;

use starlarktest::{go_regex, load_assert_module, set_reporter};

// ---- chunkedfile ----

struct Chunk {
    source: String,
    filename: String,
    want_errs: BTreeMap<i32, Regex>,
}

/// Interprets a Go string literal (as strconv.Unquote does).
fn go_unquote(s: &str) -> Option<String> {
    if s.len() >= 2 && s.starts_with('`') && s.ends_with('`') {
        return Some(s[1..s.len() - 1].to_string());
    }
    if s.len() < 2 || !s.starts_with('"') || !s.ends_with('"') {
        return None;
    }
    let body = &s[1..s.len() - 1];
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'v' => out.push('\x0b'),
            'x' => {
                let h: String = chars.by_ref().take(2).collect();
                out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
            }
            'u' => {
                let h: String = chars.by_ref().take(4).collect();
                out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
            }
            'U' => {
                let h: String = chars.by_ref().take(8).collect();
                out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn read_chunks(filename: &str, report: &mut Vec<String>) -> Vec<Chunk> {
    let data = std::fs::read_to_string(filename).expect("read test file");
    let mut chunks = Vec::new();
    let mut linenum = 1;
    for chunk in data.split("\n---\n") {
        // Pad with newlines so the line numbers match the original file.
        let src = "\n".repeat(linenum - 1) + chunk;
        let mut want_errs = BTreeMap::new();
        for line in chunk.split('\n') {
            if let Some((_, after)) = line.split_once("###") {
                let rest = after.trim();
                match go_unquote(rest) {
                    None => report.push(format!(
                        "{}:{}: not a quoted regexp: {}",
                        filename, linenum, rest
                    )),
                    Some(p) => match Regex::new(&go_regex(&p)) {
                        Ok(rx) => {
                            want_errs.insert(linenum as i32, rx);
                        }
                        Err(e) => report.push(format!("{}:{}: {}", filename, linenum, e)),
                    },
                }
            }
            linenum += 1;
        }
        chunks.push(Chunk {
            source: src,
            filename: filename.to_string(),
            want_errs,
        });
    }
    chunks
}

impl Chunk {
    fn got_error(&mut self, linenum: i32, msg: &str, report: &mut Vec<String>) {
        match self.want_errs.remove(&linenum) {
            Some(rx) => {
                if !rx.is_match(msg.as_bytes()) {
                    report.push(format!(
                        "{}:{}: error {:?} does not match pattern {:?}",
                        self.filename,
                        linenum,
                        msg,
                        rx.as_str()
                    ));
                }
            }
            None => report.push(format!(
                "{}:{}: unexpected error: {}",
                self.filename, linenum, msg
            )),
        }
    }

    fn done(&self, report: &mut Vec<String>) {
        for (linenum, rx) in &self.want_errs {
            report.push(format!(
                "{}:{}: expected error matching {:?}",
                self.filename,
                linenum,
                rx.as_str()
            ));
        }
    }
}

// ---- test-only value types ----

/// An iterable value representing the infinite Fibonacci sequence.
struct Fib;

struct FibIterator {
    x: i64,
    y: i64,
}

impl ValueIterator for FibIterator {
    fn next(&mut self) -> Option<Value> {
        let v = Value::int(self.x);
        (self.x, self.y) = (self.y, self.x.wrapping_add(self.y));
        Some(v)
    }
}

impl StarlarkValue for Fib {
    fn type_name(&self) -> &str {
        "fib"
    }
    fn repr(&self, out: &mut String) {
        out.push_str("fib");
    }
    fn hash(&self) -> Result<u32, Error> {
        Err(Error::new("fib is unhashable"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn iterate(&self) -> Option<Box<dyn ValueIterator>> {
        Some(Box::new(FibIterator { x: 0, y: 1 }))
    }
}

/// A test-only implementation of HasAttrs. It permits any field to be set.
struct HasFields {
    attrs: RefCell<StringDict>,
    frozen: Cell<bool>,
}

impl StarlarkValue for HasFields {
    fn type_name(&self) -> &str {
        "hasfields"
    }
    fn repr(&self, out: &mut String) {
        out.push_str("hasfields");
    }
    fn hash(&self) -> Result<u32, Error> {
        Ok(42)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn freeze(&self) {
        if !self.frozen.get() {
            self.frozen.set(true);
            self.attrs.borrow().freeze();
        }
    }
    fn has_attrs(&self) -> bool {
        true
    }
    fn attr(&self, _this: &Value, name: &str) -> Result<Option<Value>, Error> {
        Ok(self.attrs.borrow().get(name).cloned())
    }
    fn attr_names(&self) -> Vec<String> {
        self.attrs.borrow().keys()
    }
    fn set_field(&self, name: &str, v: Value) -> Option<Result<(), Error>> {
        if self.frozen.get() {
            return Some(Err(Error::new("cannot set field on a frozen hasfields")));
        }
        if name.starts_with("no") {
            // for testing
            return Some(Err(Error::NoSuchAttr(format!("no .{} field", name))));
        }
        self.attrs.borrow_mut().insert(name, v);
        Some(Ok(()))
    }
    fn binary(
        &self,
        op: Token,
        y: &Value,
        _side: starlark::starlark::Side,
    ) -> Result<Option<Value>, Error> {
        // This method exists so we can exercise 'list += x'
        // where x is not Iterable but defines list+x.
        if op == Token::Plus && matches!(y, Value::List(_)) {
            return Ok(Some(Value::int(42))); // list+hasfields is 42
        }
        Ok(None)
    }
}

fn new_has_fields(
    _: &mut Thread,
    b: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    if !args.is_empty() || !kwargs.is_empty() {
        return Err(Error::new(format!("{}: unexpected arguments", b.name())));
    }
    Ok(Value::other(HasFields {
        attrs: RefCell::new(StringDict::new()),
        frozen: Cell::new(false),
    }))
}

/// Implements the 'load' operation as used in the evaluator tests.
fn load(thread: &mut Thread, module: &str) -> Result<StringDict, Error> {
    if module == "assert.star" {
        return load_assert_module();
    }
    for (name, m) in starlark::lib::modules() {
        if module == format!("{}.star", name) {
            let mut d = StringDict::new();
            d.insert(name, m);
            return Ok(d);
        }
    }
    let caller = thread.call_frame(0).pos.filename().to_string();
    let dir = std::path::Path::new(&caller)
        .parent()
        .unwrap_or(std::path::Path::new(""));
    let filename = dir.join(module).to_string_lossy().into_owned();
    exec_file(thread, &filename, None, &StringDict::new())
}

fn get_options(src: &str) -> FileOptions {
    let option = |name: &str| src.contains(&format!("option:{}", name));
    FileOptions {
        set: option("set"),
        while_: option("while"),
        top_level_control: option("toplevelcontrol"),
        global_reassign: option("globalreassign"),
        load_binds_globally: option("loadbindsglobally"),
        recursion: option("recursion"),
    }
}

fn run_file(file: &str) {
    let filename = format!("starlark/testdata/{}", file);
    let mut thread = Thread::new("");
    thread.load = Some(Rc::new(load));
    let reports = set_reporter(&mut thread);

    let mut report = Vec::new();
    for mut chunk in read_chunks(&filename, &mut report) {
        let mut predeclared = StringDict::new();
        predeclared.insert("hasfields", Builtin::value("hasfields", new_has_fields));
        predeclared.insert("fibonacci", Value::other(Fib));
        predeclared.insert("struct", Builtin::value("struct", starlarkstruct::make));

        let opts = get_options(&chunk.source);
        let src = chunk.source.clone();
        match exec_file_options(
            &opts,
            &mut thread,
            &filename,
            Some(src.as_bytes()),
            &predeclared,
        ) {
            Ok(_) => {}
            Err(err) => match err.as_eval() {
                Some(e) => {
                    let found = (0..e.call_stack.len())
                        .map(|i| e.call_stack.at(i))
                        .find(|fr| fr.pos.filename() == filename)
                        .map(|fr| fr.pos.line);
                    match found {
                        Some(line) => chunk.got_error(line, &e.msg, &mut report),
                        None => report.push(e.backtrace()),
                    }
                }
                None => report.push(format!("\n{}", err)),
            },
        }
        chunk.done(&mut report);
    }
    report.extend(reports.borrow().iter().cloned());
    if !report.is_empty() {
        panic!("{} failures:\n{}", report.len(), report.join("\n"));
    }
}

macro_rules! testdata {
    ($($name:ident => $file:expr),* $(,)?) => {$(
        #[test]
        fn $name() {
            // Deep Starlark recursion needs a large native stack.
            let h = std::thread::Builder::new().stack_size(4 << 30).spawn(|| run_file($file)).unwrap();
            if let Err(e) = h.join() {
                std::panic::resume_unwind(e);
            }
        }
    )*};
}

testdata! {
    assign => "assign.star",
    bool => "bool.star",
    builtins => "builtins.star",
    bytes => "bytes.star",
    control => "control.star",
    dict => "dict.star",
    float => "float.star",
    function => "function.star",
    int => "int.star",
    json => "json.star",
    list => "list.star",
    math => "math.star",
    misc => "misc.star",
    set => "set.star",
    string => "string.star",
    time => "time.star",
    tuple => "tuple.star",
    recursion => "recursion.star",
    module => "module.star",
    while_ => "while.star",
}

// ---- unit tests of the evaluator API (the rest of starlark/eval_test.go) ----

/// Returns v.String() or err.Error(), as the Go tests print them.
fn value_or_error(r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => v.to_string(),
        Err(err) => err.to_string(),
    }
}

#[test]
fn test_eval_expr() {
    // This is mostly redundant with the new *.star tests.
    let mut thread = Thread::new("");
    let tests: &[(&str, &str)] = &[
        ("123", "123"),
        ("-1", "-1"),
        (r#""a"+"b""#, r#""ab""#),
        ("1+2", "3"),
        // lists
        ("[]", "[]"),
        ("[1]", "[1]"),
        ("[1,]", "[1]"),
        ("[1, 2]", "[1, 2]"),
        ("[2 * x for x in [1, 2, 3]]", "[2, 4, 6]"),
        ("[2 * x for x in [1, 2, 3] if x > 1]", "[4, 6]"),
        (
            "[(x, y) for x in [1, 2] for y in [3, 4]]",
            "[(1, 3), (1, 4), (2, 3), (2, 4)]",
        ),
        (
            "[(x, y) for x in [1, 2] if x == 2 for y in [3, 4]]",
            "[(2, 3), (2, 4)]",
        ),
        // tuples
        ("()", "()"),
        ("(1)", "1"),
        ("(1,)", "(1,)"),
        ("(1, 2)", "(1, 2)"),
        ("(1, 2, 3, 4, 5)", "(1, 2, 3, 4, 5)"),
        ("1, 2", "(1, 2)"),
        // dicts
        ("{}", "{}"),
        (r#"{"a": 1}"#, r#"{"a": 1}"#),
        (r#"{"a": 1,}"#, r#"{"a": 1}"#),
        // conditional
        ("1 if 3 > 2 else 0", "1"),
        (r#"1 if "foo" else 0"#, "1"),
        (r#"1 if "" else 0"#, "0"),
        // indexing
        (r#"["a", "b"][0]"#, r#""a""#),
        (r#"["a", "b"][1]"#, r#""b""#),
        (r#"("a", "b")[0]"#, r#""a""#),
        (r#"("a", "b")[1]"#, r#""b""#),
        (r#""aΩb"[0]"#, r#""a""#),
        (r#""aΩb"[1]"#, r#""\xce""#),
        (r#""aΩb"[3]"#, r#""b""#),
        (r#"{"a": 1}["a"]"#, "1"),
        (r#"{"a": 1}["b"]"#, r#"key "b" not in dict"#),
        ("{}[[]]", "unhashable type: list"),
        (r#"{"a": 1}[[]]"#, "unhashable type: list"),
        ("[x for x in range(3)]", "[0, 1, 2]"),
    ];
    let mut failures = Vec::new();
    for &(src, want) in tests {
        let got = value_or_error(eval(
            &mut thread,
            "<expr>",
            src.as_bytes(),
            &StringDict::new(),
        ));
        if got != want {
            failures.push(format!("eval {} = {}, want {}", src, got, want));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn test_parameter_passing() {
    let filename = "parameters.go";
    let src = "
def a():
\treturn
def b(a, b):
\treturn a, b
def c(a, b=42):
\treturn a, b
def d(*args):
\treturn args
def e(**kwargs):
\treturn kwargs
def f(a, b=42, *args, **kwargs):
\treturn a, b, args, kwargs
def g(a, b=42, *args, c=123, **kwargs):
\treturn a, b, args, c, kwargs
def h(a, b=42, *, c=123, **kwargs):
\treturn a, b, c, kwargs
def i(a, b=42, *, c, d=123, e, **kwargs):
\treturn a, b, c, d, e, kwargs
def j(a, b=42, *args, c, d=123, e, **kwargs):
\treturn a, b, args, c, d, e, kwargs
";

    let mut thread = Thread::new("");
    let globals = exec_file(
        &mut thread,
        filename,
        Some(src.as_bytes()),
        &StringDict::new(),
    )
    .unwrap_or_else(|err| panic!("{}", err));

    // All errors are dynamic; see resolver for static errors.
    let tests: &[(&str, &str)] = &[
        // a()
        ("a()", "None"),
        ("a(1)", "function a accepts no arguments (1 given)"),
        // b(a, b)
        ("b()", "function b missing 2 arguments (a, b)"),
        ("b(1)", "function b missing 1 argument (b)"),
        ("b(a=1)", "function b missing 1 argument (b)"),
        ("b(b=1)", "function b missing 1 argument (a)"),
        ("b(1, 2)", "(1, 2)"),
        ("b", "<function b>"), // asserts that b's parameter b was treated as a local variable
        (
            "b(1, 2, 3)",
            "function b accepts 2 positional arguments (3 given)",
        ),
        ("b(1, b=2)", "(1, 2)"),
        (
            "b(1, a=2)",
            r#"function b got multiple values for parameter "a""#,
        ),
        (
            "b(1, x=2)",
            r#"function b got an unexpected keyword argument "x""#,
        ),
        ("b(a=1, b=2)", "(1, 2)"),
        ("b(b=1, a=2)", "(2, 1)"),
        (
            "b(b=1, a=2, x=1)",
            r#"function b got an unexpected keyword argument "x""#,
        ),
        (
            "b(x=1, b=1, a=2)",
            r#"function b got an unexpected keyword argument "x""#,
        ),
        // c(a, b=42)
        ("c()", "function c missing 1 argument (a)"),
        ("c(1)", "(1, 42)"),
        ("c(1, 2)", "(1, 2)"),
        (
            "c(1, 2, 3)",
            "function c accepts at most 2 positional arguments (3 given)",
        ),
        ("c(1, b=2)", "(1, 2)"),
        (
            "c(1, a=2)",
            r#"function c got multiple values for parameter "a""#,
        ),
        ("c(a=1, b=2)", "(1, 2)"),
        ("c(b=1, a=2)", "(2, 1)"),
        // d(*args)
        ("d()", "()"),
        ("d(1)", "(1,)"),
        ("d(1, 2)", "(1, 2)"),
        (
            "d(1, 2, k=3)",
            r#"function d got an unexpected keyword argument "k""#,
        ),
        (
            "d(args=[])",
            r#"function d got an unexpected keyword argument "args""#,
        ),
        // e(**kwargs)
        ("e()", "{}"),
        (
            "e(1)",
            "function e accepts 0 positional arguments (1 given)",
        ),
        ("e(k=1)", r#"{"k": 1}"#),
        ("e(kwargs={})", r#"{"kwargs": {}}"#),
        // f(a, b=42, *args, **kwargs)
        ("f()", "function f missing 1 argument (a)"),
        ("f(0)", "(0, 42, (), {})"),
        ("f(0)", "(0, 42, (), {})"),
        ("f(0, 1)", "(0, 1, (), {})"),
        ("f(0, 1, 2)", "(0, 1, (2,), {})"),
        ("f(0, 1, 2, 3)", "(0, 1, (2, 3), {})"),
        ("f(a=0)", "(0, 42, (), {})"),
        ("f(0, b=1)", "(0, 1, (), {})"),
        (
            "f(0, a=1)",
            r#"function f got multiple values for parameter "a""#,
        ),
        ("f(0, b=1, c=2)", r#"(0, 1, (), {"c": 2})"#),
        // g(a, b=42, *args, c=123, **kwargs)
        ("g()", "function g missing 1 argument (a)"),
        ("g(0)", "(0, 42, (), 123, {})"),
        ("g(0, 1)", "(0, 1, (), 123, {})"),
        ("g(0, 1, 2)", "(0, 1, (2,), 123, {})"),
        ("g(0, 1, 2, 3)", "(0, 1, (2, 3), 123, {})"),
        ("g(a=0)", "(0, 42, (), 123, {})"),
        ("g(0, b=1)", "(0, 1, (), 123, {})"),
        (
            "g(0, a=1)",
            r#"function g got multiple values for parameter "a""#,
        ),
        ("g(0, b=1, c=2, d=3)", r#"(0, 1, (), 2, {"d": 3})"#),
        // h(a, b=42, *, c=123, **kwargs)
        ("h()", "function h missing 1 argument (a)"),
        ("h(0)", "(0, 42, 123, {})"),
        ("h(0, 1)", "(0, 1, 123, {})"),
        (
            "h(0, 1, 2)",
            "function h accepts at most 2 positional arguments (3 given)",
        ),
        ("h(a=0)", "(0, 42, 123, {})"),
        ("h(0, b=1)", "(0, 1, 123, {})"),
        (
            "h(0, a=1)",
            r#"function h got multiple values for parameter "a""#,
        ),
        ("h(0, b=1, c=2)", "(0, 1, 2, {})"),
        ("h(0, b=1, d=2)", r#"(0, 1, 123, {"d": 2})"#),
        ("h(0, b=1, c=2, d=3)", r#"(0, 1, 2, {"d": 3})"#),
        // i(a, b=42, *, c, d=123, e, **kwargs)
        ("i()", "function i missing 3 arguments (a, c, e)"),
        ("i(0)", "function i missing 2 arguments (c, e)"),
        ("i(0, 1)", "function i missing 2 arguments (c, e)"),
        (
            "i(0, 1, 2)",
            "function i accepts at most 2 positional arguments (3 given)",
        ),
        ("i(0, 1, e=2)", "function i missing 1 argument (c)"),
        (
            "i(0, 1, 2, 3)",
            "function i accepts at most 2 positional arguments (4 given)",
        ),
        ("i(a=0)", "function i missing 2 arguments (c, e)"),
        ("i(0, b=1)", "function i missing 2 arguments (c, e)"),
        (
            "i(0, a=1)",
            r#"function i got multiple values for parameter "a""#,
        ),
        ("i(0, b=1, c=2)", "function i missing 1 argument (e)"),
        ("i(0, b=1, d=2)", "function i missing 2 arguments (c, e)"),
        ("i(0, b=1, c=2, d=3)", "function i missing 1 argument (e)"),
        ("i(0, b=1, c=2, d=3, e=4)", "(0, 1, 2, 3, 4, {})"),
        (
            "i(0, 1, b=1, c=2, d=3, e=4)",
            r#"function i got multiple values for parameter "b""#,
        ),
        // j(a, b=42, *args, c, d=123, e, **kwargs)
        ("j()", "function j missing 3 arguments (a, c, e)"),
        ("j(0)", "function j missing 2 arguments (c, e)"),
        ("j(0, 1)", "function j missing 2 arguments (c, e)"),
        ("j(0, 1, 2)", "function j missing 2 arguments (c, e)"),
        ("j(0, 1, e=2)", "function j missing 1 argument (c)"),
        ("j(0, 1, 2, 3)", "function j missing 2 arguments (c, e)"),
        ("j(a=0)", "function j missing 2 arguments (c, e)"),
        ("j(0, b=1)", "function j missing 2 arguments (c, e)"),
        (
            "j(0, a=1)",
            r#"function j got multiple values for parameter "a""#,
        ),
        ("j(0, b=1, c=2)", "function j missing 1 argument (e)"),
        ("j(0, b=1, d=2)", "function j missing 2 arguments (c, e)"),
        ("j(0, b=1, c=2, d=3)", "function j missing 1 argument (e)"),
        ("j(0, b=1, c=2, d=3, e=4)", "(0, 1, (), 2, 3, 4, {})"),
        (
            "j(0, 1, b=1, c=2, d=3, e=4)",
            r#"function j got multiple values for parameter "b""#,
        ),
        ("j(0, 1, 2, c=3, e=4)", "(0, 1, (2,), 3, 123, 4, {})"),
    ];
    let mut failures = Vec::new();
    for &(src, want) in tests {
        let got = value_or_error(eval(&mut thread, "<expr>", src.as_bytes(), &globals));
        if got != want {
            failures.push(format!("eval {} = {}, want {}", src, got, want));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Ensures that the Starlark print function calls Thread.print, if provided.
#[test]
fn test_print() {
    let src = r#"
print("hello")
def f(): print("hello", "world", sep=", ")
f()
"#;
    let buf = Rc::new(RefCell::new(String::new()));
    let out = buf.clone();
    let mut thread = Thread::new("");
    thread.print = Some(Rc::new(move |thread: &Thread, msg: &[u8]| {
        let caller = thread.call_frame(1);
        out.borrow_mut().push_str(&format!(
            "{}: {}: {}\n",
            caller.pos,
            caller.name,
            String::from_utf8_lossy(msg)
        ));
    }));
    if let Err(err) = exec_file(
        &mut thread,
        "foo.star",
        Some(src.as_bytes()),
        &StringDict::new(),
    ) {
        panic!("{}", err);
    }
    let want = "foo.star:2:6: <toplevel>: hello\nfoo.star:3:15: f: hello, world\n";
    assert_eq!(
        *buf.borrow(),
        want,
        "output was {}, want {}",
        buf.borrow(),
        want
    );
}

/// Exercises the Int.to_i64 and Int.to_u64 methods (Go's Int64 and Uint64).
#[test]
fn test_int() {
    let one = Int::from_i64(1);

    let tests = [
        (Int::from_i64(i64::MIN).sub(&one), "error", "error"),
        (Int::from_i64(i64::MIN), "-9223372036854775808", "error"),
        (Int::from_i64(-1), "-1", "error"),
        (Int::from_i64(0), "0", "0"),
        (Int::from_i64(1), "1", "1"),
        (
            Int::from_i64(i64::MAX),
            "9223372036854775807",
            "9223372036854775807",
        ),
        (Int::from_u64(u64::MAX), "error", "18446744073709551615"),
        (Int::from_u64(u64::MAX).add(&one), "error", "error"),
    ];
    let mut failures = Vec::new();
    for (i, want_int64, want_uint64) in tests {
        let got_int64 = i.to_i64().map_or("error".to_string(), |x| x.to_string());
        let got_uint64 = i.to_u64().map_or("error".to_string(), |x| x.to_string());
        if got_int64 != want_int64 {
            failures.push(format!(
                "({}).Int64() = {}, want {}",
                i, got_int64, want_int64
            ));
        }
        if got_uint64 != want_uint64 {
            failures.push(format!(
                "({}).Uint64() = {}, want {}",
                i, got_uint64, want_uint64
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn backtrace(r: &Result<StringDict, Error>) -> String {
    match r {
        Err(Error::Eval(err)) => err.backtrace(),
        Ok(_) => panic!("ExecFile succeeded unexpectedly"),
        Err(err) => panic!("ExecFile failed with {}, wanted *EvalError", err),
    }
}

#[test]
fn test_backtrace() {
    // This test ensures continuity of the stack of active Starlark
    // functions, including propagation through built-ins such as 'min'.
    let src = "
def f(x): return 1//x
def g(x): return f(x)
def h(): return min([1, 2, 0], key=g)
def i(): return h()
i()
";
    let mut thread = Thread::new("");
    let r = exec_file(
        &mut thread,
        "crash.star",
        Some(src.as_bytes()),
        &StringDict::new(),
    );
    let want = "Traceback (most recent call last):
  crash.star:6:2: in <toplevel>
  crash.star:5:18: in i
  crash.star:4:20: in h
  <builtin>: in min
  crash.star:3:19: in g
  crash.star:2:19: in f
Error: floored division by zero";
    assert_eq!(backtrace(&r), want);

    // Additionally, ensure that errors originating in
    // Starlark and/or Rust each have an accurate frame.
    // The topmost frame, if built-in, is not shown,
    // but the name of the built-in function is shown
    // as "Error in fn: ...".
    //
    // This program fails in Starlark (f) if x==0,
    // or in Rust (string.join) if x is non-zero.
    let src2 = "
def f(): ''.join([1//i])
f()
";
    let wants = [
        "Traceback (most recent call last):
  crash.star:3:2: in <toplevel>
  crash.star:2:20: in f
Error: floored division by zero",
        "Traceback (most recent call last):
  crash.star:3:2: in <toplevel>
  crash.star:2:17: in f
Error in join: join: in list, want string, got int",
    ];
    for (i, want) in wants.iter().enumerate() {
        let mut globals = StringDict::new();
        globals.insert("i", Value::int(i as i64));
        let r = exec_file(&mut thread, "crash.star", Some(src2.as_bytes()), &globals);
        assert_eq!(backtrace(&r), *want);
    }
}

#[test]
fn test_load_backtrace() {
    // This test ensures that load() does NOT preserve stack traces,
    // but that API callers can get them by unwrapping the error causes.
    // For discussion, see:
    // https://github.com/google/starlark-go/pull/244
    let src = "
load('crash.star', 'x')
";
    let loaded_src = "
def f(x):
  return 1 // x

f(0)
";
    let mut thread = Thread::new("");
    thread.load = Some(Rc::new(move |_: &mut Thread, module: &str| {
        exec_file(
            &mut Thread::new(""),
            module,
            Some(loaded_src.as_bytes()),
            &StringDict::new(),
        )
    }));
    let r = exec_file(
        &mut thread,
        "root.star",
        Some(src.as_bytes()),
        &StringDict::new(),
    );

    let want = "Traceback (most recent call last):
  root.star:2:1: in <toplevel>
Error: cannot load crash.star: floored division by zero";
    assert_eq!(backtrace(&r), want);

    // Returns the innermost EvalError in the chain of causes.
    fn unwrap_eval_error(err: &Error) -> Option<Rc<EvalError>> {
        let mut result = None;
        let mut err = Some(err.clone());
        while let Some(e) = err {
            err = match &e {
                Error::Eval(eval_err) => {
                    result = Some(eval_err.clone());
                    eval_err.cause.clone()
                }
                Error::Wrapped(_, cause) => Some(cause.as_ref().clone()),
                _ => None,
            };
        }
        result
    }

    let Err(err) = &r else {
        panic!("ExecFile succeeded unexpectedly");
    };
    let unwrapped = unwrap_eval_error(err).expect("EvalError");
    let want_unwrapped = "Traceback (most recent call last):
  crash.star:5:2: in <toplevel>
  crash.star:3:12: in f
Error: floored division by zero";
    assert_eq!(backtrace(&Err(Error::Eval(unwrapped))), want_unwrapped);
}

/// Parses and resolves a file syntax tree once then executes it
/// repeatedly with different values of its predeclared variables.
#[test]
fn test_repeated_exec() {
    let mut predeclared = StringDict::new();
    predeclared.insert("x", Value::None);
    let prog = {
        let p = &predeclared;
        source_program_options(
            &legacy_file_options(),
            "repeat.star",
            Some(b"y = 2 * x"),
            &|name| p.has(name),
        )
        .unwrap_or_else(|err| panic!("{}", err))
    };

    let tests = [
        (Value::int(42), Value::int(84)),
        (Value::string("mur"), Value::string("murmur")),
        (
            Value::tuple(vec![Value::None]),
            Value::tuple(vec![Value::None, Value::None]),
        ),
    ];
    for (x, want) in tests {
        predeclared.insert("x", x.clone()); // update the values in dictionary
        let mut thread = Thread::new("");
        let (globals, err) = prog.init(&mut thread, &predeclared);
        if let Some(err) = err {
            panic!("x={}: {}", x, err); // exec error
        }
        let y = globals.get("y").cloned().unwrap_or(Value::None);
        match equal(&y, &want) {
            Err(err) => panic!("x={}: {}", x, err), // comparison error
            Ok(eq) => assert!(eq, "x={}: got y={}, want {}", x, y, want),
        }
    }
}

/// Ensures that even Programs from empty files have a valid position.
#[test]
fn test_empty_position() {
    let predeclared = StringDict::new();
    for content in ["", "empty = False"] {
        let prog = source_program_options(
            &legacy_file_options(),
            "hello.star",
            Some(content.as_bytes()),
            &|name| predeclared.has(name),
        )
        .unwrap_or_else(|err| panic!("{}", err));
        assert_eq!(prog.filename(), "hello.star", "Program.Filename()");
    }
}

/// A parameter variable that accepts hasfields values (the Rust analogue
/// of unpacking into a *hasfields).
struct HasFieldsArg(Option<Value>);

impl Unpacker for HasFieldsArg {
    fn unpack(&mut self, v: &Value) -> Result<(), Error> {
        if v.downcast_ref::<HasFields>().is_none() {
            return Err(Error::new(format!("got {}, want hasfields", v.type_name())));
        }
        self.0 = Some(v.clone());
        Ok(())
    }
}

/// Tests that user-defined implementations of StarlarkValue may be unpacked.
#[test]
fn test_unpack_user_defined() {
    // success
    let want = Value::other(HasFields {
        attrs: RefCell::new(StringDict::new()),
        frozen: Cell::new(false),
    });
    let mut x = HasFieldsArg(None);
    if let Err(err) = unpack_args(
        "unpack",
        std::slice::from_ref(&want),
        &[],
        &mut [("x", &mut x)],
    ) {
        panic!("UnpackArgs failed: {}", err);
    }
    match &x.0 {
        Some(got) if got.is_identical(&want) => {}
        got => panic!("for x, got {:?}, want {}", got, want),
    }

    // failure
    let err = unpack_args("unpack", &[Value::int(42)], &[], &mut [("x", &mut x)]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "unpack: for parameter x: got int, want hasfields",
        "unpack args error"
    );
}

#[derive(Clone, Debug, Default, PartialEq)]
struct OptionalStringUnpacker {
    str: String,
    is_set: bool,
}

impl Unpacker for OptionalStringUnpacker {
    fn unpack(&mut self, v: &Value) -> Result<(), Error> {
        let Some(s) = v.as_str() else {
            return Err(Error::new(format!("got {}, want string", v.type_name())));
        };
        self.str = String::from_utf8_lossy(s).into_owned();
        self.is_set = true;
        Ok(())
    }
}

/// Lets two parameters share one variable (as the Go test does with &a).
struct SharedUnpacker<'a>(&'a RefCell<OptionalStringUnpacker>);

impl Unpacker for SharedUnpacker<'_> {
    fn unpack(&mut self, v: &Value) -> Result<(), Error> {
        self.0.borrow_mut().unpack(v)
    }
}

fn osu(s: &str, is_set: bool) -> OptionalStringUnpacker {
    OptionalStringUnpacker {
        str: s.to_string(),
        is_set,
    }
}

#[test]
fn test_unpack_custom_unpacker() {
    let mut a = OptionalStringUnpacker::default();
    let want_a = osu("a", true);
    let mut b = osu("b", false);
    let want_b = osu("b", false);

    // Success
    if let Err(err) = unpack_args(
        "unpack",
        &[Value::string("a")],
        &[],
        &mut [("a?", &mut a), ("b?", &mut b)],
    ) {
        panic!("UnpackArgs failed: {}", err);
    }
    assert_eq!(a, want_a, "for a");
    assert_eq!(b, want_b, "for b");

    // failure
    let err = unpack_args("unpack", &[Value::int(42)], &[], &mut [("a", &mut a)]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "unpack: for parameter a: got int, want string",
        "unpack args error"
    );
}

#[test]
fn test_unpack_none_coalescing() {
    let a = RefCell::new(osu("a", false));
    let want_a = osu("a", false);
    let b = osu("b", false);
    let want_b = osu("b", false);

    // Success
    let args = [Value::None];
    let kwargs = [(Value::string("b"), Value::None)];
    if let Err(err) = unpack_args(
        "unpack",
        &args,
        &kwargs,
        &mut [
            ("a??", &mut SharedUnpacker(&a)),
            ("b??", &mut SharedUnpacker(&a)),
        ],
    ) {
        panic!("UnpackArgs failed: {}", err);
    }
    assert_eq!(*a.borrow(), want_a, "for a");
    assert_eq!(b, want_b, "for b");

    // failure
    let err = unpack_args(
        "unpack",
        &[Value::int(42)],
        &[],
        &mut [("a??", &mut SharedUnpacker(&a))],
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "unpack: for parameter a: got int, want string",
        "unpack args error"
    );

    let err = unpack_args(
        "unpack",
        &[],
        &[
            (Value::string("a"), Value::None),
            (Value::string("a"), Value::None),
        ],
        &mut [("a??", &mut SharedUnpacker(&a))],
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"unpack: got multiple values for keyword argument "a""#,
        "unpack args error"
    );
}

#[test]
fn test_unpack_required_after_optional() {
    // Assert 'c' is implicitly optional
    let (mut a, mut b, mut c) = (String::new(), String::new(), String::new());
    let args = [Value::string("a")];
    if let Err(err) = unpack_args(
        "unpack",
        &args,
        &[],
        &mut [("a", &mut a), ("b?", &mut b), ("c", &mut c)],
    ) {
        panic!("UnpackArgs failed: {}", err);
    }
}

/// Go's AsInt: converts a Starlark int to a Rust integer of type T,
/// reporting an error if it is out of T's range.
fn as_int<T: std::fmt::Display + TryFrom<i64> + TryFrom<u64>>(
    x: &Value,
    signed: bool,
) -> Result<T, Error> {
    let bits = (std::mem::size_of::<T>() * 8) as u32;
    let v = if signed {
        T::try_from(as_signed(x, bits)?).ok()
    } else {
        T::try_from(as_unsigned(x, bits)?).ok()
    };
    Ok(v.expect("value in range"))
}

#[test]
fn test_as_int() {
    let tests: [(Value, &str, &str); 8] = [
        (Value::int(42), "i32", "42"),
        (Value::int(-1), "i32", "-1"),
        (
            Value::Int(Int::from_i64(1).lsh(40)),
            "i32",
            "1099511627776 out of range (want value in signed 32-bit range)",
        ),
        (
            Value::Int(Int::from_i64(-1).lsh(40)),
            "i32",
            "-1099511627776 out of range (want value in signed 32-bit range)",
        ),
        (Value::int(42), "u16", "42"),
        (Value::int(0xffff), "u16", "65535"),
        (
            Value::int(0x10000),
            "u16",
            "65536 out of range (want value in unsigned 16-bit range)",
        ),
        (
            Value::int(-1),
            "u16",
            "-1 out of range (want value in unsigned 16-bit range)",
        ),
    ];
    let mut failures = Vec::new();
    for (val, ty, want) in tests {
        let got = if ty == "i32" {
            as_int::<i32>(&val, true).map(|x| x.to_string())
        } else {
            as_int::<u16>(&val, false).map(|x| x.to_string())
        };
        let got = got.unwrap_or_else(|err| err.to_string());
        if got != want {
            failures.push(format!(
                "AsInt({}, {}): got {:?}, want {:?}",
                val, ty, got, want
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn test_docstring() {
    let globals = exec_file(
        &mut Thread::new(""),
        "doc.star",
        Some(b"\ndef somefunc():\n\t\"somefunc doc\"\n\treturn 0\n"),
        &StringDict::new(),
    )
    .unwrap_or_default();

    match globals.get("somefunc") {
        Some(Value::Function(f)) if f.doc() == "somefunc doc" => {}
        _ => panic!("docstring not found"),
    }
}

/// Reports whether two StringDicts have the same keys and identical or
/// equal values (Go's reflect.DeepEqual of two maps).
fn same_dict(x: &StringDict, y: &StringDict) -> bool {
    x.len() == y.len()
        && x.iter().all(|(k, v)| {
            y.get(k)
                .is_some_and(|w| v.is_identical(w) || equal(v, w).unwrap_or(false))
        })
}

#[test]
fn test_function_globals() {
    let mut predeclared = StringDict::new();
    predeclared.insert("predecl", Value::string("foo"));
    let globals = exec_file(
        &mut Thread::new(""),
        "func.star",
        Some(b"\ndef somefunc():\n\treturn predecl\n"),
        &predeclared,
    )
    .unwrap_or_default();

    let Some(Value::Function(f)) = globals.get("somefunc") else {
        panic!("somefunc is not a function");
    };
    let mut fn_globals = f.globals();
    assert!(
        same_dict(&fn_globals, &globals),
        "somefunc.Globals(): got {}, want {}",
        fn_globals,
        globals
    );
    fn_globals.insert("shouldnt_appear", Value::string("something_new"));
    assert!(
        !globals.has("shouldnt_appear"),
        "somefunc.Globals(): Globals() reflected onto ExecFile's returned globals"
    );
}

#[test]
fn test_function_module() {
    let mut predeclared = StringDict::new();
    predeclared.insert("predecl", Value::string("foo"));
    let globals = exec_file(
        &mut Thread::new(""),
        "func.star",
        Some(b"\ndef somefunc1():\n\treturn predecl\n\ndef somefunc2():\n\treturn 42\n"),
        &predeclared,
    )
    .unwrap_or_default();

    let (Some(Value::Function(fn1)), Some(Value::Function(fn2))) =
        (globals.get("somefunc1"), globals.get("somefunc2"))
    else {
        panic!("somefunc1 and somefunc2 are not functions");
    };
    assert!(
        Rc::ptr_eq(fn1.module(), fn2.module()),
        "somefunc1 and somefunc2 have different modules: {:p} vs. {:p}",
        fn1.module(),
        fn2.module()
    );

    let module = fn1.module();

    let mut module_globals = module.globals();
    assert!(
        same_dict(&module_globals, &globals),
        "module.Globals(): got {}, want {}",
        module_globals,
        globals
    );
    module_globals.insert("shouldnt_appear", Value::string("something_new"));
    assert!(
        !globals.has("shouldnt_appear"),
        "module.Globals(): Globals() reflected onto ExecFile's returned globals"
    );

    // The Rust Module holds its own copy of the predeclared dictionary
    // (exec_file borrows it), so compare contents rather than identity.
    let module_predeclared = module.predeclared();
    assert!(
        same_dict(module_predeclared, &predeclared),
        "module.Predeclared(): got {}, want {}",
        module_predeclared,
        predeclared
    );
}

#[test]
fn test_frame_locals() {
    // trace prints a nice stack trace including argument
    // values of calls to Starlark functions.
    fn trace(thread: &Thread) -> String {
        let mut buf = String::new();
        for i in 0..thread.call_stack_depth() {
            let fr = thread.debug_frame(i);
            buf.push_str(&format!("{}(", fr.callable().callable_name()));
            if let Value::Function(f) = fr.callable() {
                for i in 0..f.num_params() {
                    if i > 0 {
                        buf.push_str(", ");
                    }
                    let (name, _) = f.param(i);
                    let (_, v) = fr.local(i);
                    match v {
                        Some(v) => buf.push_str(&format!("{}={}", name, v)),
                        None => buf.push_str(&format!("{}=%!s(<nil>)", name)),
                    }
                }
            } else {
                buf.push_str("..."); // a built-in function
            }
            buf.push_str(")\n");
        }
        buf
    }

    let got = Rc::new(RefCell::new(String::new()));
    let out = got.clone();
    let mut predeclared = StringDict::new();
    predeclared.insert(
        "builtin",
        Builtin::value("builtin", move |thread, _, _, _| {
            *out.borrow_mut() = trace(thread);
            Ok(Value::None)
        }),
    );
    if let Err(err) = exec_file(
        &mut Thread::new(""),
        "foo.star",
        Some(b"\ndef f(x, y): builtin()\ndef g(z): f(z, z*z)\ng(7)\n"),
        &predeclared,
    ) {
        panic!("ExecFile failed: {}", err);
    }

    let want = "builtin(...)\nf(x=7, y=49)\ng(z=7)\n<toplevel>()\n";
    assert_eq!(
        *got.borrow(),
        want,
        "got <<{}>>, want <<{}>>",
        got.borrow(),
        want
    );
}

// TestUnpackErrorBadType is not ported: it tests how Go's reflection-based
// UnpackArgs names a parameter type whose Type method panics; Rust
// parameters are statically typed Unpackers with no such fallback.

/// Regression test for github.com/google/starlark-go/issues/233.
#[test]
fn test_repl_chunk() {
    let mut thread = Thread::new("");
    let mut globals = StringDict::new();
    let mut exec = |src: &str, globals: &mut StringDict| {
        let mut f = syntax::parse(&legacy_file_options(), "<repl>", src.as_bytes())
            .unwrap_or_else(|err| panic!("{}", err));
        if let Err(err) = exec_repl_chunk(&mut f, &mut thread, globals) {
            panic!("{}", err);
        }
    };
    let show = |globals: &StringDict, name: &str| match globals.get(name) {
        Some(v) => v.to_string(),
        None => "<nil>".to_string(),
    };

    exec("x = 0; y = 0", &mut globals);
    let got = format!("{} {}", show(&globals, "x"), show(&globals, "y"));
    assert_eq!(got, "0 0", "chunk1");

    exec("x += 1; y = y + 1", &mut globals);
    let got = format!("{} {}", show(&globals, "x"), show(&globals, "y"));
    assert_eq!(got, "1 1", "chunk2");
}

#[test]
fn test_cancel() {
    // A thread cancelled before it begins executes no code.
    {
        let mut thread = Thread::new("");
        thread.cancel("nope");
        let r = exec_file(
            &mut thread,
            "precancel.star",
            Some(b"x = 1//0"),
            &StringDict::new(),
        );
        assert_eq!(
            r.err().map(|e| e.to_string()).unwrap_or_default(),
            "Starlark computation cancelled: nope",
            "execution returned error, want cancellation"
        );

        // cancellation is sticky
        let r = exec_file(
            &mut thread,
            "precancel.star",
            Some(b"x = 1//0"),
            &StringDict::new(),
        );
        assert_eq!(
            r.err().map(|e| e.to_string()).unwrap_or_default(),
            "Starlark computation cancelled: nope",
            "execution returned error, want cancellation"
        );
    }
    // A thread cancelled during a built-in executes no more code.
    {
        let mut thread = Thread::new("");
        let mut predeclared = StringDict::new();
        predeclared.insert(
            "stopit",
            Builtin::value("stopit", |thread, _, args, _| {
                thread.cancel(&args[0].to_string());
                Ok(Value::None)
            }),
        );
        let r = exec_file(
            &mut thread,
            "stopit.star",
            Some(b"msg = 'nope'; stopit(msg); x = 1//0"),
            &predeclared,
        );
        assert_eq!(
            r.err().map(|e| e.to_string()).unwrap_or_default(),
            r#"Starlark computation cancelled: "nope""#,
            "execution returned error, want cancellation"
        );
    }
}

#[test]
fn test_execution_steps() {
    // A Thread records the number of computation steps.
    let mut thread = Thread::new("");
    fn count_steps(thread: &mut Thread, n: i64) -> (u64, Result<StringDict, Error>) {
        let mut predeclared = StringDict::new();
        predeclared.insert("n", Value::int(n));
        let steps0 = thread.execution_steps();
        let r = exec_file(
            thread,
            "steps.star",
            Some(b"squares = [x*x for x in range(n)]"),
            &predeclared,
        );
        (thread.execution_steps() - steps0, r)
    }
    let (steps100, r) = count_steps(&mut thread, 1000);
    if let Err(err) = r {
        panic!("execution failed: {}", err);
    }
    let (steps10000, r) = count_steps(&mut thread, 100000);
    if let Err(err) = r {
        panic!("execution failed: {}", err);
    }
    let ratio = steps10000 as f64 / steps100 as f64;
    assert!(
        (99.0..=101.0).contains(&ratio),
        "computation steps did not increase linearly: f(100)={}, f(10000)={}, ratio={}, want ~100",
        steps100,
        steps10000,
        ratio
    );

    // Exceeding the step limit causes cancellation.
    thread.set_max_execution_steps(1000);
    let (_, r) = count_steps(&mut thread, 1000);
    assert_eq!(
        r.err().map(|e| e.to_string()).unwrap_or_default(),
        "Starlark computation cancelled: too many steps",
        "execution returned error, want cancellation"
    );

    thread.steps = 0;
    thread.uncancel();
    let (_, r) = count_steps(&mut thread, 1);
    if let Err(err) = r {
        panic!("execution returned error {:?}, want nil", err.to_string());
    }
}

/// Fails if the interpreter proper sprouts new external dependencies.
/// We may expand the list of permitted dependencies, but should do so
/// deliberately, not casually. (Go runs `go list -deps`; the Rust port
/// checks the [dependencies] table of Cargo.toml.)
#[test]
fn test_deps() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("Cargo.toml");
    let mut in_deps = false;
    let mut failures = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_deps = line == "[dependencies]";
            continue;
        }
        if !in_deps || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let pkg = line.split('=').next().unwrap_or_default().trim();
        if !matches!(pkg, "num-bigint" | "num-integer" | "num-traits") {
            failures.push(format!("new interpreter dependency: {}", pkg));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Ensures that a panic from an application-defined built-in may traverse
/// the interpreter safely; see issue #411.
#[test]
fn test_panic_safety() {
    let mut predeclared = StringDict::new();
    predeclared.insert(
        "panic",
        Builtin::value("panic", |_, _, args, _| {
            std::panic::panic_any(args[0].to_string())
        }),
    );
    predeclared.insert("list", Value::list(vec![Value::int(0)]));

    // This program is executed twice, using the same Thread,
    // and panics both times, with values 1 and 2, while
    // main is on the stack and a for-loop is active.
    //
    // It mutates list, a predeclared variable.
    // This operation would fail if the previous
    // for-loop failed to close its iterator during the panic.
    //
    // It also calls main a second time without recursion enabled.
    // This operation would fail if the previous
    // call failed to pop main from the stack during the panic.
    let src = "
list[0] += 1

def main():
    for x in list:
        panic(x)

main()
";
    let mut thread = Thread::new("");
    for i in [1, 2] {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            exec_file(
                &mut thread,
                "panic.star",
                Some(src.as_bytes()),
                &predeclared,
            )
        }));
        match r {
            Err(payload) => {
                let got = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_default();
                assert_eq!(got, i.to_string(), "recover");
            }
            Ok(Err(err)) => panic!(
                "ExecFile returned error {:?}, expected panic",
                err.to_string()
            ),
            Ok(Ok(v)) => panic!("ExecFile returned {}, expected panic", v),
        }
    }
}

#[test]
fn test_debug_frame() {
    let mut predeclared = StringDict::new();
    predeclared.insert(
        "env",
        Builtin::value("env", |thread, _, _, _| {
            if thread.call_stack_depth() < 2 {
                return Err(Error::new("env must not be called directly"));
            }
            let fr = thread.debug_frame(1); // parent
            let Value::Function(f) = fr.callable() else {
                return Err(Error::new("env must be called from a Starlark function"));
            };
            let dict = Dict::new();
            for i in 0..fr.num_locals() {
                let (bind, val) = fr.local(i);
                let Some(val) = val else {
                    continue;
                };
                let _ = dict.set_key(Value::string(&bind.name), val.clone()); // ignore error
            }
            for i in 0..f.num_free_vars() {
                let (bind, val) = f.free_var(i);
                if let Some(val) = val {
                    let _ = dict.set_key(Value::string(&bind.name), val); // ignore error
                }
            }
            let dict = Value::Dict(Rc::new(dict));
            dict.freeze();
            Ok(dict)
        }),
    );
    let src = "
e = [None]

def f(p):
    outer = 3
    def g(q):
        inner = outer + 1
        e[0] = env() # {\"q\": 2, \"inner\": 4, \"outer\": 3}
\tinner2 = None # not defined at call to env()
    g(2)

f(1)
";
    let mut thread = Thread::new("");
    let m = exec_file(&mut thread, "env.star", Some(src.as_bytes()), &predeclared).unwrap_or_else(
        |err| {
            panic!(
                "ExecFile returned error {:?}, expected panic",
                err.to_string()
            )
        },
    );
    let Some(Value::List(e)) = m.get("e") else {
        panic!("e is not a list");
    };
    let got = e.index(0).to_string();
    let want = r#"{"q": 2, "inner": 4, "outer": 3}"#;
    assert_eq!(got, want, "env() returned {}, want {}", got, want);
}

#[test]
fn test_unpack_args_optional_inference() {
    // success
    let kwargs = [
        (Value::string("x"), Value::int(1)),
        (Value::string("y"), Value::int(2)),
    ];
    let (mut x, mut y, mut z) = (0i64, 0i64, 0i64);
    if let Err(err) = unpack_args(
        "unpack",
        &[],
        &kwargs,
        &mut [("x", &mut x), ("y?", &mut y), ("z", &mut z)],
    ) {
        panic!("UnpackArgs failed: {}", err);
    }
    assert_eq!(format!("x={} y={} z={}", x, y, z), "x=1 y=2 z=0");

    // success
    let args = [Value::int(1), Value::int(2)];
    x = 0;
    y = 0;
    z = 0;
    if let Err(err) = unpack_args(
        "unpack",
        &args,
        &[],
        &mut [("x", &mut x), ("y?", &mut y), ("z", &mut z)],
    ) {
        panic!("UnpackArgs failed: {}", err);
    }
    assert_eq!(format!("x={} y={} z={}", x, y, z), "x=1 y=2 z=0");
}

// TestUnpackArgNoEscape is not ported: it measures heap allocations of
// Go-internal NoEscape variants of UnpackArgs, which have no Rust analogue.
