//! The IR verifier.
//!
//! The verifier is the gate between a frontend and code generation. It answers
//! one question: is this module well formed enough to generate code from? It
//! checks structure, uniqueness, dominance, types of operands, aggregate
//! offsets, and call arity. It does not optimize and it does not repair.
//!
//! A frontend that fails here gets an [`IrError`] naming the function, the
//! block where possible, and the source span where the frontend recorded one.

use alloc::{collections::BTreeMap, vec::Vec};

use crate::{
    BlockId, CallArg, CallTarget, Function, Instruction, IrError, IrErrorKind, Module, ReturnValue,
    Terminator, Type, ValueId,
};

/// Verifies a module and every function in it.
pub fn verify_module(module: &Module) -> Result<(), IrError> {
    let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
    for function in &module.functions {
        if seen.insert(function.name.as_str(), ()).is_some() {
            return Err(IrError::new(IrErrorKind::RedefinedFunction {
                name: function.name.clone(),
            })
            .in_function(&function.name));
        }
    }
    for segment in &module.data {
        if segment.alignment == 0 || !segment.alignment.is_power_of_two() {
            return Err(IrError::new(IrErrorKind::InvalidType {
                detail: alloc::format!(
                    "data segment {} has alignment {}",
                    segment.name,
                    segment.alignment
                ),
            }));
        }
    }
    for function in &module.functions {
        verify_function(module, function)?;
    }
    Ok(())
}

fn verify_function(module: &Module, function: &Function) -> Result<(), IrError> {
    if function.blocks.is_empty() {
        return Err(IrError::new(IrErrorKind::EmptyFunction).in_function(&function.name));
    }
    let result_size = function.result.size_in_bytes();
    if function.result.alignment_in_bytes() == 0 {
        return Err(IrError::new(IrErrorKind::InvalidType {
            detail: alloc::format!("result of {} has zero alignment", function.name),
        })
        .in_function(&function.name));
    }

    let mut block_ids: BTreeMap<u32, ()> = BTreeMap::new();
    for block in &function.blocks {
        if block_ids.insert(block.id.get(), ()).is_some() {
            return Err(IrError::new(IrErrorKind::RedefinedBlock {
                block: block.id.get(),
            })
            .in_function(&function.name));
        }
    }

    // Every instruction and terminator operand must name a defined value, and
    // every defined value must be used in a dominated block.
    let mut defined: BTreeMap<u32, BlockId> = BTreeMap::new();
    for (index, parameter) in function.params.iter().enumerate() {
        let raw = index as u32;
        defined.insert(raw, function.blocks[0].id);
        if parameter.ty.size_in_bytes().is_none() {
            return Err(IrError::new(IrErrorKind::InvalidType {
                detail: alloc::format!("parameter {} has no size", parameter.name),
            })
            .in_function(&function.name));
        }
    }
    for block in &function.blocks {
        let mut next = defined.len() as u32;
        for instruction in &block.instructions {
            check_operands(function, block.id, instruction, &defined)?;
            if produces_value(instruction) {
                defined.insert(next, block.id);
                next += 1;
            }
        }
        check_terminator(function, block.id, &block.terminator, &defined, result_size)?;
    }

    let dominators = compute_dominators(function, &block_ids)?;
    for block in &function.blocks {
        let Some(available) = dominators.get(&block.id.get()) else {
            continue;
        };
        for instruction in &block.instructions {
            for used in operands(instruction) {
                if let Some(block_of_definition) = defined.get(&used.get())
                    && !available.contains(&block_of_definition.get())
                {
                    return Err(IrError::new(IrErrorKind::UndominatedUse {
                        value: used.get(),
                        block: block.id.get(),
                    })
                    .in_function(&function.name));
                }
            }
        }
    }
    check_calls(module, function)?;
    Ok(())
}

fn check_calls(module: &Module, function: &Function) -> Result<(), IrError> {
    for block in &function.blocks {
        for instruction in &block.instructions {
            let Instruction::Call { target, args, .. } = instruction else {
                continue;
            };
            match target {
                CallTarget::Function(name) | CallTarget::Syscall(name) => {
                    let Some(callee) = module.function(name) else {
                        return Err(IrError::new(IrErrorKind::UnknownCallee {
                            name: name.clone(),
                        })
                        .in_function(&function.name));
                    };
                    check_call_arity_and_types(function, callee, args)?;
                }
                CallTarget::Imported(name) => {
                    return Err(
                        IrError::new(IrErrorKind::UnresolvedImport { name: name.clone() })
                            .in_function(&function.name),
                    );
                }
            }
        }
    }
    Ok(())
}

fn check_call_arity_and_types(
    function: &Function,
    callee: &Function,
    args: &[CallArg],
) -> Result<(), IrError> {
    if args.len() != callee.params.len() {
        return Err(IrError::new(IrErrorKind::ArityMismatch {
            expected: callee.params.len(),
            actual: args.len(),
        })
        .in_function(&function.name));
    }
    for (index, (argument, parameter)) in args.iter().zip(callee.params.iter()).enumerate() {
        let CallArg::Value(value) = argument else {
            continue;
        };
        let Some(actual) = value_type(function, *value) else {
            continue;
        };
        if actual != parameter.ty {
            return Err(IrError::new(IrErrorKind::ArgumentTypeMismatch {
                index,
                detail: alloc::format!(
                    "parameter {} expects {} but the argument is {}",
                    parameter.name,
                    parameter.ty,
                    actual
                ),
            })
            .in_function(&function.name));
        }
    }
    Ok(())
}

fn check_operands(
    function: &Function,
    block: BlockId,
    instruction: &Instruction,
    defined: &BTreeMap<u32, BlockId>,
) -> Result<(), IrError> {
    for used in operands(instruction) {
        if !defined.contains_key(&used.get()) {
            return Err(
                IrError::new(IrErrorKind::UndefinedValue { value: used.get() })
                    .in_function(&function.name)
                    .with_block(block),
            );
        }
    }
    if let Instruction::Store { value, width, .. } = instruction
        && let Some(ty) = value_type(function, *value)
        && let Some(size) = ty.size_in_bytes()
        && size != width.bytes()
        && !(matches!(ty, Type::Slice { .. }) && width.bytes() == 8)
    {
        return Err(IrError::new(IrErrorKind::WidthMismatch {
            bytes: width.bytes(),
            size,
        })
        .in_function(&function.name)
        .with_block(block));
    }
    Ok(())
}

fn check_terminator(
    function: &Function,
    block: BlockId,
    terminator: &Terminator,
    defined: &BTreeMap<u32, BlockId>,
    result_size: Option<u32>,
) -> Result<(), IrError> {
    let check_value = |value: ValueId| -> Result<(), IrError> {
        if !defined.contains_key(&value.get()) {
            return Err(
                IrError::new(IrErrorKind::UndefinedValue { value: value.get() })
                    .in_function(&function.name)
                    .with_block(block),
            );
        }
        Ok(())
    };
    match terminator {
        Terminator::Jump(_) | Terminator::Unreachable => Ok(()),
        Terminator::Branch {
            condition,
            then_block,
            otherwise,
        } => {
            check_value(*condition)?;
            if *then_block == *otherwise {
                return Err(IrError::new(IrErrorKind::InvalidBuilderState {
                    detail: alloc::format!("branch in b{} has identical targets", block.get()),
                })
                .in_function(&function.name)
                .with_block(block));
            }
            Ok(())
        }
        Terminator::Return(ReturnValue::Value(value)) => {
            check_value(*value)?;
            if function.result == Type::Void {
                return Err(IrError::new(IrErrorKind::InvalidType {
                    detail: alloc::format!(
                        "function {} returns a value but is declared void",
                        function.name
                    ),
                })
                .in_function(&function.name)
                .with_block(block));
            }
            let Some(_size) = result_size else {
                return Ok(());
            };
            Ok(())
        }
        Terminator::Return(ReturnValue::Void) => {
            if function.result != Type::Void {
                return Err(IrError::new(IrErrorKind::InvalidType {
                    detail: alloc::format!(
                        "function {} returns void but is declared {}",
                        function.name,
                        function.result.size_in_bytes().unwrap_or(0)
                    ),
                })
                .in_function(&function.name)
                .with_block(block));
            }
            Ok(())
        }
    }
}

/// Whether an instruction produces a value.
pub(crate) fn produces_value(instruction: &Instruction) -> bool {
    !matches!(
        instruction,
        Instruction::Store { .. } | Instruction::Trap { .. }
    )
}

/// Every value an instruction reads.
pub(crate) fn operands(instruction: &Instruction) -> Vec<ValueId> {
    let mut used = Vec::new();
    match instruction {
        Instruction::Const { .. } | Instruction::Trap { .. } => {}
        Instruction::Binary { left, right, .. } | Instruction::Compare { left, right, .. } => {
            used.push(*left);
            used.push(*right);
        }
        Instruction::LogicalAnd { left, right } | Instruction::LogicalOr { left, right } => {
            used.push(*left);
            used.push(*right);
        }
        Instruction::Unary { operand, .. } | Instruction::Copy { value: operand } => {
            used.push(*operand);
        }
        Instruction::Load { address, .. } => used.push(*address),
        Instruction::Store { address, value, .. } => {
            used.push(*address);
            used.push(*value);
        }
        Instruction::Call { args, .. } => {
            for argument in args {
                if let CallArg::Value(value) = argument {
                    used.push(*value);
                }
            }
        }
        Instruction::Intrinsic { operand, .. } => {
            if let Some(value) = operand {
                used.push(*value);
            }
        }
        Instruction::Extract { aggregate, .. } => used.push(*aggregate),
        Instruction::Insert {
            aggregate, value, ..
        } => {
            used.push(*aggregate);
            used.push(*value);
        }
    }
    used
}

fn value_type(function: &Function, value: ValueId) -> Option<Type> {
    let raw = value.get();
    if (raw as usize) < function.params.len() {
        return Some(function.params[raw as usize].ty.clone());
    }
    let mut index = function.params.len() as u32;
    for block in &function.blocks {
        for instruction in &block.instructions {
            if produces_value(instruction) {
                if index == raw {
                    return Some(instruction_result_type(instruction));
                }
                index += 1;
            }
        }
    }
    None
}

fn instruction_result_type(instruction: &Instruction) -> Type {
    match instruction {
        Instruction::Const { ty, .. }
        | Instruction::Binary { ty, .. }
        | Instruction::Unary { ty, .. }
        | Instruction::Load { ty, .. } => ty.clone(),
        Instruction::Compare { .. }
        | Instruction::LogicalAnd { .. }
        | Instruction::LogicalOr { .. } => Type::Bool,
        Instruction::Call { result, .. } => result.clone(),
        Instruction::Intrinsic { result, .. } => result.clone(),
        Instruction::Copy { .. } => Type::Void,
        Instruction::Extract { ty, .. } | Instruction::Insert { result: ty, .. } => ty.clone(),
        Instruction::Store { .. } | Instruction::Trap { .. } => Type::Void,
    }
}

/// Iterative dominator computation over the reverse-postorder of the CFG.
///
/// The entry block dominates every reachable block. Unreachable blocks are
/// absent from the result, and uses inside them are not checked for dominance,
/// because nothing can reach them and a code generator may drop them.
fn compute_dominators(
    function: &Function,
    block_ids: &BTreeMap<u32, ()>,
) -> Result<BTreeMap<u32, Vec<u32>>, IrError> {
    let _ = block_ids;
    let order = reverse_postorder(function);
    let mut reachable = Vec::new();
    let mut seen = BTreeMap::new();
    for id in &order {
        if function.block(*id).is_some() {
            seen.insert(id.get(), true);
            reachable.push(*id);
        }
    }
    let mut dominators: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    if let Some(entry) = function.blocks.first() {
        dominators.insert(entry.id.get(), alloc::vec![entry.id.get()]);
    }
    for id in reachable.iter().skip(1) {
        dominators.insert(id.get(), all_block_ids(function));
    }
    let mut changed = true;
    while changed {
        changed = false;
        for id in reachable.iter().skip(1) {
            let Some(block) = function.block(*id) else {
                continue;
            };
            let predecessors = predecessors_of(function, &block.id, &reachable);
            let mut intersection: Option<Vec<u32>> = None;
            for predecessor in predecessors {
                let Some(set) = dominators.get(&predecessor.get()) else {
                    continue;
                };
                intersection = Some(match intersection {
                    None => set.clone(),
                    Some(current) => current
                        .into_iter()
                        .filter(|candidate| set.contains(candidate))
                        .collect(),
                });
            }
            let mut updated = intersection.unwrap_or_default();
            updated.push(id.get());
            updated.sort_unstable();
            updated.dedup();
            if dominators.get(&id.get()) != Some(&updated) {
                dominators.insert(id.get(), updated);
                changed = true;
            }
        }
    }
    Ok(dominators)
}

fn all_block_ids(function: &Function) -> Vec<u32> {
    function.blocks.iter().map(|block| block.id.get()).collect()
}

fn successors_of(terminator: &Terminator) -> Vec<BlockId> {
    match terminator {
        Terminator::Jump(target) => alloc::vec![*target],
        Terminator::Branch {
            then_block,
            otherwise,
            ..
        } => alloc::vec![*then_block, *otherwise],
        Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
    }
}

fn predecessors_of(function: &Function, block: &BlockId, reachable: &[BlockId]) -> Vec<BlockId> {
    let mut predecessors = Vec::new();
    for candidate in reachable {
        let Some(other) = function.block(*candidate) else {
            continue;
        };
        if successors_of(&other.terminator).contains(block) {
            predecessors.push(*candidate);
        }
    }
    predecessors
}

fn reverse_postorder(function: &Function) -> Vec<BlockId> {
    let mut visited: BTreeMap<u32, ()> = BTreeMap::new();
    let mut order: Vec<BlockId> = Vec::new();
    let Some(entry) = function.blocks.first() else {
        return order;
    };
    let mut stack: Vec<BlockId> = alloc::vec![entry.id];
    while let Some(id) = stack.pop() {
        if visited.contains_key(&id.get()) {
            continue;
        }
        visited.insert(id.get(), ());
        let Some(block) = function.block(id) else {
            continue;
        };
        order.push(id);
        for successor in successors_of(&block.terminator) {
            stack.push(successor);
        }
    }
    order.reverse();
    order
}
