//! Statement and block printing for the canonical formatter.

use crate::ast::*;

impl super::Formatter {
    pub(super) fn format_block(&mut self, block: &Block) {
        let end = block.span.offset + block.span.length;
        if block.stmts.is_empty()
            && block.final_expr.is_none()
            && !self.has_comment_in(block.span.offset, end)
        {
            self.write_str("{}");
            return;
        }
        self.write_str("{\n");
        self.push_indent();
        for (i, stmt) in block.stmts.iter().enumerate() {
            self.flush_comments_before(stmt.span.offset);
            let last = i + 1 == block.stmts.len() && block.final_expr.is_none();
            match &stmt.kind {
                StmtKind::Expr(expr) if !block_stmt_needs_semicolon(expr, last) => {
                    self.write_indent();
                    self.stmt_start = true;
                    self.format_expr(expr);
                    self.output.push('\n');
                }
                _ => self.format_stmt(stmt),
            }
        }
        if let Some(ref expr) = block.final_expr {
            self.flush_comments_before(expr.span.offset);
            self.write_indent();
            self.stmt_start = true;
            self.format_expr(expr);
            self.output.push('\n');
        }
        self.flush_comments_before(end);
        self.pop_indent();
        self.write_indent();
        self.write_str("}");
    }

    // ── Statements ──────────────────────────────────────────────

    pub(super) fn format_stmt(&mut self, stmt: &Stmt) {
        let key = crate::resolver::SpanKey::from_span(&stmt.span);
        if let Some(overrides) = self.stmt_lint_overrides.get(&key).cloned() {
            for o in overrides {
                self.write_indent();
                self.write_str("#[");
                self.write_str(o.level.as_attr_name());
                self.write_str("(");
                self.write_str(&o.lint);
                self.write_str(")]\n");
            }
        }
        match &stmt.kind {
            StmtKind::Let {
                is_mut,
                pattern,
                ty,
                value,
            } => {
                self.write_indent();
                self.write_str("let ");
                if *is_mut {
                    self.write_str("mut ");
                }
                self.format_pattern(pattern);
                if let Some(ref t) = ty {
                    self.write_str(": ");
                    self.format_type_expr(t);
                }
                self.write_str(" = ");
                // `freeze <place>` (B-2026-08-01-33 stage 3) — the keyword is
                // recorded on `Program::freeze_spans`, not inside the
                // expression, so it has to be written back here. Dropping it
                // would silently rewrite a non-counting frozen binding into an
                // ordinary counted one.
                if self
                    .freeze_spans
                    .contains(&crate::resolver::SpanKey::from_span(&value.span))
                {
                    self.write_str("freeze ");
                }
                self.format_expr(value);
                self.write_str(";\n");
            }
            StmtKind::LetUninit {
                is_mut, name, ty, ..
            } => {
                self.write_indent();
                self.write_str("let ");
                if *is_mut {
                    self.write_str("mut ");
                }
                self.write_ident(name);
                self.write_str(": ");
                self.format_type_expr(ty);
                self.write_str(";\n");
            }
            StmtKind::LetElse {
                pattern,
                ty,
                value,
                else_block,
            } => {
                self.write_indent();
                self.write_str("let ");
                self.format_pattern(pattern);
                if let Some(ref t) = ty {
                    self.write_str(": ");
                    self.format_type_expr(t);
                }
                self.write_str(" = ");
                self.format_expr(value);
                self.write_str(" else ");
                self.format_block(else_block);
                self.write_str(";\n");
            }
            StmtKind::Defer { body } => {
                self.write_indent();
                self.write_str("defer ");
                self.format_block(body);
                self.output.push('\n');
            }
            StmtKind::ErrDefer { binding, body } => {
                self.write_indent();
                self.write_str("errdefer");
                if let Some(ref b) = binding {
                    self.write_str("(");
                    self.write_ident(b);
                    self.write_str(")");
                }
                self.write_str(" ");
                self.format_block(body);
                self.output.push('\n');
            }
            StmtKind::Assign { target, value } => {
                self.write_indent();
                self.format_expr(target);
                self.write_str(" = ");
                self.format_expr(value);
                self.write_str(";\n");
            }
            StmtKind::MultiAssign { targets, values } => {
                self.write_indent();
                for (i, target) in targets.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.format_expr(target);
                }
                self.write_str(" = ");
                for (i, value) in values.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.format_expr(value);
                }
                self.write_str(";\n");
            }
            StmtKind::CompoundAssign { target, op, value } => {
                self.write_indent();
                self.format_expr(target);
                self.write_str(match op {
                    CompoundOp::Add => " += ",
                    CompoundOp::Sub => " -= ",
                    CompoundOp::Mul => " *= ",
                    CompoundOp::Div => " /= ",
                    CompoundOp::Mod => " %= ",
                    CompoundOp::BitAnd => " &= ",
                    CompoundOp::BitOr => " |= ",
                    CompoundOp::BitXor => " ^= ",
                    CompoundOp::Shl => " <<= ",
                    CompoundOp::Shr => " >>= ",
                });
                self.format_expr(value);
                self.write_str(";\n");
            }
            StmtKind::Expr(expr) => {
                self.write_indent();
                self.stmt_start = true;
                self.format_expr(expr);
                self.write_str(";\n");
            }
        }
    }
}

/// Whether an expression statement is printed with its `;`. A block-like
/// expression (`if`, `match`, `for`, ...) ends its statement at its `}`, so
/// the parser takes it with or without one and builds the same tree. The
/// exception is a block's last statement: without the `;`, anything but
/// `while` / `for` would become the block's value.
fn block_stmt_needs_semicolon(expr: &Expr, last: bool) -> bool {
    if matches!(
        expr.kind,
        ExprKind::While { .. } | ExprKind::WhileLet { .. } | ExprKind::For { .. }
    ) {
        return false;
    }
    last || !crate::parser::Parser::is_block_like_prefix(expr)
}
