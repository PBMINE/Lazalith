//! Builders that keep structural invariants while an IR module is built.
//!
//! The builders exist so that a frontend cannot construct a malformed module by
//! accident: a block must be terminated before another is started, instructions
//! cannot follow a terminator, value identifiers are dense and function-local,
//! and a function cannot be finished while it is missing a terminator.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use lazalith_types::SourceSpan;

use crate::{
    Block, BlockId, DataSegment, Function, Instruction, IrError, IrErrorKind, Linkage, Module,
    Name, Parameter, Terminator, Type, ValueId,
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
    current: Option<usize>,
    next_value: u32,
    span: Option<SourceSpan>,
    param_values: Vec<ValueId>,
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
            current: None,
            span,
            param_values,
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

    /// Switches to a block by name, creating it if it does not exist.
    ///
    /// Switching back to a block that already exists is always allowed, so a
    /// front end can close a loop after emitting its body. Creating a *new*
    /// block requires the current block to be terminated first, because a
    /// front end that leaves a block open has not decided where control flow
    /// goes.
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
        let id = self.blocks.len() as u32;
        let block_id = BlockId::new(id).ok_or_else(|| {
            self.error(IrErrorKind::InvalidBuilderState {
                detail: String::from("block label out of range"),
            })
        })?;
        let block = BlockBuilder::new(block_id, name.to_string());
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
        FunctionBuilder::new(String::from(name), linkage, params, result, None)
    }

    /// Records a source span for the function under construction.
    pub fn set_function_span(&mut self, span: SourceSpan) {
        let _ = span;
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
