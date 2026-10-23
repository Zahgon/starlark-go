// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Syntax tree traversal (ported from go.starlark.net/syntax/walk.go).

use crate::syntax::ast::*;

/// A reference to a node of a syntax tree.
///
/// Identifiers, literals and dict entries are always presented as
/// `Ident`, `Literal` and `DictEntry` respectively, never as `Expr`.
#[derive(Clone, Copy, Debug)]
pub enum Node<'a> {
    File(&'a File),
    Stmt(&'a Stmt),
    Expr(&'a Expr),
    Ident(&'a Ident),
    Literal(&'a Literal),
    DictEntry(&'a DictEntry),
    Clause(&'a Clause),
}

impl<'a> Node<'a> {
    fn of_expr(e: &'a Expr) -> Node<'a> {
        match e {
            Expr::Ident(id) => Node::Ident(id),
            Expr::Literal(lit) => Node::Literal(lit),
            Expr::DictEntry(entry) => Node::DictEntry(entry),
            _ => Node::Expr(e),
        }
    }

    /// Returns the name of the node's type, as in the Go implementation
    /// (e.g. "CallExpr", "ForStmt").
    pub fn type_name(&self) -> &'static str {
        match self {
            Node::File(_) => "File",
            Node::Stmt(s) => match s {
                Stmt::Assign(_) => "AssignStmt",
                Stmt::Branch(_) => "BranchStmt",
                Stmt::Def(_) => "DefStmt",
                Stmt::Expr(_) => "ExprStmt",
                Stmt::For(_) => "ForStmt",
                Stmt::While(_) => "WhileStmt",
                Stmt::If(_) => "IfStmt",
                Stmt::Load(_) => "LoadStmt",
                Stmt::Return(_) => "ReturnStmt",
            },
            Node::Expr(e) => match e {
                Expr::Binary(_) => "BinaryExpr",
                Expr::Call(_) => "CallExpr",
                Expr::Comprehension(_) => "Comprehension",
                Expr::Cond(_) => "CondExpr",
                Expr::DictEntry(_) => "DictEntry",
                Expr::Dict(_) => "DictExpr",
                Expr::Dot(_) => "DotExpr",
                Expr::Ident(_) => "Ident",
                Expr::Index(_) => "IndexExpr",
                Expr::Lambda(_) => "LambdaExpr",
                Expr::List(_) => "ListExpr",
                Expr::Literal(_) => "Literal",
                Expr::Paren(_) => "ParenExpr",
                Expr::Slice(_) => "SliceExpr",
                Expr::Tuple(_) => "TupleExpr",
                Expr::Unary(_) => "UnaryExpr",
            },
            Node::Ident(_) => "Ident",
            Node::Literal(_) => "Literal",
            Node::DictEntry(_) => "DictEntry",
            Node::Clause(Clause::For(_)) => "ForClause",
            Node::Clause(Clause::If(_)) => "IfClause",
        }
    }
}

/// Walk traverses a syntax tree in depth-first order.
/// It starts by calling f(Some(n)).
/// If f returns true, Walk calls itself
/// recursively for each child of n.
/// Walk then calls f(None).
pub fn walk<'a>(n: Node<'a>, f: &mut dyn FnMut(Option<Node<'a>>) -> bool) {
    if !f(Some(n)) {
        return;
    }

    match n {
        Node::File(file) => walk_stmts(&file.stmts, f),
        Node::Stmt(stmt) => match stmt {
            Stmt::Expr(s) => walk_expr(&s.x, f),
            Stmt::Branch(_) => {}
            Stmt::If(s) => {
                walk_expr(&s.cond, f);
                walk_stmts(&s.true_, f);
                walk_stmts(&s.false_, f);
            }
            Stmt::Assign(s) => {
                walk_expr(&s.lhs, f);
                walk_expr(&s.rhs, f);
            }
            Stmt::Def(s) => {
                walk(Node::Ident(&s.name), f);
                for param in &s.params {
                    walk_expr(param, f);
                }
                walk_stmts(&s.body, f);
            }
            Stmt::For(s) => {
                walk_expr(&s.vars, f);
                walk_expr(&s.x, f);
                walk_stmts(&s.body, f);
            }
            Stmt::While(s) => {
                walk_expr(&s.cond, f);
                walk_stmts(&s.body, f);
            }
            Stmt::Return(s) => {
                if let Some(r) = &s.result {
                    walk_expr(r, f);
                }
            }
            Stmt::Load(s) => {
                walk(Node::Literal(&s.module), f);
                for from in &s.from {
                    walk(Node::Ident(from), f);
                }
                for to in &s.to {
                    walk(Node::Ident(to), f);
                }
            }
        },
        Node::Ident(_) | Node::Literal(_) => {}
        Node::DictEntry(e) => {
            walk_expr(&e.key, f);
            walk_expr(&e.value, f);
        }
        Node::Clause(Clause::If(c)) => walk_expr(&c.cond, f),
        Node::Clause(Clause::For(c)) => {
            walk_expr(&c.vars, f);
            walk_expr(&c.x, f);
        }
        Node::Expr(e) => match e {
            Expr::Ident(_) | Expr::Literal(_) => {}
            Expr::List(e) => {
                for x in &e.list {
                    walk_expr(x, f);
                }
            }
            Expr::Paren(e) => walk_expr(&e.x, f),
            Expr::Cond(e) => {
                walk_expr(&e.cond, f);
                walk_expr(&e.true_, f);
                walk_expr(&e.false_, f);
            }
            Expr::Index(e) => {
                walk_expr(&e.x, f);
                walk_expr(&e.y, f);
            }
            Expr::DictEntry(e) => {
                walk_expr(&e.key, f);
                walk_expr(&e.value, f);
            }
            Expr::Slice(e) => {
                walk_expr(&e.x, f);
                for x in [&e.lo, &e.hi, &e.step].into_iter().flatten() {
                    walk_expr(x, f);
                }
            }
            Expr::Comprehension(e) => {
                walk_expr(&e.body, f);
                for clause in &e.clauses {
                    walk(Node::Clause(clause), f);
                }
            }
            Expr::Tuple(e) => {
                for x in &e.list {
                    walk_expr(x, f);
                }
            }
            Expr::Dict(e) => {
                for entry in &e.list {
                    walk(Node::DictEntry(entry), f);
                }
            }
            Expr::Unary(e) => {
                if let Some(x) = &e.x {
                    walk_expr(x, f);
                }
            }
            Expr::Binary(e) => {
                walk_expr(&e.x, f);
                walk_expr(&e.y, f);
            }
            Expr::Dot(e) => {
                walk_expr(&e.x, f);
                walk(Node::Ident(&e.name), f);
            }
            Expr::Call(e) => {
                walk_expr(&e.func, f);
                for arg in &e.args {
                    walk_expr(arg, f);
                }
            }
            Expr::Lambda(e) => {
                for param in &e.params {
                    walk_expr(param, f);
                }
                walk_expr(&e.body, f);
            }
        },
    }

    f(None);
}

fn walk_expr<'a>(e: &'a Expr, f: &mut dyn FnMut(Option<Node<'a>>) -> bool) {
    walk(Node::of_expr(e), f);
}

fn walk_stmts<'a>(stmts: &'a [Stmt], f: &mut dyn FnMut(Option<Node<'a>>) -> bool) {
    for stmt in stmts {
        walk(Node::Stmt(stmt), f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{parse, FileOptions};

    #[test]
    fn test_walk() {
        let src = "
for x in y:
  if x:
    pass
  else:
    f([2*x for x in \"abc\"])
";
        let f = parse(&FileOptions::default(), "hello.go", src.as_bytes()).unwrap();
        let mut buf = String::new();
        let mut depth = 0;
        walk(Node::File(&f), &mut |n| {
            match n {
                None => depth -= 1,
                Some(n) => {
                    buf.push_str(&"  ".repeat(depth));
                    buf.push_str(n.type_name());
                    buf.push('\n');
                    depth += 1;
                }
            }
            true
        });
        let want = "
File
  ForStmt
    Ident
    Ident
    IfStmt
      Ident
      BranchStmt
      ExprStmt
        CallExpr
          Ident
          Comprehension
            BinaryExpr
              Literal
              Ident
            ForClause
              Ident
              Literal";
        assert_eq!(buf.trim(), want.trim());
    }

    #[test]
    fn example_walk() {
        let src = r#"
load("library", "a")

def b(c, *, d=e):
    f += {g: h}
    i = -(j)
    return k.l[m + n]

for o in [p for q, r in s if t]:
    u(lambda: v, w[x:y:z])
"#;
        let f = parse(&FileOptions::default(), "hello.star", src.as_bytes()).unwrap();
        let mut idents = Vec::new();
        walk(Node::File(&f), &mut |n| {
            if let Some(Node::Ident(id)) = n {
                idents.push(id.name.clone());
            }
            true
        });
        assert_eq!(
            idents.join(" "),
            "a a b c d e f g h i j k l m n o p q r s t u v w x y z"
        );
    }
}
