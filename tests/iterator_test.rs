// Copyright 2024 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Tests of iteration over the elements of Tuple, List and Set and the
//! entries of Dict (ported from starlark/iterator_test.go). The type-specific
//! accessors (Go's x.Elements and Entries methods) are compared with the
//! generic iteration function (Go's Elements and Entries functions).

use std::rc::Rc;

use starlark::starlark::{iterate, Dict, List, Set, Value};

/// Appends the first elements of the generic iteration over x to got,
/// stopping (and dropping the iterator) once got has n elements.
fn take_elements(x: &Value, got: &mut Vec<String>, n: usize) {
    let mut it = iterate(x).expect("iterable");
    while let Some(elem) = it.next() {
        got.push(elem.to_string());
        if got.len() == n {
            break; // skip 3
        }
    }
}

#[test]
fn test_tuple_elements() {
    let tuple: Rc<[Value]> = Rc::from(vec![Value::int(1), Value::int(2), Value::int(3)]);

    let mut got = Vec::new();
    for elem in tuple.iter() {
        got.push(elem.to_string());
        if got.len() == 2 {
            break; // skip 3
        }
    }
    take_elements(&Value::Tuple(tuple), &mut got, 4);
    assert_eq!(got, ["1", "2", "1", "2"]);
}

#[test]
fn test_list_elements() {
    let list = Rc::new(List::new(vec![Value::int(1), Value::int(2), Value::int(3)]));

    let mut got = Vec::new();
    for elem in list.elems().iter() {
        got.push(elem.to_string());
        if got.len() == 2 {
            break; // skip 3
        }
    }
    take_elements(&Value::List(list), &mut got, 4);
    assert_eq!(got, ["1", "2", "1", "2"]);
}

#[test]
fn test_set_elements() {
    let set = Rc::new(Set::new());
    set.insert(Value::int(1)).unwrap();
    set.insert(Value::int(2)).unwrap();
    set.insert(Value::int(3)).unwrap();

    let mut got = Vec::new();
    for elem in set.elems() {
        got.push(elem.to_string());
        if got.len() == 2 {
            break; // skip 3
        }
    }
    take_elements(&Value::Set(set), &mut got, 4);
    assert_eq!(got, ["1", "2", "1", "2"]);
}

#[test]
fn test_dict_entries() {
    let dict = Rc::new(Dict::new());
    dict.set_key(Value::string("one"), Value::int(1)).unwrap();
    dict.set_key(Value::string("two"), Value::int(2)).unwrap();
    dict.set_key(Value::string("three"), Value::int(3)).unwrap();

    let mut got = Vec::new();
    for (k, v) in dict.items() {
        got.push(format!("{} {}", k, v));
        if got.len() == 2 {
            break; // skip 3
        }
    }
    // The generic form: iterate over the keys and look up each value.
    let d = Value::Dict(dict.clone());
    let mut it = iterate(&d).expect("iterable");
    while let Some(k) = it.next() {
        let v = dict.get(&k).unwrap().unwrap();
        got.push(format!("{} {}", k, v));
        if got.len() == 4 {
            break; // skip 3
        }
    }
    assert_eq!(
        got,
        [r#""one" 1"#, r#""two" 2"#, r#""one" 1"#, r#""two" 2"#]
    );
}
