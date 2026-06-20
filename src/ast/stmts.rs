//! Statement and block AST — `let` / `let-else` / `let-uninit` /
//! `defer` / `errdefer` / assignment / compound-assign / expression
//! statements, plus the `Block` wrapper.

use crate::token::Span;

use super::{Expr, Pattern, TypeExpr};

// ── Blocks ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub final_expr: Option<Box<Expr>>,
    pub span: Span,
}

// ── Statements ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Let {
        is_mut: bool,
        pattern: Pattern,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    /// Uninitialized binding: `let x: T;` or `let mut x: T;`.
    /// Type annotation is required (no RHS to infer from). Pattern is restricted
    /// to a single name — destructuring needs a value. Definite-assignment
    /// analysis (in `OwnershipChecker`) tracks initialization through later
    /// assignments before any read.
    LetUninit {
        is_mut: bool,
        name: String,
        name_span: Span,
        ty: TypeExpr,
    },
    LetElse {
        pattern: Pattern,
        ty: Option<TypeExpr>,
        value: Expr,
        else_block: Block,
    },
    Defer {
        body: Block,
    },
    ErrDefer {
        binding: Option<String>,
        body: Block,
    },
    Assign {
        target: Expr,
        value: Expr,
    },
    /// Parallel / destructuring assignment: `t1, t2, ... = v1, v2, ...;`.
    /// All `values` are evaluated (left-to-right) into temporaries before any
    /// `target` is written, so `a, b = b, a` swaps. `targets.len() ==
    /// values.len()` is enforced by the parser.
    ///
    /// This is surface syntax: the [`crate::desugar`] pass (between parse and
    /// resolve) rewrites every `MultiAssign` into a block-expr statement of
    /// `let`-temps + single `Assign`s, so no phase from the resolver onward
    /// ever observes it. It survives to the **formatter** only (which skips
    /// the desugar pass to round-trip source verbatim).
    MultiAssign {
        targets: Vec<Expr>,
        values: Vec<Expr>,
    },
    CompoundAssign {
        target: Expr,
        op: CompoundOp,
        value: Expr,
    },
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompoundOp {
    Add,    // +=
    Sub,    // -=
    Mul,    // *=
    Div,    // /=
    Mod,    // %=
    BitAnd, // &=
    BitOr,  // |=
    BitXor, // ^=
    Shl,    // <<=
    Shr,    // >>=
}
