use super::*;

impl BodyLowering<'_> {
    /// Splice `callee` into the block being lowered, as a call of it with
    /// `arguments`: the block branches into a copy of the callee's blocks,
    /// whose returns all branch to a new block the rest of this one goes on in.
    /// Answers each return's block and value, for the call's result.
    pub(super) fn inline(
        &mut self,
        callee: &Function,
        arguments: Vec<Operand>,
    ) -> Result<Vec<(BlockId, Operand)>, TranslateError> {
        if callee
            .blocks
            .iter()
            .enumerate()
            .any(|(index, block)| block.id.0 as usize != index)
        {
            return Err(semantic(format!(
                "AIR function {}'s blocks are not numbered in order",
                callee.name
            )));
        }
        let next_block = u32::try_from(self.block_count + self.inlined_blocks.len())
            .ok()
            .and_then(|next| next.checked_add(u32::from(self.continuation.is_some())))
            .ok_or_else(|| semantic("inlined function has too many blocks"))?;
        let block_count = u32::try_from(callee.blocks.len())
            .map_err(|_| semantic("inlined function has too many blocks"))?;
        let continuation = BlockId(next_block + block_count);
        let value_offset = u32::try_from(self.function.values.len())
            .map_err(|_| semantic("inlined function has too many values"))?;
        self.function.values.extend(callee.values.iter().cloned());
        let parameters: HashMap<ValueId, Operand> = callee
            .parameters
            .iter()
            .map(|parameter| parameter.value)
            .zip(arguments)
            .collect();
        let remap = Remap {
            parameters,
            value_offset,
            block_offset: next_block,
        };

        let head = std::mem::take(&mut self.instructions);
        let into_callee = Terminator::Branch(BlockId(next_block));
        match self.continuation.replace(continuation) {
            None => self.split = Some((head, into_callee)),
            // A second call in one LLVM block: the first one's continuation
            // ends by entering this callee.
            Some(previous) => self.inlined_blocks.push(Block {
                id: previous,
                name: format!("block{}_after_call", self.block_index),
                instructions: head,
                terminator: into_callee,
            }),
        }
        let mut returns = Vec::new();
        for block in &callee.blocks {
            let id = remap.block(block.id);
            let instructions = block
                .instructions
                .iter()
                .map(|instruction| remap.instruction(instruction))
                .collect();
            let terminator = match &block.terminator {
                Terminator::Return(value) => {
                    if let Some(value) = value {
                        returns.push((id, remap.operand(value)));
                    }
                    Terminator::Branch(continuation)
                }
                terminator => remap.terminator(terminator),
            };
            self.inlined_blocks.push(Block {
                id,
                name: format!("{}_{}", callee.name, block.name),
                instructions,
                terminator,
            });
        }
        Ok(returns)
    }
}

/// How a callee's values and blocks are numbered in its caller.
struct Remap {
    parameters: HashMap<ValueId, Operand>,
    value_offset: u32,
    block_offset: u32,
}

impl Remap {
    fn value(&self, value: ValueId) -> ValueId {
        ValueId(value.0 + self.value_offset)
    }

    fn block(&self, block: BlockId) -> BlockId {
        BlockId(block.0 + self.block_offset)
    }

    fn operand(&self, operand: &Operand) -> Operand {
        match operand {
            Operand::Value(value) => self
                .parameters
                .get(value)
                .cloned()
                .unwrap_or(Operand::Value(self.value(*value))),
            Operand::Composite { ty, elements } => Operand::Composite {
                ty: ty.clone(),
                elements: elements
                    .iter()
                    .map(|element| self.operand(element))
                    .collect(),
            },
            operand => operand.clone(),
        }
    }

    fn instruction(&self, instruction: &Instruction) -> Instruction {
        let mut kind = instruction.kind.clone();
        for operand in crate::ir::instruction_operands_mut(&mut kind) {
            *operand = self.operand(operand);
        }
        if let InstructionKind::Phi { incomings } = &mut kind {
            for (block, _) in incomings {
                *block = self.block(*block);
            }
        }
        Instruction {
            result: instruction.result.map(|result| self.value(result)),
            kind,
        }
    }

    fn terminator(&self, terminator: &Terminator) -> Terminator {
        match terminator {
            Terminator::Branch(target) => Terminator::Branch(self.block(*target)),
            Terminator::CondBranch {
                condition,
                true_block,
                false_block,
            } => Terminator::CondBranch {
                condition: self.operand(condition),
                true_block: self.block(*true_block),
                false_block: self.block(*false_block),
            },
            Terminator::Return(_) => unreachable!("returns branch to the continuation"),
            Terminator::Unreachable => Terminator::Unreachable,
        }
    }
}
