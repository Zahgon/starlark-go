// Copyright 2018 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Tests of the struct extension (ported from starlarkstruct/struct_test.go).

use std::any::Any;

use starlark::starlark::{
    equal, exec_file, get_attr, unpack_args, Builtin, Error, StarlarkValue, StringDict, Thread,
    Value,
};
use starlark::starlarkstruct::{self, Struct};

#[path = "common/starlarktest.rs"]
mod starlarktest;

#[test]
fn test() {
    let mut thread = Thread::new("");
    thread.load = Some(std::rc::Rc::new(load));
    let reports = starlarktest::set_reporter(&mut thread);
    let filename = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/starlarkstruct/testdata/struct.star"
    );
    let mut predeclared = StringDict::new();
    predeclared.insert("struct", Builtin::value("struct", starlarkstruct::make));
    predeclared.insert("gensym", Builtin::value("gensym", gensym));
    match exec_file(&mut thread, filename, None, &predeclared) {
        Ok(_) => {}
        Err(Error::Eval(err)) => panic!("{}", err.backtrace()),
        Err(err) => panic!("{}", err),
    }
    let reports = reports.borrow();
    assert!(reports.is_empty(), "{}", reports.join("\n"));
}

/// Implements the 'load' operation as used in the evaluator tests.
fn load(_: &mut Thread, module: &str) -> Result<StringDict, Error> {
    if module == "assert.star" {
        return starlarktest::load_assert_module();
    }
    Err(Error::new("load not implemented"))
}

/// A built-in function that generates a unique symbol.
fn gensym(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut name = String::new();
    unpack_args("gensym", args, kwargs, &mut [("name", &mut name)])?;
    Ok(Value::other(Symbol { name }))
}

/// A symbol is a distinct value that acts as a constructor of "branded"
/// struct instances, like a class symbol in Python or a "provider" in Bazel.
struct Symbol {
    name: String,
}

impl StarlarkValue for Symbol {
    fn type_name(&self) -> &str {
        "symbol"
    }
    fn repr(&self, out: &mut String) {
        out.push_str(&self.name);
    }
    fn hash(&self) -> Result<u32, Error> {
        Err(Error::new(format!("unhashable: {}", self.type_name())))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn is_callable(&self) -> bool {
        true
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn call(
        &self,
        thread: &mut Thread,
        args: &[Value],
        kwargs: &[(Value, Value)],
    ) -> Result<Value, Error> {
        if !args.is_empty() {
            return Err(Error::new(format!(
                "{}: unexpected positional arguments",
                self.name
            )));
        }
        // The constructor is the symbol itself: the value being called.
        let sym = thread.debug_frame(0).callable().clone();
        Ok(Value::other(Struct::from_keywords(sym, kwargs)))
    }
}

fn abc_struct() -> Value {
    let mut d = StringDict::new();
    d.insert("b", Value::int(2));
    d.insert("a", Value::int(1));
    d.insert("c", Value::int(3));
    Value::other(Struct::from_string_dict(
        starlarkstruct::default_constructor(),
        &d,
    ))
}

#[test]
fn test_attr_at() {
    let v = abc_struct();
    let s = v.downcast_ref::<Struct>().unwrap();

    // Positions are those of AttrNames: sorted by field name.
    let names = s.attr_names();
    assert_eq!(names.join(","), "a,b,c", "AttrNames()");
    assert_eq!(s.len(), names.len(), "Len()");
    for (pos, name) in names.iter().enumerate() {
        let (got_name, got_value) = s.attr_at(pos);
        assert_eq!(got_name, name, "AttrAt({})", pos);
        let want =
            get_attr(&v, name).unwrap_or_else(|err| panic!("Attr({}) failed: {}", name, err));
        match equal(got_value, &want) {
            Err(err) => panic!("comparing AttrAt({}) and Attr({}): {}", pos, name, err),
            Ok(eq) => assert!(
                eq,
                "AttrAt({}): want {} (= Attr({})), got {}",
                pos, want, name, got_value
            ),
        }
    }
}

#[test]
fn test_entries() {
    let v = abc_struct();
    let s = v.downcast_ref::<Struct>().unwrap();

    // sequence matches Attr and AttrNames
    {
        let mut names = Vec::new();
        for (name, value) in s.entries() {
            names.push(name.clone());
            let want =
                get_attr(&v, name).unwrap_or_else(|err| panic!("Attr({}) failed: {}", name, err));
            match equal(value, &want) {
                Err(err) => panic!(
                    "comparing Entries value of {} and Attr({}): {}",
                    name, name, err
                ),
                Ok(eq) => assert!(
                    eq,
                    "Entries: {}: want {} (= Attr({})), got {}",
                    name, want, name, value
                ),
            }
        }
        assert_eq!(names.join(","), s.attr_names().join(","), "Entries names");
    }
    // reusable iterator
    {
        let seq = s.entries();
        for _ in 0..2 {
            let mut n = 0;
            for _ in seq {
                n += 1;
            }
            assert_eq!(n, s.len(), "Entries: want {} entries, got {}", s.len(), n);
        }
    }
    // an empty struct yields nothing
    {
        let empty =
            Struct::from_string_dict(starlarkstruct::default_constructor(), &StringDict::new());
        if let Some((name, value)) = empty.entries().first() {
            panic!("Entries of empty struct yielded {} = {}", name, value);
        }
    }
}
