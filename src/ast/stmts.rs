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
