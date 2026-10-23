// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! The bytecode interpreter (ported from interp.go).

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use super::eval::nearest;
use super::eval::{
    binary, call, get_attr, get_index, list_extend, set_args, set_field, set_index, slice, unary,
    universal, Thread,
};
use super::value::{
    compare, iterate, len, Dict, Error, Function, StarlarkValue, Value, ValueIterator,
};
use crate::compile::Opcode;
use crate::syntax::Token;

/// Calls a Starlark function: binds its parameters and runs its bytecode.
pub(crate) fn call_internal(
    fnv: &Rc<Function>,
    thread: &mut Thread,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    // Postcondition: args is not mutated.
    let f = &fnv.funcode;
    let module = &fnv.module;
    let prog = &module.program;
    if prog.recursion {
        // prevent stack overflow
        if thread.stack.len() > 100_000 {
            return Err(Error::new("Starlark stack overflow"));
        }
    } else {
        // detect recursion
        let n = thread.stack.len();
        for fr in &thread.stack[..n - 1] {
            // We look for the same function code,
            // not function value, otherwise the user could
            // defeat the check by writing the Y combinator.
            if let Value::Function(frfn) = &fr.callable {
                if Rc::ptr_eq(&frfn.funcode, f) {
                    return Err(Error::new(format!(
                        "function {} called recursively",
                        fnv.name()
                    )));
                }
            }
        }
    }

    let fi = thread.stack.len() - 1;

    // Allocate space for locals.
    let mut locals: Vec<Option<Value>> = vec![None; f.locals.len()];

    // Digest arguments and set parameters.
    if let Err(err) = set_args(&mut locals, fnv, args, kwargs) {
        return Err(thread.eval_error(err));
    }

    // Check cancellation and increment steps on entry.
    // (Needed for recursive CALLs.)
    if thread.check_steps() {
        thread.check_steps_slow()?;
    }

    // Spill indicated locals to cells.
    // Each cell is a separate alloc to avoid spurious liveness.
    for &index in &f.cells {
        let v = locals[index].take();
        locals[index] = Some(Value::other(CellValue(RefCell::new(v))));
    }

    // The locals live in the frame, where the debugger API can see them.
    thread.stack[fi].locals = locals;

    let mut stack: Vec<Value> = Vec::with_capacity(f.max_stack);
    let mut iterstack: Vec<Box<dyn ValueIterator>> = Vec::new(); // stack of active iterators
    let code = &f.code;
    let mut pc: usize = 0;

    macro_rules! pop {
        () => {
            stack.pop().expect("operand stack underflow")
        };
    }
    macro_rules! fail {
        ($e:expr) => {
            break Err($e)
        };
    }
    macro_rules! tri {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => fail!(e),
            }
        };
    }

    let result: Result<Value, Error> = loop {
        thread.stack[fi].pc = pc;
        let insn = code[pc];
        pc += 1;
        let arg = insn.arg as usize;

        match insn.op {
            Opcode::Nop => {}

            Opcode::Dup => {
                let v = stack[stack.len() - 1].clone();
                stack.push(v);
            }

            Opcode::Dup2 => {
                let n = stack.len();
                let (a, b) = (stack[n - 2].clone(), stack[n - 1].clone());
                stack.push(a);
                stack.push(b);
            }

            Opcode::Pop => {
                pop!();
            }

            Opcode::Exch => {
                let n = stack.len();
                stack.swap(n - 2, n - 1);
            }

            Opcode::Eql | Opcode::Neq | Opcode::Gt | Opcode::Lt | Opcode::Le | Opcode::Ge => {
                let op = match insn.op {
                    Opcode::Eql => Token::Eql,
                    Opcode::Neq => Token::Neq,
                    Opcode::Gt => Token::Gt,
                    Opcode::Lt => Token::Lt,
                    Opcode::Le => Token::Le,
                    _ => Token::Ge,
                };
                let y = pop!();
                let x = pop!();
                let ok = tri!(compare(op, &x, &y));
                stack.push(Value::Bool(ok));
            }

            Opcode::Plus
            | Opcode::Minus
            | Opcode::Star
            | Opcode::Slash
            | Opcode::Slashslash
            | Opcode::Percent
            | Opcode::Amp
            | Opcode::Pipe
            | Opcode::Circumflex
            | Opcode::Ltlt
            | Opcode::Gtgt
            | Opcode::In => {
                let binop = match insn.op {
                    Opcode::Plus => Token::Plus,
                    Opcode::Minus => Token::Minus,
                    Opcode::Star => Token::Star,
                    Opcode::Slash => Token::Slash,
                    Opcode::Slashslash => Token::Slashslash,
                    Opcode::Percent => Token::Percent,
                    Opcode::Amp => Token::Amp,
                    Opcode::Pipe => Token::Pipe,
                    Opcode::Circumflex => Token::Circumflex,
                    Opcode::Ltlt => Token::Ltlt,
                    Opcode::Gtgt => Token::Gtgt,
                    _ => Token::In,
                };
                let y = pop!();
                let x = pop!();
                let z = tri!(binary(binop, &x, &y));
                stack.push(z);
            }

            Opcode::Uplus | Opcode::Uminus | Opcode::Tilde => {
                let unop = match insn.op {
                    Opcode::Tilde => Token::Tilde,
                    Opcode::Uplus => Token::Plus,
                    _ => Token::Minus,
                };
                let x = pop!();
                let y = tri!(unary(unop, &x));
                stack.push(y);
            }

            Opcode::InplaceAdd => {
                let y = pop!();
                let x = pop!();

                // It's possible that y is not Iterable but
                // nonetheless defines x+y, in which case we
                // should fall back to the general case.
                let mut z = None;
                if let Value::List(xlist) = &x {
                    if y.is_iterable() {
                        tri!(xlist.check_mutable("apply += to"));
                        list_extend(xlist, &y);
                        z = Some(x.clone());
                    }
                }
                let z = match z {
                    Some(z) => z,
                    None => tri!(binary(Token::Plus, &x, &y)),
                };
                stack.push(z);
            }

            Opcode::InplacePipe => {
                let y = pop!();
                let x = pop!();

                // It's possible that y is not Dict but
                // nonetheless defines x|y, in which case we
                // should fall back to the general case.
                let z = match (&x, &y) {
                    (Value::Dict(xdict), Value::Dict(ydict)) => {
                        tri!(xdict.ht.check_mutable("apply |= to"));
                        let _ = xdict.ht.add_all(&ydict.ht); // can't fail
                        x.clone()
                    }
                    _ => tri!(binary(Token::Pipe, &x, &y)),
                };
                stack.push(z);
            }

            Opcode::None => stack.push(Value::None),

            Opcode::True => stack.push(Value::Bool(true)),

            Opcode::False => stack.push(Value::Bool(false)),

            Opcode::Mandatory => stack.push(Value::other(Mandatory)),

            Opcode::Jmp => {
                // On back edge, check cancellation and step count.
                if arg < pc && thread.check_steps() {
                    tri!(thread.check_steps_slow());
                }
                pc = arg;
            }

            Opcode::Call | Opcode::CallVar | Opcode::CallKw | Opcode::CallVarKw => {
                let mut kwargs = None;
                if insn.op == Opcode::CallKw || insn.op == Opcode::CallVarKw {
                    kwargs = Some(pop!());
                }

                let mut args = None;
                if insn.op == Opcode::CallVar || insn.op == Opcode::CallVarKw {
                    args = Some(pop!());
                }

                // named args (pairs)
                let nkvpairs = arg & 0xff;
                let mut kvpairs: Vec<(Value, Value)> = Vec::with_capacity(nkvpairs);
                if nkvpairs > 0 {
                    let base = stack.len() - 2 * nkvpairs;
                    for i in 0..nkvpairs {
                        kvpairs
                            .push((stack[base + 2 * i].clone(), stack[base + 2 * i + 1].clone()));
                    }
                    stack.truncate(base);
                }
                if let Some(kwargs) = kwargs {
                    // Add key/value items from **kwargs dictionary.
                    let items = match &kwargs {
                        Value::Dict(d) => Some(d.items()),
                        Value::Other(o) => o.items(),
                        _ => None,
                    };
                    let Some(items) = items else {
                        fail!(Error::new(format!(
                            "argument after ** must be a mapping, not {}",
                            kwargs.type_name()
                        )));
                    };
                    if let Some((k, _)) = items.iter().find(|(k, _)| !matches!(k, Value::String(_)))
                    {
                        fail!(Error::new(format!(
                            "keywords must be strings, not {}",
                            k.type_name()
                        )));
                    }
                    kvpairs.extend(items);
                }

                // positional args
                let npos = arg >> 8;
                let mut positional: Vec<Value> = stack.split_off(stack.len() - npos);
                if let Some(args) = args {
                    // Add elements from *args sequence.
                    let Some(mut iter) = iterate(&args) else {
                        fail!(Error::new(format!(
                            "argument after * must be iterable, not {}",
                            args.type_name()
                        )));
                    };
                    while let Some(elem) = iter.next() {
                        positional.push(elem);
                    }
                }

                let function = pop!();

                let z = tri!(call(thread, &function, &positional, &kvpairs));
                stack.push(z);

                // Check whether callee cancelled the thread.
                if thread.check_steps() {
                    tri!(thread.check_steps_slow());
                }
            }

            Opcode::Iterpush => {
                let x = pop!();
                let Some(iter) = iterate(&x) else {
                    fail!(Error::new(format!(
                        "{} value is not iterable",
                        x.type_name()
                    )));
                };
                iterstack.push(iter);
            }

            Opcode::Iterjmp => {
                let iter = iterstack.last_mut().expect("iterator stack underflow");
                match iter.next() {
                    Some(v) => stack.push(v),
                    None => pc = arg,
                }
            }

            Opcode::Iterpop => {
                iterstack.pop();
            }

            Opcode::Not => {
                let x = pop!();
                stack.push(Value::Bool(!x.truth()));
            }

            Opcode::Return => {
                break Ok(pop!());
            }

            Opcode::Setindex => {
                let z = pop!();
                let y = pop!();
                let x = pop!();
                tri!(set_index(&x, &y, z));
            }

            Opcode::Index => {
                let y = pop!();
                let x = pop!();
                let z = tri!(get_index(&x, &y));
                stack.push(z);
            }

            Opcode::Attr => {
                let x = pop!();
                let name = &prog.names[arg];
                let y = tri!(get_attr(&x, name));
                stack.push(y);
            }

            Opcode::Setfield => {
                let y = pop!();
                let x = pop!();
                let name = &prog.names[arg];
                tri!(set_field(&x, name, y));
            }

            Opcode::Makedict => stack.push(Value::Dict(Rc::new(Dict::new()))),

            Opcode::Setdict | Opcode::Setdictuniq => {
                let v = pop!();
                let k = pop!();
                let dict = pop!();
                let Value::Dict(dict) = dict else {
                    fail!(Error::new("internal error: SETDICT on non-dict"));
                };
                let oldlen = dict.len();
                tri!(dict.set_key(k.clone(), v));
                if insn.op == Opcode::Setdictuniq && dict.len() == oldlen {
                    fail!(Error::new(format!("duplicate key: {}", k.repr())));
                }
            }

            Opcode::Append => {
                let elem = pop!();
                let list = pop!();
                if let Value::List(l) = list {
                    l.push_unchecked(elem);
                }
            }

            Opcode::Slice => {
                let step = pop!();
                let hi = pop!();
                let lo = pop!();
                let x = pop!();
                let res = tri!(slice(&x, &lo, &hi, &step));
                stack.push(res);
            }

            Opcode::Unpack => {
                let n = arg;
                let iterable = pop!();
                let Some(mut iter) = iterate(&iterable) else {
                    fail!(Error::new(format!(
                        "got {} in sequence assignment",
                        iterable.type_name()
                    )));
                };
                let mut elems = Vec::with_capacity(n);
                while elems.len() < n {
                    match iter.next() {
                        Some(v) => elems.push(v),
                        None => break,
                    }
                }
                let too_many = iter.next().is_some();
                drop(iter);
                if too_many {
                    // NB: Len may return -1 here in obscure cases.
                    let l = len(&iterable).map_or(-1, |l| l as i64);
                    fail!(Error::new(format!(
                        "too many values to unpack (got {}, want {})",
                        l, n
                    )));
                }
                if elems.len() < n {
                    fail!(Error::new(format!(
                        "too few values to unpack (got {}, want {})",
                        elems.len(),
                        n
                    )));
                }
                stack.extend(elems.into_iter().rev());
            }

            Opcode::Cjmp => {
                let cond = pop!();
                if cond.truth() {
                    // On back edge, check cancellation and increment step count.
                    if arg < pc && thread.check_steps() {
                        tri!(thread.check_steps_slow());
                    }
                    pc = arg;
                }
            }

            Opcode::Constant => stack.push(module.constants[arg].clone()),

            Opcode::Maketuple => {
                let elems = stack.split_off(stack.len() - arg);
                stack.push(Value::tuple(elems));
            }

            Opcode::Makelist => {
                let elems = stack.split_off(stack.len() - arg);
                stack.push(Value::list(elems));
            }

            Opcode::Makefunc => {
                let funcode = prog.functions[arg].clone();
                let Value::Tuple(tuple) = pop!() else {
                    fail!(Error::new("internal error: MAKEFUNC without tuple"));
                };
                let n = tuple.len() - funcode.freevars.len();
                let defaults = tuple[..n].to_vec();
                let freevars = tuple[n..].to_vec();
                stack.push(Value::Function(Rc::new(Function::new(
                    funcode,
                    module.clone(),
                    defaults,
                    freevars,
                ))));
            }

            Opcode::Load => {
                let n = arg;
                let modv = pop!();
                let modname =
                    String::from_utf8_lossy(modv.as_str().unwrap_or_default()).into_owned();

                let Some(load) = thread.load.clone() else {
                    fail!(Error::new("load not implemented by this application"));
                };

                let dict = match load(thread, &modname) {
                    Ok(d) => d,
                    Err(err2) => fail!(Error::Wrapped(
                        format!("cannot load {}: {}", modname, err2),
                        Rc::new(err2)
                    )),
                };

                let base = stack.len();
                let mut failed = None;
                for i in 0..n {
                    let from =
                        String::from_utf8_lossy(stack[base - 1 - i].as_str().unwrap_or_default())
                            .into_owned();
                    match dict.get(&from) {
                        Some(v) => stack[base - 1 - i] = v.clone(),
                        None => {
                            let mut msg =
                                format!("load: name {} not found in module {}", from, modname);
                            if let Some(n) = nearest(&from, &dict.keys()) {
                                msg = format!("{} (did you mean {}?)", msg, n);
                            }
                            failed = Some(Error::new(msg));
                            break;
                        }
                    }
                }
                if let Some(e) = failed {
                    fail!(e);
                }

                // Check whether Load callback cancelled the thread.
                if thread.check_steps() {
                    tri!(thread.check_steps_slow());
                }
            }

            Opcode::Setlocal => {
                thread.stack[fi].locals[arg] = Some(pop!());
            }

            Opcode::Setlocalcell => {
                let v = pop!();
                if let Some(c) = thread.stack[fi].locals[arg].as_ref().and_then(as_cell) {
                    *c.0.borrow_mut() = Some(v);
                }
            }

            Opcode::Setglobal => {
                let v = pop!();
                module.globals.borrow_mut()[arg] = Some(v);
            }

            Opcode::Local => match &thread.stack[fi].locals[arg] {
                Some(x) => stack.push(x.clone()),
                None => fail!(Error::new(format!(
                    "local variable {} referenced before assignment",
                    f.locals[arg].name
                ))),
            },

            Opcode::Free => stack.push(fnv.freevars[arg].clone()),

            Opcode::Localcell => {
                let v = thread.stack[fi].locals[arg]
                    .as_ref()
                    .and_then(as_cell)
                    .and_then(|c| c.0.borrow().clone());
                match v {
                    Some(v) => stack.push(v),
                    None => fail!(Error::new(format!(
                        "local variable {} referenced before assignment",
                        f.locals[arg].name
                    ))),
                }
            }

            Opcode::Freecell => {
                let v = as_cell(&fnv.freevars[arg]).and_then(|c| c.0.borrow().clone());
                match v {
                    Some(v) => stack.push(v),
                    None => fail!(Error::new(format!(
                        "local variable {} referenced before assignment",
                        f.freevars[arg].name
                    ))),
                }
            }

            Opcode::Global => {
                let x = module.globals.borrow()[arg].clone();
                match x {
                    Some(x) => stack.push(x),
                    None => fail!(Error::new(format!(
                        "global variable {} referenced before assignment",
                        prog.globals[arg].name
                    ))),
                }
            }

            Opcode::Predeclared => {
                let name = &prog.names[arg];
                match module.predeclared.get(name) {
                    Some(x) => stack.push(x.clone()),
                    None => fail!(Error::new(format!(
                        "internal error: predeclared variable {} is uninitialized",
                        name
                    ))),
                }
            }

            Opcode::Universal => {
                let name = &prog.names[arg];
                match universal(name) {
                    Some(x) => stack.push(x),
                    None => fail!(Error::new(format!(
                        "internal error: universal variable {} is undefined",
                        name
                    ))),
                }
            }
        }
    };
    // (iterators are dropped, i.e. Done, here)
    drop(iterstack);
    result
}

/// A sentinel value used in a function's defaults tuple to indicate that
/// a (keyword-only) parameter is mandatory.
pub(crate) struct Mandatory;

impl StarlarkValue for Mandatory {
    fn type_name(&self) -> &str {
        "mandatory"
    }
    fn repr(&self, out: &mut String) {
        out.push_str("mandatory");
    }
    fn hash(&self) -> Result<u32, Error> {
        Ok(0)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn truth(&self) -> bool {
        false
    }
}

/// Reports whether v is the mandatory sentinel.
pub(crate) fn is_mandatory(v: &Value) -> bool {
    v.downcast_ref::<Mandatory>().is_some()
}

/// A cell is a box containing a Value. Local variables marked as cells
/// hold their value indirectly so that they may be shared by outer and
/// inner nested functions. Cells are always accessed using indirect
/// {FREE,LOCAL,SETLOCAL}CELL instructions. The FreeVars tuple contains
/// only cells. The FREE instruction always yields a cell.
pub(crate) struct CellValue(RefCell<Option<Value>>);

impl StarlarkValue for CellValue {
    fn type_name(&self) -> &str {
        "cell"
    }
    fn repr(&self, out: &mut String) {
        out.push_str("cell");
    }
    fn hash(&self) -> Result<u32, Error> {
        Err(Error::new("unhashable: cell"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn freeze(&self) {
        let v = self.0.borrow().clone();
        if let Some(v) = v {
            v.freeze();
        }
    }
}

fn as_cell(v: &Value) -> Option<&CellValue> {
    v.downcast_ref::<CellValue>()
}

/// Returns the contents of the cell v, if v is a cell holding a value.
pub(crate) fn cell_contents(v: &Value) -> Option<Value> {
    as_cell(v).and_then(|c| c.0.borrow().clone())
}
