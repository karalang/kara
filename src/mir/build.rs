//! A small API for constructing MIR bodies block by block.
//!
//! The HIR-to-MIR builder sits on top of this; tests use it directly.

use crate::ids::NodeId;

use super::syntax::*;
use super::ty::Ty;

pub struct BodyBuilder {
    instance: InstanceId,
    locals: Vec<LocalDecl>,
    arg_count: usize,
    blocks: Vec<(Vec<Statement>, Option<Terminator>)>,
    scopes: Vec<SourceScopeData>,
    info: SourceInfo,
    /// See [`Body::par_regions`].
    pub par_regions: Vec<ParRegion>,
    /// See [`Body::suspends`].
    pub suspends: bool,
}

impl BodyBuilder {
    /// The instance this body is for.
    pub fn instance(&self) -> &InstanceId {
        &self.instance
    }

    /// Starts a body returning `ret`. Add every parameter with
    /// [`Self::arg`] before any other local.
    pub fn new(instance: InstanceId, ret: Ty) -> Self {
        let info = SourceInfo::dummy();
        BodyBuilder {
            instance,
            locals: vec![LocalDecl {
                ty: ret,
                mutability: Mutability::Mut,
                kind: LocalKind::ReturnPlace,
                source_info: info,
            }],
            arg_count: 0,
            blocks: Vec::new(),
            scopes: vec![SourceScopeData {
                parent: None,
                span: info.span,
            }],
            info,
            par_regions: Vec::new(),
            suspends: false,
        }
    }

    /// The source info stamped on everything pushed from now on.
    pub fn set_source_info(&mut self, info: SourceInfo) {
        self.info = info;
    }

    pub fn arg(&mut self, name: &str, ty: Ty) -> Local {
        assert_eq!(
            self.locals.len(),
            self.arg_count + 1,
            "parameters must be declared before other locals"
        );
        self.arg_count += 1;
        self.push_local(
            ty,
            Mutability::Not,
            LocalKind::Arg {
                name: name.to_string(),
                node: NodeId::DUMMY,
            },
        )
    }

    pub fn user_local(&mut self, name: &str, ty: Ty, mutability: Mutability) -> Local {
        self.push_local(
            ty,
            mutability,
            LocalKind::User {
                name: name.to_string(),
                node: NodeId::DUMMY,
            },
        )
    }

    pub fn temp(&mut self, ty: Ty) -> Local {
        self.push_local(ty, Mutability::Mut, LocalKind::Temp)
    }

    pub fn push_local(&mut self, ty: Ty, mutability: Mutability, kind: LocalKind) -> Local {
        let l = Local(self.locals.len() as u32);
        self.locals.push(LocalDecl {
            ty,
            mutability,
            kind,
            source_info: self.info,
        });
        l
    }

    /// A user binding's name and pattern node.
    pub fn user_name(&self, l: Local) -> Option<(String, NodeId)> {
        match &self.locals[l.index()].kind {
            LocalKind::User { name, node } => Some((name.clone(), *node)),
            _ => None,
        }
    }

    pub fn is_temp(&self, l: Local) -> bool {
        matches!(self.locals[l.index()].kind, LocalKind::Temp)
    }

    pub fn local_ty(&self, l: Local) -> Ty {
        self.locals[l.index()].ty
    }

    pub fn set_local_ty(&mut self, l: Local, t: Ty) {
        self.locals[l.index()].ty = t;
    }

    /// How many blocks the body has so far.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn new_block(&mut self) -> BasicBlock {
        let b = BasicBlock(self.blocks.len() as u32);
        self.blocks.push((Vec::new(), None));
        b
    }

    pub fn push(&mut self, bb: BasicBlock, kind: StatementKind) {
        let slot = &mut self.blocks[bb.index()];
        assert!(slot.1.is_none(), "{bb} is already terminated");
        slot.0.push(Statement {
            kind,
            source_info: self.info,
        });
    }

    pub fn assign(&mut self, bb: BasicBlock, place: impl Into<Place>, rv: Rvalue) {
        self.push(bb, StatementKind::Assign(place.into(), rv));
    }

    pub fn terminate(&mut self, bb: BasicBlock, kind: TerminatorKind) {
        let slot = &mut self.blocks[bb.index()];
        assert!(slot.1.is_none(), "{bb} is already terminated");
        slot.1 = Some(Terminator {
            kind,
            source_info: self.info,
        });
    }

    /// End every block that has no terminator yet with `kind`: the dead
    /// blocks a builder opens after a `return` or `break`.
    pub fn terminate_open_blocks(&mut self, kind: TerminatorKind) {
        for slot in &mut self.blocks {
            if slot.1.is_none() {
                slot.1 = Some(Terminator {
                    kind: kind.clone(),
                    source_info: self.info,
                });
            }
        }
    }

    /// Finishes the body in the `Built` phase. Fails if a block was left
    /// without a terminator.
    pub fn finish(self) -> Result<Body, String> {
        let mut blocks = Vec::with_capacity(self.blocks.len());
        for (i, (statements, term)) in self.blocks.into_iter().enumerate() {
            let terminator = term.ok_or_else(|| format!("bb{i} has no terminator"))?;
            blocks.push(BasicBlockData {
                statements,
                terminator,
            });
        }
        if blocks.is_empty() {
            return Err("a body needs at least one block".into());
        }
        Ok(Body {
            instance: self.instance,
            locals: self.locals,
            arg_count: self.arg_count,
            blocks,
            scopes: self.scopes,
            phase: MirPhase::Built,
            span: self.info.span,
            par_regions: self.par_regions,
            suspends: self.suspends,
        })
    }
}
