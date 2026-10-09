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

/// What a [`ParRegion`] came from.
#[derive(Debug, Clone, PartialEq)]
pub enum ParKind {
    /// `par { a, b }` (one branch per expression) or the older
    /// `par { s1; s2; }` (one branch per statement).
    Block,
    /// `par for`, with its `limit` when it has one.
    For { limit: Option<Operand> },
}

/// The blocks of a `par` construct's branches, for the effect-conflict
/// check (C10). A `Block` region has one entry per branch; a `For` region
/// has one, the body of one iteration (the push onto the result `Vec` is
/// outside it). The blocks are those the builder opened while lowering the
/// branch, its exits included; drop elaboration adds blocks it does not
/// list.
#[derive(Debug, Clone, PartialEq)]
pub struct ParRegion {
    pub kind: ParKind,
    pub span: Span,
    pub branches: Vec<Vec<BasicBlock>>,
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
    /// The `par` regions, outer before inner (a region nested in another's
    /// branch comes after it).
    pub par_regions: Vec<ParRegion>,
    /// The function declares `suspends`. With the calls to suspending
    /// callees, this decides which bodies the coroutine transform rewrites
    /// (`coroutine::coroutines`).
    pub suspends: bool,
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
    /// A borrow of the static at this index in the program's statics:
    /// `ref T`, or `mut ref T` for a `let mut` binding.
    Static(u32),
}

/// A module binding that lives in one global place: its value is computed
/// once, by the body named `init` (no parameters, returning `ty`), before
/// `main` runs, in declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticDef {
    pub name: String,
    pub ty: Ty,
    /// `let mut`: its uses borrow it `mut ref`.
    pub mutable: bool,
    pub init: String,
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

/// A query about a type, answered from its layout (`mir::layout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullOp {
    /// `T.size_of()`: the bytes a value of `T` takes, padding included.
    SizeOf,
    /// `T.align_of()`.
    AlignOf,
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
    /// `u8 as char`: every byte is a Unicode scalar, so it cannot fail.
    /// Wider integers reach `char` through `char.try_from`.
    IntToChar,
    /// `c as u32` and other integer targets: the scalar value, wrapped to
    /// the target width.
    CharToInt,
    /// `b as i64`: 0 or 1.
    BoolToInt,
    /// A closure or function item, moved into an erased `Fn` value whose
    /// type forgets which one it is; or an `Fn` value widened to a kind
    /// that accepts it (`Fn` into `MutFn` or `OnceFn`, `MutFn` into
    /// `OnceFn`).
    Erase,
    /// `ref shared T` (or `ref weak T`) into a new `weak T` (core
    /// semantics §6.5). The operand is borrowed, so no strong count is
    /// taken and given back; copying a weak value is a downgrade of it.
    Downgrade,
    /// `ref weak T` into `Option[shared T]`: `Some` with a new handle,
    /// counted, while the object is alive, `None` after its last handle
    /// is dropped.
    Upgrade,
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
    /// A `usize` fact about a type.
    NullaryOp(NullOp, Ty),
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
    /// A run-time borrow flag on a `mut` field of a `shared` value (core
    /// semantics §6.2). Only the flag pass inserts these, after drop
    /// elaboration; the builder never does.
    BorrowFlag(FlagOp),
    Nop,
}

/// One borrow-flag operation. The flagged place is a `mut` field of a
/// `shared` value whose type is neither `Copy` nor a handle aggregate;
/// the flag belongs to the object, so two handles to it share one flag.
#[derive(Debug, Clone, PartialEq)]
pub enum FlagOp {
    /// Takes the flag for the loan made by the `Ref` at site `loan`:
    /// panics if `kind` is `Mut` and the flag is held at all, or if it is
    /// `Shared` and the flag is held mutably.
    Acquire {
        place: Place,
        kind: BorrowKind,
        loan: u32,
    },
    /// Gives back every flag the loan site `loan` holds in this frame, if
    /// any; the pass puts it where no local can hold the loan any more.
    Release { loan: u32 },
    /// An access that takes no borrow: a write or drop (`Mut`) panics if
    /// the flag is held at all, a read through the field (`Shared`) if it
    /// is held mutably.
    Check { place: Place, kind: BorrowKind },
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
        /// Where a panic in the callee goes.
        unwind: UnwindAction,
    },
    Drop {
        place: Place,
        target: BasicBlock,
        /// Where a panic in the drop body goes.
        unwind: UnwindAction,
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
            TerminatorKind::Goto { target }
            | TerminatorKind::Drop {
                target,
                unwind: UnwindAction::Abort,
                ..
            } => vec![*target],
            TerminatorKind::SwitchInt { targets, .. } => targets.all_targets().collect(),
            TerminatorKind::Call {
                target,
                unwind: UnwindAction::Abort,
                ..
            } => target.iter().copied().collect(),
            TerminatorKind::Return | TerminatorKind::Abort { .. } | TerminatorKind::Unreachable => {
                Vec::new()
            }
        }
    }
}

/// What a panic during a `Call` or `Drop` does (`docs/spikes/mir-types.md`
/// §1). v1 has one answer: a panic aborts the process and no drops run
/// (core semantics §7). The slot exists so that task-level recovery can add
/// a cleanup edge later without changing the shape of every terminator;
/// each pass matches on it exhaustively, so a new variant is a compile error
/// at every place that has to decide what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnwindAction {
    #[default]
    Abort,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BasicBlockData {
    pub statements: Vec<Statement>,
    pub terminator: Terminator,
}
