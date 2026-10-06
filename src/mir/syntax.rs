//! The MIR data types, as specified in `docs/spikes/mir-types.md` §2.

use crate::ids::{DefId, NodeId};
use crate::token::Span;

use super::ty::Ty;

macro_rules! index_type {
    ($(#[$m:meta])* $name:ident, $prefix:literal) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl $name {
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }
    };
}

index_type!(
    /// A local: `_0` is the return place, `_1..=arg_count` the parameters.
    Local,
    "_"
);
index_type!(
    /// A basic block; `bb0` is the entry.
    BasicBlock,
    "bb"
);
index_type!(FieldIdx, "");
index_type!(VariantIdx, "");
index_type!(
    /// A lexical scope, for names and debug info only.
    SourceScope,
    "scope"
);

impl Local {
    pub const RETURN_PLACE: Local = Local(0);
}

/// Identifies the monomorphic function instance a body implements.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstanceId {
    pub def: DefId,
    pub args: Vec<Ty>,
    /// Display name, e.g. `eat` or `Vec[R].push`.
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirPhase {
    Built,
    Checked,
    DropsElaborated,
    Optimized,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceInfo {
    pub span: Span,
    pub scope: SourceScope,
}

impl SourceInfo {
    pub fn dummy() -> Self {
        SourceInfo {
            span: Span {
                line: 0,
                column: 0,
                offset: 0,
                length: 0,
            },
            scope: SourceScope(0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceScopeData {
    pub parent: Option<SourceScope>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    pub instance: InstanceId,
    pub locals: Vec<LocalDecl>,
    pub arg_count: usize,
    pub blocks: Vec<BasicBlockData>,
    pub scopes: Vec<SourceScopeData>,
    pub phase: MirPhase,
    pub span: Span,
}

impl Body {
    pub fn local(&self, l: Local) -> &LocalDecl {
        &self.locals[l.index()]
    }

    pub fn block(&self, b: BasicBlock) -> &BasicBlockData {
        &self.blocks[b.index()]
    }

    pub fn args(&self) -> impl Iterator<Item = Local> {
        (1..=self.arg_count as u32).map(Local)
    }

    pub fn return_ty(&self) -> Ty {
        self.locals[0].ty
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutability {
    Not,
    Mut,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalDecl {
    pub ty: Ty,
    pub mutability: Mutability,
    pub kind: LocalKind,
    pub source_info: SourceInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LocalKind {
    ReturnPlace,
    Arg {
        name: String,
        node: NodeId,
    },
    /// A `let` or pattern binding.
    User {
        name: String,
        node: NodeId,
    },
    Temp,
    /// A `bool` introduced by drop elaboration.
    DropFlag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjElem {
    /// A struct field or tuple element, with the field's type.
    Field(FieldIdx, Ty),
    /// View an enum place as one variant; precedes `Field`.
    Downcast(VariantIdx),
    Deref,
    /// An array or slice element at a runtime index (a `usize` local).
    Index(Local),
    /// An array element at a constant index.
    ConstIndex(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Place {
    pub local: Local,
    pub projection: Vec<ProjElem>,
}

impl Place {
    pub fn local(l: Local) -> Place {
        Place {
            local: l,
            projection: Vec::new(),
        }
    }

    pub fn project(&self, e: ProjElem) -> Place {
        let mut p = self.clone();
        p.projection.push(e);
        p
    }

    pub fn field(&self, f: u32, ty: Ty) -> Place {
        self.project(ProjElem::Field(FieldIdx(f), ty))
    }

    /// Does any projection forbid moving out (core semantics §3.7)?
    pub fn is_move_forbidden(&self) -> bool {
        self.projection.iter().any(|e| {
            matches!(
                e,
                ProjElem::Deref | ProjElem::Index(_) | ProjElem::ConstIndex(_)
            )
        })
    }

    /// Is `self` a prefix of `other` (equal counts)?
    pub fn is_prefix_of(&self, other: &Place) -> bool {
        self.local == other.local
            && self.projection.len() <= other.projection.len()
            && other.projection[..self.projection.len()] == self.projection[..]
    }
}

impl From<Local> for Place {
    fn from(l: Local) -> Place {
        Place::local(l)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    /// The type is `Copy`; the source stays initialized.
    Copy(Place),
    /// The source becomes uninitialized.
    Move(Place),
    Const(Const),
}

impl Operand {
    pub fn place(&self) -> Option<&Place> {
        match self {
            Operand::Copy(p) | Operand::Move(p) => Some(p),
            Operand::Const(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Const {
    pub ty: Ty,
    pub kind: ConstKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstKind {
    /// Integers, `bool` (0/1) and `char` (scalar value); width from `ty`.
    Scalar(u128),
    Float(u64),
    Str(String),
    Unit,
    /// A function item: the callee of a direct `Call`.
    FnDef(InstanceId),
    ZeroSized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowKind {
    Shared,
    Mut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn name(self) -> &'static str {
        match self {
            BinOp::Add => "Add",
            BinOp::Sub => "Sub",
            BinOp::Mul => "Mul",
            BinOp::Div => "Div",
            BinOp::Rem => "Rem",
            BinOp::BitAnd => "BitAnd",
            BinOp::BitOr => "BitOr",
            BinOp::BitXor => "BitXor",
            BinOp::Shl => "Shl",
            BinOp::Shr => "Shr",
            BinOp::Eq => "Eq",
            BinOp::Ne => "Ne",
            BinOp::Lt => "Lt",
            BinOp::Le => "Le",
            BinOp::Gt => "Gt",
            BinOp::Ge => "Ge",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastKind {
    IntToInt,
    IntToFloat,
    FloatToInt,
    FloatToFloat,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AggregateKind {
    Tuple,
    /// Element type.
    Array(Ty),
    /// A struct (variant 0) or one enum variant; `ty` is the ADT type.
    Adt {
        ty: Ty,
        variant: VariantIdx,
    },
    /// Allocates a counted box with count 1; `ty` is the shared type.
    Shared {
        ty: Ty,
        variant: VariantIdx,
    },
    /// The captures, in capture order; `ty` is the closure type.
    Closure {
        ty: Ty,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Rvalue {
    Use(Operand),
    Ref(BorrowKind, Place),
    /// A new handle to a `shared` value; count + 1.
    Retain(Place),
    BinaryOp(BinOp, Operand, Operand),
    /// Yields `(result, overflowed: bool)`.
    CheckedBinaryOp(BinOp, Operand, Operand),
    UnaryOp(UnOp, Operand),
    Cast(CastKind, Operand, Ty),
    Discriminant(Place),
    Len(Place),
    Aggregate(AggregateKind, Vec<Operand>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub kind: StatementKind,
    pub source_info: SourceInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatementKind {
    /// Does not drop the destination's old value: the builder emits an
    /// explicit `Drop` first (core semantics §7.5).
    Assign(Place, Rvalue),
    StorageLive(Local),
    StorageDead(Local),
    SetDiscriminant(Place, VariantIdx),
    Nop,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwitchTargets {
    pub values: Vec<(u128, BasicBlock)>,
    pub otherwise: BasicBlock,
}

impl SwitchTargets {
    pub fn if_else(then_bb: BasicBlock, else_bb: BasicBlock) -> Self {
        // `bool` is 0/1: 0 is false.
        SwitchTargets {
            values: vec![(0, else_bb)],
            otherwise: then_bb,
        }
    }

    pub fn all_targets(&self) -> impl Iterator<Item = BasicBlock> + '_ {
        self.values
            .iter()
            .map(|(_, b)| *b)
            .chain(std::iter::once(self.otherwise))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortReason {
    Panic,
    Overflow,
    DivByZero,
    BoundsCheck,
    UnreachableArm,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Terminator {
    pub kind: TerminatorKind,
    pub source_info: SourceInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TerminatorKind {
    Goto {
        target: BasicBlock,
    },
    SwitchInt {
        discr: Operand,
        targets: SwitchTargets,
    },
    Call {
        func: Operand,
        /// By-value arguments are `Move`: the callee owns them (§4.2).
        args: Vec<Operand>,
        destination: Place,
        /// `None` when the callee never returns.
        target: Option<BasicBlock>,
    },
    Drop {
        place: Place,
        target: BasicBlock,
    },
    Return,
    Abort {
        reason: AbortReason,
    },
    Unreachable,
}

impl TerminatorKind {
    pub fn successors(&self) -> Vec<BasicBlock> {
        match self {
            TerminatorKind::Goto { target } | TerminatorKind::Drop { target, .. } => {
                vec![*target]
            }
            TerminatorKind::SwitchInt { targets, .. } => targets.all_targets().collect(),
            TerminatorKind::Call { target, .. } => target.iter().copied().collect(),
            TerminatorKind::Return | TerminatorKind::Abort { .. } | TerminatorKind::Unreachable => {
                Vec::new()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BasicBlockData {
    pub statements: Vec<Statement>,
    pub terminator: Terminator,
}
