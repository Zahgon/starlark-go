// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! The examples of starlark/example_test.go, as tests that check their
//! documented output.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use starlark::starlark::{exec_file, unpack_args, Builtin, Error, StringDict, Thread, Value};

/// Demonstrates a simple embedding of the Starlark interpreter into a
/// Rust program.
#[test]
fn example_exec_file() {
    let data = r#"
print(greeting + ", world")
print(repeat("one"))
print(repeat("mur", 2))
squares = [x*x for x in range(10)]
"#;
    let out = Rc::new(RefCell::new(String::new()));

    // repeat(str, n=1) is a Rust function called from Starlark.
    // It behaves like the 'string * int' operation.
    let repeat = |_: &mut Thread, b: &Builtin, args: &[Value], kwargs: &[(Value, Value)]| {
        let mut s: Vec<u8> = Vec::new();
        let mut n: i64 = 1;
        unpack_args(b.name(), args, kwargs, &mut [("s", &mut s), ("n?", &mut n)])?;
        Ok(Value::string(s.repeat(n.max(0) as usize)))
    };

    // The Thread defines the behavior of the built-in 'print' function.
    let mut thread = Thread::new("example");
    let o = out.clone();
    thread.print = Some(Rc::new(move |_: &Thread, msg: &[u8]| {
        let mut o = o.borrow_mut();
        o.push_str(&String::from_utf8_lossy(msg));
        o.push('\n');
    }));

    // This dictionary defines the pre-declared environment.
    let mut predeclared = StringDict::new();
    predeclared.insert("greeting", Value::string("hello"));
    predeclared.insert("repeat", Builtin::value("repeat", repeat));

    // Execute a program.
    let globals = match exec_file(
        &mut thread,
        "apparent/filename.star",
        Some(data.as_bytes()),
        &predeclared,
    ) {
        Ok(globals) => globals,
        Err(Error::Eval(err)) => panic!("{}", err.backtrace()),
        Err(err) => panic!("{}", err),
    };

    // Print the global environment.
    let mut o = out.borrow_mut();
    o.push_str("\nGlobals:\n");
    for name in globals.keys() {
        let v = &globals.get(&name).unwrap();
        o.push_str(&format!("{} ({}) = {}\n", name, v.type_name(), v));
    }

    assert_eq!(
        *o,
        "hello, world
one
murmur

Globals:
squares (list) = [0, 1, 4, 9, 16, 25, 36, 49, 64, 81]
"
    );
}

/// Demonstrates a simple caching implementation of 'load' that works
/// sequentially.
#[test]
fn example_thread_load_sequential() {
    let fake_filesystem: HashMap<&str, &str> = HashMap::from([
        ("c.star", r#"load("b.star", "b"); c = b + "!""#),
        ("b.star", r#"load("a.star", "a"); b = a + ", world""#),
        ("a.star", r#"a = "Hello""#),
    ]);

    // A None entry indicates "load in progress".
    type Cache = HashMap<String, Option<Result<StringDict, Error>>>;

    fn load(
        cache: &Rc<RefCell<Cache>>,
        fs: &Rc<HashMap<&'static str, &'static str>>,
        module: &str,
    ) -> Result<StringDict, Error> {
        let e = cache.borrow().get(module).cloned();
        match e {
            Some(Some(e)) => e,
            Some(None) => {
                // request for package whose loading is in progress
                Err(Error::new("cycle in load graph"))
            }
            None => {
                // Add a placeholder to indicate "load in progress".
                cache.borrow_mut().insert(module.to_string(), None);

                // Load and initialize the module in a new thread.
                let data = fs.get(module).copied().unwrap_or_default();
                let mut thread = Thread::new(&format!("exec {}", module));
                let (c, f) = (cache.clone(), fs.clone());
                thread.load = Some(Rc::new(move |_: &mut Thread, module: &str| {
                    load(&c, &f, module)
                }));
                let e = exec_file(
                    &mut thread,
                    module,
                    Some(data.as_bytes()),
                    &StringDict::new(),
                );

                // Update the cache.
                cache
                    .borrow_mut()
                    .insert(module.to_string(), Some(e.clone()));
                e
            }
        }
    }

    let cache = Rc::new(RefCell::new(Cache::new()));
    let globals =
        load(&cache, &Rc::new(fake_filesystem), "c.star").unwrap_or_else(|err| panic!("{}", err));
    let out = format!("{}\n", globals.get("c").unwrap());

    assert_eq!(out, "\"Hello, world!\"\n");
}

// ExampleThread_Load_parallel and TestThreadLoad_ParallelCycle are not
// ported: they load modules concurrently from several goroutines that
// share the resulting module values, but Rust Starlark values are
// reference-counted with Rc and cannot be shared between OS threads.
