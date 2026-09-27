//! Builders that keep structural invariants while an IR module is built.
//!
//! The builders exist so that a frontend cannot construct a malformed module by
//! accident: a block must be terminated before another is started, instructions
//! cannot follow a terminator, value identifiers are dense and function-local,
//! and a function cannot be finished while it is missing a terminator.

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use lazalith_types::SourceSpan;

use crate::{
    Block, BlockId, DataSegment, Function, Instruction, IrError, IrErrorKind, Linkage, Module,
    Name, Parameter, SourceEntry, Terminator, Type, ValueId,
};

/// A block under construction.
///
/// A block is a straight-line instruction list plus exactly one terminator. The
/// builder keeps the two halves separate so an instruction cannot be appended
/// after the block has ended.
#[derive(Debug)]
pub struct BlockBuilder {
    /// The block label.
    pub id: BlockId,
    /// The block name.
    pub name: Name,
    /// Instructions in execution order.
    pub instructions: Vec<Instruction>,
    /// The terminator, once the block is closed.
    pub terminator: Option<Terminator>,
}

impl BlockBuilder {
    /// Creates an empty block.
    pub fn new(id: BlockId, name: Name) -> Self {
        Self {
            id,
            name,
            instructions: Vec::new(),
            terminator: None,
        }
    }
}

/// A function under construction.
#[derive(Debug)]
pub struct FunctionBuilder {
    name: Name,
    linkage: Linkage,
    params: Vec<Parameter>,
    result: Type,
    blocks: Vec<BlockBuilder>,
    /// Blocks that have an identifier but have not been switched to yet.
    ///
    /// A reserved block waits here rather than in `blocks` so that `blocks` stays
    /// in the order the front end emitted into it. That order is what numbers a
    /// function's values — the identifiers are dense and follow the blocks — so
    /// it has to be the emission order and not the order the blocks happened to be
    /// reserved in. A loop reserves its exit before its body, and the body's
    /// blocks are emitted first.
    reserved: Vec<BlockBuilder>,
    /// The next block identifier to hand out.
    next_block: u32,
    current: Option<usize>,
    next_value: u32,
    span: Option<SourceSpan>,
    param_values: Vec<ValueId>,
    /// The span to record against the next instruction pushed.
    pending_source: Option<SourceSpan>,
    /// The source map built so far, in push order.
    source_map: Vec<SourceEntry>,
}

impl FunctionBuilder {
    fn new(
        name: Name,
        linkage: Linkage,
        params: Vec<Parameter>,
        result: Type,
        span: Option<SourceSpan>,
    ) -> Result<Self, IrError> {
        let mut param_values = Vec::new();
        param_values
            .try_reserve_exact(params.len())
            .map_err(|_| IrError::new(IrErrorKind::Allocation))?;
        for index in 0..params.len() {
            param_values.push(ValueId::new(index as u32).ok_or_else(|| {
                IrError::new(IrErrorKind::InvalidBuilderState {
                    detail: String::from("parameter value identifier out of range"),
                })
            })?);
        }
        Ok(Self {
            next_value: params.len() as u32,
            name,
            linkage,
            params,
            result,
            blocks: Vec::new(),
            reserved: Vec::new(),
            next_block: 0,
            current: None,
            span,
            param_values,
            pending_source: None,
            source_map: Vec::new(),
        })
    }

    /// The function name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The value identifier bound to a parameter, by parameter index.
    pub fn param_value(&self, index: usize) -> Option<ValueId> {
        self.param_values.get(index).copied()
    }

    /// The identifier the next [`FunctionBuilder::emit`] will use, without
    /// allocating it. Parameters occupy the identifiers before the first
    /// emitted instruction.
    pub fn peek_next_value(&self) -> Result<ValueId, IrError> {
        let raw = self.next_value.checked_add(1).ok_or(IrError::new(
            IrErrorKind::InvalidBuilderState {
                detail: String::from("value identifier space exhausted"),
            },
        ))?;
        ValueId::new(raw - 1).ok_or_else(|| {
            IrError::new(IrErrorKind::InvalidBuilderState {
                detail: String::from("value identifier out of range"),
            })
        })
    }

    /// Reserves a block, creating it empty and returning its identifier.
    ///
    /// A loop or a conditional has to *name* a block it has not built yet: the
    /// branch that leaves a loop body goes to a block that comes after it. Working
    /// out in advance which number that block will get only works if nothing else
    /// creates a block in between, and a body containing another `if`, or a
    /// short-circuiting `&&`, creates blocks of its own — so a predicted number
    /// silently becomes the wrong block.
    ///
    /// Reserving the block makes the identifier real instead of predicted. The
    /// block is created empty and is *not* made current; the front end fills it
    /// later with [`switch_to_block`](Self::switch_to_block), which finds it by
    /// name. A name that is already reserved returns the identifier it already
    /// has, so reserving twice is harmless.
    pub fn reserve_block(&mut self, name: &str) -> Result<BlockId, IrError> {
        if let Some(block) = self.blocks.iter().find(|block| block.name == name) {
            return Ok(block.id);
        }
        if let Some(block) = self.reserved.iter().find(|block| block.name == name) {
            return Ok(block.id);
        }
        let id = self.take_block_id()?;
        self.reserved
            .try_reserve(1)
            .map_err(|_| IrError::new(IrErrorKind::Allocation))?;
        self.reserved.push(BlockBuilder::new(id, name.to_string()));
        Ok(id)
    }

    /// The next unused block identifier.
    fn take_block_id(&mut self) -> Result<BlockId, IrError> {
        let raw = self.next_block;
        self.next_block = self
            .next_block
            .checked_add(1)
            .ok_or_else(|| IrError::new(IrErrorKind::Allocation))?;
        BlockId::new(raw).ok_or_else(|| {
            self.error(IrErrorKind::InvalidBuilderState {
                detail: String::from("block label out of range"),
            })
        })
    }

    /// Switches to a block by name, creating it if it does not exist.
    ///
    /// Switching back to a block that already exists is always allowed, so a
    /// front end can close a loop after emitting its body. Creating a *new*
    /// block requires the current block to be terminated first, because a
    /// front end that leaves a block open has not decided where control flow
    /// goes.
    ///
    /// A block that was [reserved](Self::reserve_block) keeps the identifier it
    /// was given and joins `blocks` here, in the order the front end fills it in.
    pub fn switch_to_block(&mut self, name: &str) -> Result<BlockId, IrError> {
        if let Some(index) = self.blocks.iter().position(|block| block.name == name) {
            self.current = Some(index);
            return Ok(self.blocks[index].id);
        }
        if let Some(index) = self.current
            && self.blocks[index].terminator.is_none()
        {
            return Err(self
                .error(IrErrorKind::InvalidBuilderState {
                    detail: String::from("cannot create a new block before terminating one"),
                })
                .with_block(self.blocks[index].id));
        }
        // A reserved block already has its identifier; only a block nobody has
        // thought of yet needs one.
        let block = match self.reserved.iter().position(|block| block.name == name) {
            Some(index) => self.reserved.remove(index),
            None => BlockBuilder::new(self.take_block_id()?, name.to_string()),
        };
        let block_id = block.id;
        self.blocks
            .try_reserve(1)
            .map_err(|_| IrError::new(IrErrorKind::Allocation))?;
        self.blocks.push(block);
        self.current = Some(self.blocks.len() - 1);
        Ok(block_id)
    }

    /// Emits an instruction that produces a value.
    pub fn emit(&mut self, instruction: Instruction) -> Result<ValueId, IrError> {
        let value = self.peek_next_value()?;
        self.push(instruction)?;
        self.bump(value);
        Ok(value)
    }

    /// Emits an instruction that produces no value.
    pub fn emit_effect(&mut self, instruction: Instruction) -> Result<(), IrError> {
        self.push(instruction)
    }

    /// Records that the next instruction pushed came from `span`.
    ///
    /// The mark applies to *every* instruction pushed until the next mark, so a
    /// front end calls this once per statement rather than once per instruction.
    /// That is the granularity a debugger wants — a PC inside a thirty-instruction
    /// statement should name the statement — and it keeps the map the size of the
    /// source rather than the size of the program.
    ///
    /// Two marks on the same span in the same block record one entry, because the
    /// second would resolve to the same answer as the first and a table of
    /// duplicates is a table that has to be searched.
    pub fn mark(&mut self, span: SourceSpan) {
        self.pending_source = Some(span);
    }

    fn push(&mut self, instruction: Instruction) -> Result<(), IrError> {
        let index = self.current.ok_or_else(|| {
            self.error(IrErrorKind::InvalidBuilderState {
                detail: String::from("no block is selected"),
            })
        })?;
        if self.blocks[index].terminator.is_some() {
            return Err(self
                .error(IrErrorKind::InvalidBuilderState {
                    detail: String::from("instruction follows a terminator"),
                })
                .with_block(self.blocks[index].id));
        }
        self.blocks[index].instructions.push(instruction);
        if let Some(span) = self.pending_source.take() {
            let at = self.blocks[index].instructions.len() - 1;
            // A mark records the *start* of a run. If the previous entry is the
            // instruction immediately before this one and names the same span,
            // this is the same run continuing and a second entry would resolve to
            // the same answer.
            let continues = self.source_map.last().is_some_and(|entry| {
                entry.block == index
                    && entry.instruction + 1 == at
                    && entry.span.id() == span.id()
                    && entry.span.start().as_u32() == span.start().as_u32()
                    && entry.span.end().as_u32() == span.end().as_u32()
            });
            if !continues {
                self.source_map.push(SourceEntry {
                    block: index,
                    instruction: at,
                    span,
                });
            }
        }
        Ok(())
    }

    fn bump(&mut self, value: ValueId) {
        self.next_value = self.next_value.max(value.get() + 1);
    }

    /// Terminates the current block.
    pub fn terminate(&mut self, terminator: Terminator) -> Result<(), IrError> {
        let index = self.current.ok_or_else(|| {
            self.error(IrErrorKind::InvalidBuilderState {
                detail: String::from("no block is selected"),
            })
        })?;
        if self.blocks[index].terminator.is_some() {
            return Err(self
                .error(IrErrorKind::InvalidBuilderState {
                    detail: String::from("block is already terminated"),
                })
                .with_block(self.blocks[index].id));
        }
        self.blocks[index].terminator = Some(terminator);
        Ok(())
    }

    /// Finishes the function, checking that every block is terminated.
    pub fn finish(self) -> Result<Function, IrError> {
        if self.blocks.is_empty() {
            return Err(self.error(IrErrorKind::EmptyFunction));
        }
        let name = self.name.clone();
        // A block that was reserved and never filled in is a block some branch
        // names and no code reaches. Dropping it silently would turn that branch
        // into a jump to nothing, so it is reported here instead.
        if let Some(block) = self.reserved.first() {
            return Err(IrError::new(IrErrorKind::InvalidBuilderState {
                detail: format!("the reserved block `{}` was never filled in", block.name),
            })
            .in_function(&name)
            .with_block(block.id));
        }
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(self.blocks.len())
            .map_err(|_| IrError::new(IrErrorKind::Allocation))?;
        for builder in self.blocks {
            let terminator = builder.terminator.ok_or_else(|| {
                IrError::new(IrErrorKind::MissingTerminator {
                    block: builder.id.get(),
                })
                .in_function(&name)
            })?;
            blocks.push(Block {
                id: builder.id,
                name: builder.name,
                instructions: builder.instructions,
                terminator,
            });
        }
        Ok(Function {
            name: self.name,
            linkage: self.linkage,
            params: self.params,
            result: self.result,
            blocks,
            span: self.span,
            source_map: self.source_map,
        })
    }

    fn error(&self, kind: IrErrorKind) -> IrError {
        IrError::new(kind).in_function(&self.name)
    }
}

/// A module under construction.
#[derive(Debug)]
pub struct ModuleBuilder {
    module: Module,
    /// The span to give the next function started, if one was recorded.
    pending_span: Option<SourceSpan>,
}

impl ModuleBuilder {
    /// Creates a module with the given name.
    pub fn new(name: &str) -> Self {
        Self {
            module: Module {
                name: String::from(name),
                functions: Vec::new(),
                data: Vec::new(),
            },
            pending_span: None,
        }
    }

    /// Starts a function. The caller must call `finish_function` with the
    /// returned builder before starting another one.
    pub fn function(
        &mut self,
        name: &str,
        linkage: Linkage,
        params: Vec<Parameter>,
        result: Type,
    ) -> Result<FunctionBuilder, IrError> {
        if self
            .module
            .functions
            .iter()
            .any(|function| function.name == name)
        {
            return Err(IrError::new(IrErrorKind::RedefinedFunction {
                name: String::from(name),
            }));
        }
        // A span recorded before `function` applies to the next function started and
        // to no other: a span that leaked from one function to the next would put
        // a diagnostic in the wrong place, which is worse than none.
        let span = self.pending_span.take();
        FunctionBuilder::new(String::from(name), linkage, params, result, span)
    }

    /// Records the source span for the next function started.
    ///
    /// This used to do nothing at all, so every function in a module reported no
    /// span and a diagnostic could not say where a function came from.
    pub fn set_function_span(&mut self, span: SourceSpan) {
        self.pending_span = Some(span);
    }

    /// Adds a completed function.
    pub fn add_function(&mut self, function: Function) -> Result<(), IrError> {
        if self
            .module
            .functions
            .iter()
            .any(|existing| existing.name == function.name)
        {
            return Err(IrError::new(IrErrorKind::RedefinedFunction {
                name: function.name.clone(),
            }));
        }
        self.module
            .functions
            .try_reserve(1)
            .map_err(|_| IrError::new(IrErrorKind::Allocation))?;
        self.module.functions.push(function);
        Ok(())
    }

    /// Adds a data segment.
    pub fn add_data(&mut self, segment: DataSegment) -> Result<(), IrError> {
        if self
            .module
            .data
            .iter()
            .any(|data| data.name == segment.name)
        {
            return Err(IrError::new(IrErrorKind::RedefinedFunction {
                name: segment.name,
            }));
        }
        self.module
            .data
            .try_reserve(1)
            .map_err(|_| IrError::new(IrErrorKind::Allocation))?;
        self.module.data.push(segment);
        Ok(())
    }

    /// Finishes the module and runs the verifier over it.
    pub fn finish(self) -> Result<Module, IrError> {
        crate::verify_module(&self.module)?;
        Ok(self.module)
    }
}
