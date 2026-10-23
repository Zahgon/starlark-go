// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Tests of the Value API (ported from starlark/value_test.go).

use std::any::Any;
use std::rc::Rc;

use starlark::starlark::{
    exec_file, iterate, Dict, Error, List, StarlarkValue, StringDict, Thread, Value, ValueIterator,
};

#[test]
fn test_string_method() {
    let s = Value::string("hello");
    let tests: [(String, &str); 6] = [
        // quoted string:
        (s.repr(), r#""hello""#),
        (format!("{}", s), r#""hello""#),
        (format!("{:+}", s), r#""hello""#),
        (s.to_string(), r#""hello""#),
        (format!("{:?}", s), r#""hello""#),
        // unquoted:
        (s.to_display_string(), "hello"),
    ];
    // (Go's %#v verb, which calls GoString, has no Rust counterpart:
    // Rust's Debug formatting of a Value is its quoted String form.)
    for (i, (got, want)) in tests.iter().enumerate() {
        assert_eq!(got, want, "#{}: got <<{}>>, want <<{}>>", i, got, want);
    }
}

#[test]
fn test_list_append() {
    let l = List::new(Vec::new());
    l.append(Value::string("hello")).unwrap();
    let Some(res) = l.index(0).as_str().map(|s| s.to_vec()) else {
        panic!(
            "failed list.Append() got: {}, want: starlark.String",
            l.index(0).type_name()
        );
    };
    assert_eq!(res, b"hello", "failed list.Append()");
}

#[test]
fn test_param_default() {
    let s = |x: &str| Some(Value::string(x));
    let tests: Vec<(&str, &str, Vec<Option<Value>>)> = vec![
        (
            "function with all required params",
            "all_required",
            vec![None, None, None],
        ),
        (
            "function with all optional params",
            "all_opt",
            vec![s("a"), Some(Value::None), s("")],
        ),
        (
            "function with required and optional params",
            "mix_required_opt",
            vec![None, None, s("c"), s("d")],
        ),
        (
            "function with required, optional, and varargs params",
            "with_varargs",
            vec![None, s("b"), None],
        ),
        (
            "function with required, optional, varargs, and keyword-only params",
            "with_varargs_kwonly",
            vec![None, s("b"), s("c"), None, None],
        ),
        (
            "function with required, optional, and keyword-only params",
            "with_kwonly",
            vec![None, s("b"), s("c"), None],
        ),
        (
            "function with required, optional, and kwargs params",
            "with_kwargs",
            vec![None, s("b"), s("c"), None],
        ),
        (
            "function with required, optional, varargs, kw-only, and kwargs params",
            "with_varargs_kwonly_kwargs",
            vec![None, s("b"), s("c"), None, s("e"), None, None],
        ),
    ];

    // Values are compared by type and String form.
    let show = |v: &[Option<Value>]| -> Vec<Option<String>> {
        v.iter()
            .map(|x| x.as_ref().map(|x| format!("{}:{}", x.type_name(), x)))
            .collect()
    };

    for (desc, star_fn, want_defaults) in tests {
        let mut thread = Thread::new("");
        let filename = "starlark/testdata/function_param.star";
        let globals = exec_file(&mut thread, filename, None, &StringDict::new())
            .unwrap_or_else(|err| panic!("{}: ExecFile({:?}) failed: {}", desc, filename, err));

        let Some(Value::Function(f)) = globals.get(star_fn) else {
            panic!(
                "{}: value {:?} is not a Starlark function",
                desc,
                globals.get(star_fn)
            );
        };

        let param_defaults: Vec<Option<Value>> =
            (0..f.num_params()).map(|i| f.param_default(i)).collect();
        assert_eq!(
            show(&param_defaults),
            show(&want_defaults),
            "{}: param defaults",
            desc
        );
    }
}

/// A fakeMapping is an iterable mapping that delegates to a Dict, so that
/// the generic iteration functions (not the Dict fast paths) are used.
struct FakeMapping {
    dict: Rc<Dict>,
}

impl StarlarkValue for FakeMapping {
    fn type_name(&self) -> &str {
        "fakeMapping"
    }
    fn repr(&self, out: &mut String) {
        out.push_str(&Value::Dict(self.dict.clone()).repr());
    }
    fn hash(&self) -> Result<u32, Error> {
        Err(Error::new("unhashable type: fakeMapping"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn iterate(&self) -> Option<Box<dyn ValueIterator>> {
        iterate(&Value::Dict(self.dict.clone()))
    }
    fn get(&self, k: &Value) -> Option<Result<Option<Value>, Error>> {
        Some(self.dict.get(k))
    }
    fn items(&self) -> Option<Vec<(Value, Value)>> {
        Some(self.dict.items())
    }
}

// In Rust, obtaining an iterator starts an iteration, which ends when the
// iterator is dropped (Go's Done); Go's lazy Elements/Entries sequences
// start none. These tests check that obtaining and discarding the
// sequence leaves no iteration in progress.

#[test]
fn test_elements_iterator_count() {
    let d = Rc::new(Dict::new());
    let m = Value::other(FakeMapping { dict: d.clone() });

    // Asking for the sequence must not leave an iteration in progress.
    drop(iterate(&m));
    if d.set_key(Value::string("one"), Value::int(1)).is_err() {
        panic!("Elements(m) started an iteration");
    }
}

#[test]
fn test_entries_iterator_count() {
    let d = Rc::new(Dict::new());
    let m = FakeMapping { dict: d.clone() };

    // Asking for the sequence must not leave an iteration in progress.
    let _ = m.items();
    if d.set_key(Value::string("one"), Value::int(1)).is_err() {
        panic!("Entries(m) started an iteration");
    }
}
