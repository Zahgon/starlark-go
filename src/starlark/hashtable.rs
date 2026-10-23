// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! The hash table underlying Starlark dict and set values (ported from
//! hashtable.go).
//!
//! Its key/value entries form a doubly-linked list in the order the
//! entries were inserted. Entries live in a slab and are found through an
//! index from hash value to slab slots, which takes the place of the Go
//! original's bucket array; the observable behaviour (insertion order,
//! error messages, the frozen and iteration checks) is the same.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use super::value::{equal, Error, Value};

#[derive(Clone)]
struct Entry {
    hash: u32,
    key: Value,
    value: Value,
    prev: Option<usize>,
    next: Option<usize>,
}

#[derive(Default)]
struct Inner {
    slots: Vec<Option<Entry>>,
    free: Vec<usize>,
    index: HashMap<u32, Vec<usize>>,
    head: Option<usize>,
    tail: Option<usize>,
    len: usize,
}

/// A hash table whose entries are kept in insertion order.
#[derive(Default)]
pub struct HashTable {
    inner: RefCell<Inner>,
    frozen: Cell<bool>,
    itercount: Cell<u32>,
    // Whether the Go original's bucket table would have been allocated
    // (by the first insertion or an explicit init); this affects whether
    // delete and count hash their argument, and thus report unhashable
    // keys.
    initialized: Cell<bool>,
}

fn key_hash(k: &Value) -> Result<u32, Error> {
    let h = k.hash()?;
    // zero is reserved
    Ok(if h == 0 { 1 } else { h })
}

impl HashTable {
    /// Returns a new, empty table.
    pub fn new() -> HashTable {
        HashTable::default()
    }

    /// Allocates the table (Go's hashtable.init), as done by NewDict,
    /// NewSet and dict union.
    pub fn init(&self) {
        self.initialized.set(true);
    }

    /// Reports whether the table is frozen.
    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    /// Marks the table and, transitively, its keys and values as frozen.
    pub fn freeze(&self) {
        if !self.frozen.get() {
            self.frozen.set(true);
            for (k, v) in self.items() {
                k.freeze();
                v.freeze();
            }
        }
    }

    /// Returns the number of entries.
    pub fn len(&self) -> usize {
        self.inner.borrow().len
    }

    /// Reports whether the table has no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the slots of entries whose hash is `h`.
    fn candidates(&self, h: u32) -> Vec<(usize, Value)> {
        let inner = self.inner.borrow();
        match inner.index.get(&h) {
            Some(slots) => slots
                .iter()
                .map(|&i| {
                    (
                        i,
                        inner.slots[i]
                            .as_ref()
                            .map(|e| e.key.clone())
                            .unwrap_or(Value::None),
                    )
                })
                .collect(),
            None => Vec::new(),
        }
    }

    /// Finds the slot of key `k` with hash `h`.
    fn find(&self, k: &Value, h: u32) -> Result<Option<usize>, Error> {
        for (slot, key) in self.candidates(h) {
            // e.g. excessively recursive tuple
            if equal(k, &key)? {
                return Ok(Some(slot));
            }
        }
        Ok(None)
    }

    /// Inserts or updates the entry for key `k`.
    pub fn insert(&self, k: Value, v: Value) -> Result<(), Error> {
        self.check_mutable("insert into")?;
        self.initialized.set(true);
        let h = key_hash(&k)?;
        if let Some(slot) = self.find(&k, h)? {
            // Key already present; update value.
            if let Some(e) = self.inner.borrow_mut().slots[slot].as_mut() {
                e.value = v;
            }
            return Ok(());
        }
        let mut inner = self.inner.borrow_mut();
        let entry = Entry {
            hash: h,
            key: k,
            value: v,
            prev: inner.tail,
            next: None,
        };
        let slot = match inner.free.pop() {
            Some(s) => {
                inner.slots[s] = Some(entry);
                s
            }
            None => {
                inner.slots.push(Some(entry));
                inner.slots.len() - 1
            }
        };
        // Append entry to doubly-linked list.
        match inner.tail {
            Some(t) => {
                if let Some(e) = inner.slots[t].as_mut() {
                    e.next = Some(slot);
                }
            }
            None => inner.head = Some(slot),
        }
        inner.tail = Some(slot);
        inner.index.entry(h).or_default().push(slot);
        inner.len += 1;
        Ok(())
    }

    /// Looks up the value for key `k`.
    pub fn lookup(&self, k: &Value) -> Result<Option<Value>, Error> {
        let h = key_hash(k)?; // unhashable
        if !self.initialized.get() {
            return Ok(None); // empty
        }
        match self.find(k, h)? {
            Some(slot) => Ok(self.inner.borrow().slots[slot]
                .as_ref()
                .map(|e| e.value.clone())),
            None => Ok(None),
        }
    }

    /// Returns the number of distinct elements of iter that are elements
    /// of the table.
    pub fn count(&self, iter: &mut dyn super::value::ValueIterator) -> Result<usize, Error> {
        if !self.initialized.get() {
            return Ok(0); // empty
        }
        let mut seen = std::collections::HashSet::new();
        let len = self.len();
        while seen.len() != len {
            let Some(k) = iter.next() else { break };
            let h = key_hash(&k)?; // unhashable
            for (slot, key) in self.candidates(h) {
                if equal(&k, &key)? {
                    seen.insert(slot);
                }
            }
        }
        Ok(seen.len())
    }

    /// Returns all the items in the map (as key/value pairs) in insertion order.
    pub fn items(&self) -> Vec<(Value, Value)> {
        let inner = self.inner.borrow();
        let mut items = Vec::with_capacity(inner.len);
        let mut cur = inner.head;
        while let Some(i) = cur {
            let e = inner.slots[i].as_ref().expect("live entry");
            items.push((e.key.clone(), e.value.clone()));
            cur = e.next;
        }
        items
    }

    /// Returns the keys in insertion order.
    pub fn keys(&self) -> Vec<Value> {
        self.items().into_iter().map(|(k, _)| k).collect()
    }

    /// Returns the first key, if any.
    pub fn first(&self) -> Option<Value> {
        let inner = self.inner.borrow();
        inner
            .head
            .and_then(|i| inner.slots[i].as_ref().map(|e| e.key.clone()))
    }

    /// Removes the entry for key `k`, returning its value if present.
    pub fn delete(&self, k: &Value) -> Result<Option<Value>, Error> {
        self.check_mutable("delete from")?;
        if !self.initialized.get() {
            return Ok(None); // empty
        }
        let h = key_hash(k)?; // unhashable
        let Some(slot) = self.find(k, h)? else {
            return Ok(None);
        };
        let mut inner = self.inner.borrow_mut();
        let e = inner.slots[slot].take().expect("live entry");
        // Remove e from doubly-linked list.
        match e.prev {
            Some(p) => {
                if let Some(pe) = inner.slots[p].as_mut() {
                    pe.next = e.next;
                }
            }
            None => inner.head = e.next,
        }
        match e.next {
            Some(n) => {
                if let Some(ne) = inner.slots[n].as_mut() {
                    ne.prev = e.prev;
                }
            }
            None => inner.tail = e.prev,
        }
        if let Some(slots) = inner.index.get_mut(&e.hash) {
            slots.retain(|&s| s != slot);
            if slots.is_empty() {
                inner.index.remove(&e.hash);
            }
        }
        inner.free.push(slot);
        inner.len -= 1;
        Ok(Some(e.value))
    }

    /// Reports an error if the hash table should not be mutated.
    /// verb+" hash table" should describe the operation.
    pub fn check_mutable(&self, verb: &str) -> Result<(), Error> {
        if self.frozen.get() {
            return Err(Error::new(format!("cannot {} frozen hash table", verb)));
        }
        if self.itercount.get() > 0 {
            return Err(Error::new(format!(
                "cannot {} hash table during iteration",
                verb
            )));
        }
        Ok(())
    }

    /// Removes all entries.
    pub fn clear(&self) -> Result<(), Error> {
        self.check_mutable("clear")?;
        *self.inner.borrow_mut() = Inner::default();
        Ok(())
    }

    /// Inserts all entries of `other`.
    pub fn add_all(&self, other: &HashTable) -> Result<(), Error> {
        for (k, v) in other.items() {
            self.insert(k, v)?;
        }
        Ok(())
    }

    /// Begins an iteration, returning the cursor of the first entry.
    pub fn iter_start(&self) -> Option<usize> {
        if !self.frozen.get() {
            self.itercount.set(self.itercount.get() + 1);
        }
        self.inner.borrow().head
    }

    /// Returns the key at `cursor` and advances the cursor.
    pub fn iter_next(&self, cursor: &mut Option<usize>) -> Option<Value> {
        let inner = self.inner.borrow();
        let i = (*cursor)?;
        let e = inner.slots.get(i)?.as_ref()?;
        *cursor = e.next;
        Some(e.key.clone())
    }

    /// Ends an iteration begun by `iter_start`.
    pub fn iter_done(&self) {
        if !self.frozen.get() {
            self.itercount.set(self.itercount.get().saturating_sub(1));
        }
    }
}

/// Computes the 32-bit FNV-1a hash of s.
///
/// The Go original uses the runtime's seeded hash for strings of 12 bytes
/// or more; since that hash is randomized per process it is not
/// observable, and FNV-1a is used for all strings here.
pub fn hash_string(s: &[u8]) -> u32 {
    soft_hash_string(s)
}

/// Computes the 32-bit FNV-1a hash of s in software.
pub fn soft_hash_string(s: &[u8]) -> u32 {
    let mut h: u32 = 2166136261;
    for &b in s {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}
