// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! A recursive-descent parser for Starlark (ported from
//! go.starlark.net/syntax/parse.go).
//!
//! The LL(1) grammar of Starlark and the names of many productions follow
//! Python 2.7.

use std::cell::RefCell;
use std::rc::Rc;

use crate::syntax::ast::*;
use crate::syntax::scan::{Error, FilePortion, Readline, Result, Scanner, TokenValue};
use crate::syntax::{FileOptions, Position, Token};

/// Parses the input data and returns the corresponding parse tree.
///
/// The filename is only used when recording position information.
pub fn parse(opts: &FileOptions, filename: &str, src: &[u8]) -> std::result::Result<File, Error> {
    let mut p = Parser::new(opts, Scanner::new(filename, src.to_vec(), None));
    p.next_token()?; // read first lookahead token
    let mut f = p.parse_file()?;
    f.path = filename.to_string();
    Ok(f)
}

/// Like `parse`, but for a portion of a file whose first line and column
/// numbers are not (1, 1), such as when a file is extracted from within a
/// larger file (Go's Parse with a FilePortion source).
pub fn parse_file_portion(
    opts: &FileOptions,
    filename: &str,
    portion: &FilePortion,
) -> std::result::Result<File, Error> {
    let mut sc = Scanner::new(filename, portion.content.to_vec(), None);
    sc.pos = Position::new(Rc::from(filename), portion.first_line, portion.first_col);
    let mut p = Parser::new(opts, sc);
    p.next_token()?; // read first lookahead token
    let mut f = p.parse_file()?;
    f.path = filename.to_string();
    Ok(f)
}

/// Parses a single compound statement:
/// a blank line, a def, for, while, or if statement, or a
/// semicolon-separated list of simple statements followed
/// by a newline. These are the units on which the REPL operates.
/// parse_compound_stmt does not consume any following input.
/// The parser calls the readline function each
/// time it needs a new line of input; it returns `Ok(None)` at end of
/// input, which is reported as an error whose message is "EOF".
pub fn parse_compound_stmt(
    opts: &FileOptions,
    filename: &str,
    readline: &mut dyn FnMut() -> std::io::Result<Option<Vec<u8>>>,
) -> std::result::Result<File, Error> {
    let readline: Readline<'_> = readline;
    let mut p = Parser::new(opts, Scanner::new(filename, Vec::new(), Some(readline)));
    p.next_token()?; // read first lookahead token

    let mut stmts = Vec::new();
    match p.tok {
        Token::Def | Token::If | Token::For | Token::While => p.parse_stmt(&mut stmts)?,
        Token::Newline => {
            // blank line
        }
        _ => {
            p.parse_simple_stmt(&mut stmts, false)?;
            // Require but don't consume newline, to avoid blocking again.
            if p.tok != Token::Newline {
                return p.in_.error(p.in_.pos.clone(), "invalid syntax");
            }
        }
    }

    Ok(File {
        options: opts.clone(),
        path: filename.to_string(),
        stmts,
        module: None,
    })
}

/// Parses a Starlark expression.
/// A comma-separated list of expressions is parsed as a tuple.
pub fn parse_expr(
    opts: &FileOptions,
    filename: &str,
    src: &[u8],
) -> std::result::Result<Expr, Error> {
    let mut p = Parser::new(opts, Scanner::new(filename, src.to_vec(), None));
    p.next_token()?; // read first lookahead token

    // Use parse_expr, not parse_test, to permit an unparenthesized tuple.
    let expr = p.parse_expr(false)?;

    // A following newline (e.g. "f()\n") appears outside any brackets,
    // on a non-blank line, and thus results in a NEWLINE token.
    if p.tok == Token::Newline {
        p.next_token()?;
    }

    if p.tok != Token::Eof {
        return p.in_.error(
            p.in_.pos.clone(),
            format!("got {} after expression, want EOF", p.tok.go_string()),
        );
    }
    Ok(expr)
}

struct Parser<'a> {
    options: FileOptions,
    in_: Scanner<'a>,
    tok: Token,
    tokval: TokenValue,
    depth: usize,
}

/// Returns the precedence (0-9) of the binary operator, or -1 for other
/// tokens.
///
/// Comparisons are nonassociative; other binary operators associate to the
/// left. Unary MINUS, unary PLUS, and TILDE have higher precedence so are
/// handled in parse_primary.
/// See https://github.com/google/starlark-go/blob/master/doc/spec.md#binary-operators
fn precedence(tok: Token) -> i32 {
    match tok {
        Token::Or => 0,
        Token::And => 1,
        Token::Not => 2,
        Token::Eql
        | Token::Neq
        | Token::Lt
        | Token::Gt
        | Token::Le
        | Token::Ge
        | Token::In
        | Token::NotIn => 3,
        Token::Pipe => 4,
        Token::Circumflex => 5,
        Token::Amp => 6,
        Token::Ltlt | Token::Gtgt => 7,
        Token::Minus | Token::Plus => 8,
        Token::Star | Token::Percent | Token::Slash | Token::Slashslash => 9,
        _ => -1,
    }
}

/// The number of precedence levels.
const PRECLEVELS: i32 = 10;

fn terminates_expr_list(tok: Token) -> bool {
    matches!(
        tok,
        Token::Eof
            | Token::Newline
            | Token::Eq
            | Token::Rbrace
            | Token::Rbrack
            | Token::Rparen
            | Token::Semi
    )
}

impl<'a> Parser<'a> {
    fn new(opts: &FileOptions, in_: Scanner<'a>) -> Parser<'a> {
        Parser {
            options: opts.clone(),
            in_,
            tok: Token::Illegal,
            tokval: TokenValue::default(),
            depth: 0,
        }
    }

    fn errorf<T>(&self, msg: String) -> Result<T> {
        self.in_.error(self.in_.pos.clone(), msg)
    }

    /// Increments the depth and checks the recursion limit.
    /// It must be paired with a call to leave.
    ///
    /// Every cycle in the parser's static call graph must include at
    /// least one function that uses enter+leave to break cycles.
    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > 1000 {
            return self.errorf("excessive nesting".to_string());
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Advances the scanner and returns the position of the
    /// previous token.
    fn next_token(&mut self) -> Result<Position> {
        let oldpos = self.tokval.pos.clone();
        self.tok = self.in_.next_token(&mut self.tokval)?;
        Ok(oldpos)
    }

    /// file_input = (NEWLINE | stmt)* EOF
    fn parse_file(&mut self) -> Result<File> {
        let mut stmts = Vec::new();
        while self.tok != Token::Eof {
            if self.tok == Token::Newline {
                self.next_token()?;
                continue;
            }
            self.parse_stmt(&mut stmts)?;
        }
        Ok(File {
            options: self.options.clone(),
            stmts,
            ..File::default()
        })
    }

    fn parse_stmt(&mut self, stmts: &mut Vec<Stmt>) -> Result<()> {
        match self.tok {
            Token::Def => stmts.push(self.parse_def_stmt()?),
            Token::If => stmts.push(self.parse_if_stmt()?),
            Token::For => stmts.push(self.parse_for_stmt()?),
            Token::While => stmts.push(self.parse_while_stmt()?),
            _ => self.parse_simple_stmt(stmts, true)?,
        }
        Ok(())
    }

    fn parse_def_stmt(&mut self) -> Result<Stmt> {
        let def = self.next_token()?; // consume DEF
        let name = self.parse_ident()?;
        let lparen = self.consume(Token::Lparen)?;
        let params = self.parse_params()?;
        let rparen = self.consume(Token::Rparen)?;
        self.consume(Token::Colon)?;
        let body = self.parse_suite()?;
        Ok(Stmt::Def(DefStmt {
            def,
            name,
            lparen,
            params,
            rparen,
            body,
            function: RefCell::new(None),
        }))
    }

    fn parse_if_stmt(&mut self) -> Result<Stmt> {
        let ifpos = self.next_token()?; // consume IF
        let cond = self.parse_test()?;
        self.consume(Token::Colon)?;
        let body = self.parse_suite()?;
        let mut if_stmt = IfStmt {
            if_: ifpos,
            cond,
            true_: body,
            else_pos: Position::default(),
            false_: Vec::new(),
        };
        // The elif chain is built innermost-last; collect it first.
        let mut elifs = Vec::new();
        while self.tok == Token::Elif {
            let elifpos = self.next_token()?; // consume ELIF
            let cond = self.parse_test()?;
            self.consume(Token::Colon)?;
            let body = self.parse_suite()?;
            elifs.push(IfStmt {
                if_: elifpos,
                cond,
                true_: body,
                else_pos: Position::default(),
                false_: Vec::new(),
            });
        }
        let mut else_part = None;
        if self.tok == Token::Else {
            let elsepos = self.next_token()?; // consume ELSE
            self.consume(Token::Colon)?;
            else_part = Some((elsepos, self.parse_suite()?));
        }

        // Link the chain: each tail's ElsePos is the following ELIF (or
        // ELSE) and its False branch the following statement(s).
        let mut tail: Option<IfStmt> = None;
        for mut elif in elifs.into_iter().rev() {
            match tail.take() {
                Some(next) => {
                    elif.else_pos = next.if_.clone();
                    elif.false_ = vec![Stmt::If(next)];
                }
                None => {
                    if let Some((elsepos, body)) = else_part.take() {
                        elif.else_pos = elsepos;
                        elif.false_ = body;
                    }
                }
            }
            tail = Some(elif);
        }
        match tail {
            Some(next) => {
                if_stmt.else_pos = next.if_.clone();
                if_stmt.false_ = vec![Stmt::If(next)];
            }
            None => {
                if let Some((elsepos, body)) = else_part {
                    if_stmt.else_pos = elsepos;
                    if_stmt.false_ = body;
                }
            }
        }
        Ok(Stmt::If(if_stmt))
    }

    fn parse_for_stmt(&mut self) -> Result<Stmt> {
        let forpos = self.next_token()?; // consume FOR
        let vars = self.parse_for_loop_variables()?;
        self.consume(Token::In)?;
        let x = self.parse_expr(false)?;
        self.consume(Token::Colon)?;
        let body = self.parse_suite()?;
        Ok(Stmt::For(ForStmt {
            for_: forpos,
            vars,
            x,
            body,
        }))
    }

    fn parse_while_stmt(&mut self) -> Result<Stmt> {
        let whilepos = self.next_token()?; // consume WHILE
        let cond = self.parse_test()?;
        self.consume(Token::Colon)?;
        let body = self.parse_suite()?;
        Ok(Stmt::While(WhileStmt {
            while_: whilepos,
            cond,
            body,
        }))
    }

    /// Equivalent to 'exprlist' production in Python grammar.
    ///
    /// loop_variables = primary_with_suffix (COMMA primary_with_suffix)* COMMA?
    fn parse_for_loop_variables(&mut self) -> Result<Expr> {
        // Avoid parse_expr because it would consume the IN token
        // following x in "for x in y: ...".
        let v = self.parse_primary_with_suffix()?;
        if self.tok != Token::Comma {
            return Ok(v);
        }

        let mut list = vec![v];
        while self.tok == Token::Comma {
            self.next_token()?;
            if terminates_expr_list(self.tok) {
                break;
            }
            list.push(self.parse_primary_with_suffix()?);
        }
        Ok(Expr::Tuple(TupleExpr {
            lparen: Position::default(),
            list,
            rparen: Position::default(),
        }))
    }

    /// simple_stmt = small_stmt (SEMI small_stmt)* SEMI? NEWLINE
    /// In REPL mode, it does not consume the NEWLINE.
    fn parse_simple_stmt(&mut self, stmts: &mut Vec<Stmt>, consume_nl: bool) -> Result<()> {
        loop {
            stmts.push(self.parse_small_stmt()?);
            if self.tok != Token::Semi {
                break;
            }
            self.next_token()?; // consume SEMI
            if self.tok == Token::Newline || self.tok == Token::Eof {
                break;
            }
        }
        // EOF without NEWLINE occurs in `if x: pass`, for example.
        if self.tok != Token::Eof && consume_nl {
            self.consume(Token::Newline)?;
        }
        Ok(())
    }

    /// small_stmt = RETURN expr?
    ///
    ///   | PASS | BREAK | CONTINUE
    ///   | LOAD ...
    ///   | expr ('=' | '+=' | '-=' | '*=' | '/=' | '%=' | '&=' | '|=' | '^=' | '<<=' | '>>=') expr   // assign
    ///   | expr
    fn parse_small_stmt(&mut self) -> Result<Stmt> {
        match self.tok {
            Token::Return => {
                let pos = self.next_token()?; // consume RETURN
                let mut result = None;
                if self.tok != Token::Eof && self.tok != Token::Newline && self.tok != Token::Semi {
                    result = Some(self.parse_expr(false)?);
                }
                return Ok(Stmt::Return(ReturnStmt {
                    return_: pos,
                    result,
                }));
            }

            Token::Break | Token::Continue | Token::Pass => {
                let tok = self.tok;
                let pos = self.next_token()?; // consume it
                return Ok(Stmt::Branch(BranchStmt {
                    token: tok,
                    token_pos: pos,
                }));
            }

            Token::Load => return self.parse_load_stmt(),

            _ => {}
        }

        // Assignment
        let x = self.parse_expr(false)?;
        match self.tok {
            Token::Eq
            | Token::PlusEq
            | Token::MinusEq
            | Token::StarEq
            | Token::SlashEq
            | Token::SlashslashEq
            | Token::PercentEq
            | Token::AmpEq
            | Token::PipeEq
            | Token::CircumflexEq
            | Token::LtltEq
            | Token::GtgtEq => {
                let op = self.tok;
                let pos = self.next_token()?; // consume op
                let rhs = self.parse_expr(false)?;
                return Ok(Stmt::Assign(AssignStmt {
                    op_pos: pos,
                    op,
                    lhs: x,
                    rhs,
                }));
            }
            _ => {}
        }

        // Expression statement (e.g. function call, doc string).
        Ok(Stmt::Expr(ExprStmt { x }))
    }

    /// Parses a string literal (the current token must be STRING).
    fn parse_string_literal(&mut self) -> Result<Literal> {
        match self.parse_primary()? {
            Expr::Literal(lit) => Ok(lit),
            _ => unreachable!("STRING token did not yield a literal"),
        }
    }

    /// Returns an identifier for the name denoted by the string literal
    /// `lit`, positioned just after its opening quote.
    fn literal_ident(lit: &Literal) -> Ident {
        let name = match &lit.value {
            LiteralValue::String(s) => String::from_utf8_lossy(s).into_owned(),
            _ => String::new(),
        };
        Ident::new(lit.token_pos.add(b"\""), name)
    }

    /// stmt = LOAD '(' STRING {',' (IDENT '=')? STRING} [','] ')'
    fn parse_load_stmt(&mut self) -> Result<Stmt> {
        let load_pos = self.next_token()?; // consume LOAD
        let lparen = self.consume(Token::Lparen)?;

        if self.tok != Token::String {
            return self
                .errorf("first operand of load statement must be a string literal".to_string());
        }
        let module = self.parse_string_literal()?;

        let mut from = Vec::new();
        let mut to = Vec::new();
        while self.tok != Token::Rparen && self.tok != Token::Eof {
            self.consume(Token::Comma)?;
            if self.tok == Token::Rparen {
                break; // allow trailing comma
            }
            match self.tok {
                Token::String => {
                    // load("module", "id")
                    // To name is same as original.
                    let lit = self.parse_string_literal()?;
                    let id = Self::literal_ident(&lit);
                    to.push(id.clone());
                    from.push(id);
                }

                Token::Ident => {
                    // load("module", to="from")
                    let id = self.parse_ident()?;
                    let name = id.name.clone();
                    to.push(id);
                    if self.tok != Token::Eq {
                        return self.errorf(format!(
                            "load operand must be \"{name}\" or {name}=\"originalname\" (want '=' after {name})"
                        ));
                    }
                    self.consume(Token::Eq)?;
                    if self.tok != Token::String {
                        return self.errorf(format!(
                            "original name of loaded symbol must be quoted: {name}=\"originalname\""
                        ));
                    }
                    let lit = self.parse_string_literal()?;
                    from.push(Self::literal_ident(&lit));
                }

                _ => {
                    return self.errorf(format!(
                        "load operand must be \"name\" or localname=\"name\" (got {})",
                        self.tok.go_string()
                    ));
                }
            }
        }
        let rparen = self.consume(Token::Rparen)?;

        if to.is_empty() {
            return self
                .in_
                .error(lparen, "load statement must import at least 1 symbol");
        }
        Ok(Stmt::Load(LoadStmt {
            load: load_pos,
            module,
            from,
            to,
            rparen,
        }))
    }

    /// suite is typically what follows a COLON (e.g. after DEF or FOR).
    /// suite = simple_stmt | NEWLINE INDENT stmt+ OUTDENT
    fn parse_suite(&mut self) -> Result<Vec<Stmt>> {
        self.enter()?;
        let r = self.parse_suite_inner();
        self.leave();
        r
    }

    fn parse_suite_inner(&mut self) -> Result<Vec<Stmt>> {
        let mut stmts = Vec::new();
        if self.tok == Token::Newline {
            self.next_token()?; // consume NEWLINE
            self.consume(Token::Indent)?;
            while self.tok != Token::Outdent && self.tok != Token::Eof {
                self.parse_stmt(&mut stmts)?;
            }
            self.consume(Token::Outdent)?;
            return Ok(stmts);
        }

        self.parse_simple_stmt(&mut stmts, true)?;
        Ok(stmts)
    }

    fn parse_ident(&mut self) -> Result<Ident> {
        if self.tok != Token::Ident {
            return self.errorf("not an identifier".to_string());
        }
        let id = Ident::new(self.tokval.pos.clone(), self.tokval.raw.clone());
        self.next_token()?;
        Ok(id)
    }

    fn consume(&mut self, t: Token) -> Result<Position> {
        if self.tok != t {
            return self.errorf(format!(
                "got {}, want {}",
                self.tok.go_string(),
                t.go_string()
            ));
        }
        self.next_token()
    }

    /// params = (param COMMA)* param COMMA?
    ///        |
    ///
    /// param = IDENT
    ///       | IDENT EQ test
    ///       | STAR
    ///       | STAR IDENT
    ///       | STARSTAR IDENT
    ///
    /// parse_params parses a parameter list.  The resulting expressions are of the form:
    ///
    /// ```text
    /// Ident                                          x
    /// Binary{Op: EQ, X: Ident, Y: Expr}              x=y
    /// Unary{Op: STAR}                                *
    /// Unary{Op: STAR, X: Ident}                      *args
    /// Unary{Op: STARSTAR, X: Ident}                  **kwargs
    /// ```
    fn parse_params(&mut self) -> Result<Vec<Expr>> {
        let mut params = Vec::new();
        while self.tok != Token::Rparen && self.tok != Token::Colon && self.tok != Token::Eof {
            if !params.is_empty() {
                self.consume(Token::Comma)?;
            }
            if self.tok == Token::Rparen {
                break;
            }

            // * or *args or **kwargs
            if self.tok == Token::Star || self.tok == Token::Starstar {
                let op = self.tok;
                let pos = self.next_token()?;
                let mut x = None;
                if op == Token::Starstar || self.tok == Token::Ident {
                    x = Some(Box::new(Expr::Ident(self.parse_ident()?)));
                }
                params.push(Expr::Unary(UnaryExpr { op_pos: pos, op, x }));
                continue;
            }

            // IDENT
            // IDENT = test
            let id = self.parse_ident()?;
            if self.tok == Token::Eq {
                // default value
                let eq = self.next_token()?;
                let dflt = self.parse_test()?;
                params.push(Expr::Binary(BinaryExpr {
                    x: Box::new(Expr::Ident(id)),
                    op_pos: eq,
                    op: Token::Eq,
                    y: Box::new(dflt),
                }));
                continue;
            }

            params.push(Expr::Ident(id));
        }
        Ok(params)
    }

    /// Parses an expression, possibly consisting of a
    /// comma-separated list of 'test' expressions.
    ///
    /// In many cases we must use parse_test to avoid ambiguity such as
    /// f(x, y) vs. f((x, y)).
    fn parse_expr(&mut self, in_parens: bool) -> Result<Expr> {
        let x = self.parse_test()?;
        if self.tok != Token::Comma {
            return Ok(x);
        }

        // tuple
        let list = self.parse_exprs(vec![x], in_parens)?;
        Ok(Expr::Tuple(TupleExpr {
            lparen: Position::default(),
            list,
            rparen: Position::default(),
        }))
    }

    /// Parses a comma-separated list of expressions, starting with the comma.
    /// It is used to parse tuples and list elements.
    /// expr_list = (',' expr)* ','?
    fn parse_exprs(
        &mut self,
        mut exprs: Vec<Expr>,
        allow_trailing_comma: bool,
    ) -> Result<Vec<Expr>> {
        while self.tok == Token::Comma {
            let pos = self.next_token()?;
            if terminates_expr_list(self.tok) {
                if !allow_trailing_comma {
                    return self
                        .in_
                        .error(pos, "unparenthesized tuple with trailing comma");
                }
                break;
            }
            exprs.push(self.parse_test()?);
        }
        Ok(exprs)
    }

    /// Parses a 'test', a single-component expression.
    fn parse_test(&mut self) -> Result<Expr> {
        self.enter()?;
        let r = self.parse_test_inner();
        self.leave();
        r
    }

    fn parse_test_inner(&mut self) -> Result<Expr> {
        if self.tok == Token::Lambda {
            return self.parse_lambda(true);
        }

        let x = self.parse_test_prec(0)?;

        // conditional expression (t IF cond ELSE f)
        if self.tok == Token::If {
            let ifpos = self.next_token()?;
            let cond = self.parse_test_prec(0)?;
            if self.tok != Token::Else {
                return self
                    .in_
                    .error(ifpos, "conditional expression without else clause");
            }
            let elsepos = self.next_token()?;
            let else_ = self.parse_test()?;
            return Ok(Expr::Cond(CondExpr {
                if_: ifpos,
                cond: Box::new(cond),
                true_: Box::new(x),
                else_pos: elsepos,
                false_: Box::new(else_),
            }));
        }

        Ok(x)
    }

    /// Parses a single-component expression without
    /// consuming a trailing 'if expr else expr'.
    fn parse_test_no_cond(&mut self) -> Result<Expr> {
        if self.tok == Token::Lambda {
            return self.parse_lambda(false);
        }
        self.parse_test_prec(0)
    }

    /// Parses a lambda expression.
    /// The allow_cond flag allows the body to be an 'a if b else c' conditional.
    fn parse_lambda(&mut self, allow_cond: bool) -> Result<Expr> {
        self.enter()?;
        let r = self.parse_lambda_inner(allow_cond);
        self.leave();
        r
    }

    fn parse_lambda_inner(&mut self, allow_cond: bool) -> Result<Expr> {
        let lambda = self.next_token()?;
        let mut params = Vec::new();
        if self.tok != Token::Colon {
            params = self.parse_params()?;
        }
        self.consume(Token::Colon)?;

        let body = if allow_cond {
            self.parse_test()?
        } else {
            self.parse_test_no_cond()?
        };

        Ok(Expr::Lambda(LambdaExpr {
            lambda,
            params,
            body: Box::new(body),
            function: RefCell::new(None),
        }))
    }

    fn parse_test_prec(&mut self, prec: i32) -> Result<Expr> {
        self.enter()?;
        let r = self.parse_test_prec_inner(prec);
        self.leave();
        r
    }

    fn parse_test_prec_inner(&mut self, prec: i32) -> Result<Expr> {
        if prec >= PRECLEVELS {
            return self.parse_primary_with_suffix();
        }

        // expr = NOT expr
        if self.tok == Token::Not && prec == precedence(Token::Not) {
            let pos = self.next_token()?;
            let x = self.parse_test_prec(prec)?;
            return Ok(Expr::Unary(UnaryExpr {
                op_pos: pos,
                op: Token::Not,
                x: Some(Box::new(x)),
            }));
        }

        self.parse_binop_expr(prec)
    }

    /// expr = test (OP test)*
    /// Uses precedence climbing; see http://www.engr.mun.ca/~theo/Misc/exp_parsing.htm#climbing.
    fn parse_binop_expr(&mut self, prec: i32) -> Result<Expr> {
        let mut x = self.parse_test_prec(prec + 1)?;
        let mut first = true;
        loop {
            if self.tok == Token::Not {
                self.next_token()?; // consume NOT
                                    // In this context, NOT must be followed by IN.
                                    // Replace NOT IN by a single NOT_IN token.
                if self.tok != Token::In {
                    return self.errorf(format!("got {}, want in", self.tok.go_string()));
                }
                self.tok = Token::NotIn;
            }

            // Binary operator of specified precedence?
            let opprec = precedence(self.tok);
            if opprec < prec {
                return Ok(x);
            }

            // Comparisons are non-associative.
            if !first && opprec == precedence(Token::Eql) {
                let xop = match &x {
                    Expr::Binary(b) => b.op,
                    _ => Token::Illegal,
                };
                return self.errorf(format!(
                    "{} does not associate with {} (use parens)",
                    xop.name(),
                    self.tok.name()
                ));
            }

            let op = self.tok;
            let pos = self.next_token()?;
            let y = self.parse_test_prec(opprec + 1)?;
            x = Expr::Binary(BinaryExpr {
                op_pos: pos,
                op,
                x: Box::new(x),
                y: Box::new(y),
            });
            first = false;
        }
    }

    /// primary_with_suffix = primary
    ///                     | primary '.' IDENT
    ///                     | primary slice_suffix
    ///                     | primary call_suffix
    fn parse_primary_with_suffix(&mut self) -> Result<Expr> {
        let mut x = self.parse_primary()?;
        loop {
            match self.tok {
                Token::Dot => {
                    let dot = self.next_token()?;
                    let name = self.parse_ident()?;
                    x = Expr::Dot(DotExpr {
                        dot,
                        x: Box::new(x),
                        name,
                    });
                }
                Token::Lbrack => x = self.parse_slice_suffix(x)?,
                Token::Lparen => x = self.parse_call_suffix(x)?,
                _ => return Ok(x),
            }
        }
    }

    /// slice_suffix = '[' expr? ':' expr?  ':' expr? ']'
    fn parse_slice_suffix(&mut self, x: Expr) -> Result<Expr> {
        let lbrack = self.next_token()?;
        let mut lo = None;
        let mut hi = None;
        let mut step = None;
        if self.tok != Token::Colon {
            let y = self.parse_expr(false)?;

            // index x[y]
            if self.tok == Token::Rbrack {
                let rbrack = self.next_token()?;
                return Ok(Expr::Index(IndexExpr {
                    x: Box::new(x),
                    lbrack,
                    y: Box::new(y),
                    rbrack,
                }));
            }

            lo = Some(Box::new(y));
        }

        // slice or substring x[lo:hi:step]
        if self.tok == Token::Colon {
            self.next_token()?;
            if self.tok != Token::Colon && self.tok != Token::Rbrack {
                hi = Some(Box::new(self.parse_test()?));
            }
        }
        if self.tok == Token::Colon {
            self.next_token()?;
            if self.tok != Token::Rbrack {
                step = Some(Box::new(self.parse_test()?));
            }
        }
        let rbrack = self.consume(Token::Rbrack)?;
        Ok(Expr::Slice(SliceExpr {
            x: Box::new(x),
            lbrack,
            lo,
            hi,
            step,
            rbrack,
        }))
    }

    /// call_suffix = '(' arg_list? ')'
    fn parse_call_suffix(&mut self, func: Expr) -> Result<Expr> {
        let lparen = self.consume(Token::Lparen)?;
        let rparen;
        let mut args = Vec::new();
        if self.tok == Token::Rparen {
            rparen = self.next_token()?;
        } else {
            args = self.parse_args()?;
            rparen = self.consume(Token::Rparen)?;
        }
        Ok(Expr::Call(CallExpr {
            func: Box::new(func),
            lparen,
            args,
            rparen,
        }))
    }

    /// Parses a list of actual parameter values (arguments).
    /// It mirrors the structure of parse_params.
    /// arg_list = ((arg COMMA)* arg COMMA?)?
    fn parse_args(&mut self) -> Result<Vec<Expr>> {
        let mut args = Vec::new();
        while self.tok != Token::Rparen && self.tok != Token::Eof {
            if !args.is_empty() {
                self.consume(Token::Comma)?;
            }
            if self.tok == Token::Rparen {
                break;
            }

            // *args or **kwargs
            if self.tok == Token::Star || self.tok == Token::Starstar {
                let op = self.tok;
                let pos = self.next_token()?;
                let x = self.parse_test()?;
                args.push(Expr::Unary(UnaryExpr {
                    op_pos: pos,
                    op,
                    x: Some(Box::new(x)),
                }));
                continue;
            }

            // We use a different strategy from Bazel here to stay within LL(1).
            // Instead of looking ahead two tokens (IDENT, EQ) we parse
            // 'test = test' then check that the first was an IDENT.
            let mut x = self.parse_test()?;

            if self.tok == Token::Eq {
                // name = value
                if !matches!(x, Expr::Ident(_)) {
                    return self.errorf("keyword argument must have form name=expr".to_string());
                }
                let eq = self.next_token()?;
                let y = self.parse_test()?;
                x = Expr::Binary(BinaryExpr {
                    x: Box::new(x),
                    op_pos: eq,
                    op: Token::Eq,
                    y: Box::new(y),
                });
            }

            args.push(x);
        }
        Ok(args)
    }

    /// primary = IDENT
    ///         | INT | FLOAT | STRING | BYTES
    ///         | '[' ...                    // list literal or comprehension
    ///         | '{' ...                    // dict literal or comprehension
    ///         | '(' ...                    // tuple or parenthesized expression
    ///         | ('-'|'+'|'~') primary_with_suffix
    fn parse_primary(&mut self) -> Result<Expr> {
        self.enter()?;
        let r = self.parse_primary_inner();
        self.leave();
        r
    }

    fn parse_primary_inner(&mut self) -> Result<Expr> {
        match self.tok {
            Token::Ident => Ok(Expr::Ident(self.parse_ident()?)),

            Token::Int | Token::Float | Token::String | Token::Bytes => {
                let tok = self.tok;
                let value = match tok {
                    Token::Int => match self.tokval.big_int.take() {
                        Some(b) => LiteralValue::BigInt(Rc::new(b)),
                        None => LiteralValue::Int(self.tokval.int),
                    },
                    Token::Float => LiteralValue::Float(self.tokval.float),
                    _ => LiteralValue::String(std::mem::take(&mut self.tokval.string)),
                };
                let raw = self.tokval.raw.clone();
                let pos = self.next_token()?;
                Ok(Expr::Literal(Literal {
                    token: tok,
                    token_pos: pos,
                    raw,
                    value,
                }))
            }

            Token::Lbrack => self.parse_list(),

            Token::Lbrace => self.parse_dict(),

            Token::Lparen => {
                let lparen = self.next_token()?;
                if self.tok == Token::Rparen {
                    // empty tuple
                    let rparen = self.next_token()?;
                    return Ok(Expr::Tuple(TupleExpr {
                        lparen,
                        list: Vec::new(),
                        rparen,
                    }));
                }
                let e = self.parse_expr(true)?; // allow trailing comma
                let rparen = self.consume(Token::Rparen)?;
                Ok(Expr::Paren(ParenExpr {
                    lparen,
                    x: Box::new(e),
                    rparen,
                }))
            }

            Token::Minus | Token::Plus | Token::Tilde => {
                // unary
                let tok = self.tok;
                let pos = self.next_token()?;
                let x = self.parse_primary_with_suffix()?;
                Ok(Expr::Unary(UnaryExpr {
                    op_pos: pos,
                    op: tok,
                    x: Some(Box::new(x)),
                }))
            }

            _ => {
                // Report start pos of final token as it may be a NEWLINE (#532).
                self.in_.error(
                    self.tokval.pos.clone(),
                    format!("got {}, want primary expression", self.tok.go_string()),
                )
            }
        }
    }

    /// list = '[' ']'
    ///      | '[' expr ']'
    ///      | '[' expr expr_list ']'
    ///      | '[' expr (FOR loop_variables IN expr)+ ']'
    fn parse_list(&mut self) -> Result<Expr> {
        let lbrack = self.next_token()?;
        if self.tok == Token::Rbrack {
            // empty List
            let rbrack = self.next_token()?;
            return Ok(Expr::List(ListExpr {
                lbrack,
                list: Vec::new(),
                rbrack,
            }));
        }

        let x = self.parse_test()?;

        if self.tok == Token::For {
            // list comprehension
            return self.parse_comprehension_suffix(lbrack, x, Token::Rbrack);
        }

        let mut exprs = vec![x];
        if self.tok == Token::Comma {
            // multi-item list literal
            exprs = self.parse_exprs(exprs, true)?; // allow trailing comma
        }

        let rbrack = self.consume(Token::Rbrack)?;
        Ok(Expr::List(ListExpr {
            lbrack,
            list: exprs,
            rbrack,
        }))
    }

    /// dict = '{' '}'
    ///      | '{' dict_entry_list '}'
    ///      | '{' dict_entry FOR loop_variables IN expr '}'
    fn parse_dict(&mut self) -> Result<Expr> {
        let lbrace = self.next_token()?;
        if self.tok == Token::Rbrace {
            // empty dict
            let rbrace = self.next_token()?;
            return Ok(Expr::Dict(DictExpr {
                lbrace,
                list: Vec::new(),
                rbrace,
            }));
        }

        let x = self.parse_dict_entry()?;

        if self.tok == Token::For {
            // dict comprehension
            return self.parse_comprehension_suffix(
                lbrace,
                Expr::DictEntry(Box::new(x)),
                Token::Rbrace,
            );
        }

        let mut entries = vec![x];
        while self.tok == Token::Comma {
            self.next_token()?;
            if self.tok == Token::Rbrace {
                break;
            }
            entries.push(self.parse_dict_entry()?);
        }

        let rbrace = self.consume(Token::Rbrace)?;
        Ok(Expr::Dict(DictExpr {
            lbrace,
            list: entries,
            rbrace,
        }))
    }

    /// dict_entry = test ':' test
    fn parse_dict_entry(&mut self) -> Result<DictEntry> {
        let key = self.parse_test()?;
        let colon = self.consume(Token::Colon)?;
        let value = self.parse_test()?;
        Ok(DictEntry { key, colon, value })
    }

    /// comp_suffix = FOR loopvars IN expr comp_suffix
    ///             | IF expr comp_suffix
    ///             | ']'  or  ')'                              (end)
    ///
    /// There can be multiple FOR/IF clauses; the first is always a FOR.
    fn parse_comprehension_suffix(
        &mut self,
        lbrace: Position,
        body: Expr,
        end_brace: Token,
    ) -> Result<Expr> {
        let mut clauses = Vec::new();
        while self.tok != end_brace {
            if self.tok == Token::For {
                let pos = self.next_token()?;
                let vars = self.parse_for_loop_variables()?;
                let in_ = self.consume(Token::In)?;
                // Following Python 3, the operand of IN cannot be:
                // - a conditional expression ('x if y else z'),
                //   due to conflicts in Python grammar
                //  ('if' is used by the comprehension);
                // - a lambda expression
                // - an unparenthesized tuple.
                let x = self.parse_test_prec(0)?;
                clauses.push(Clause::For(ForClause {
                    for_: pos,
                    vars,
                    in_,
                    x,
                }));
            } else if self.tok == Token::If {
                let pos = self.next_token()?;
                let cond = self.parse_test_no_cond()?;
                clauses.push(Clause::If(IfClause { if_: pos, cond }));
            } else {
                return self.errorf(format!(
                    "got {}, want '{}', for, or if",
                    self.tok.go_string(),
                    end_brace.name()
                ));
            }
        }
        let rbrace = self.next_token()?;

        Ok(Expr::Comprehension(Comprehension {
            curly: end_brace == Token::Rbrace,
            lbrack: lbrace,
            body: Box::new(body),
            clauses,
            rbrack: rbrace,
        }))
    }
}
