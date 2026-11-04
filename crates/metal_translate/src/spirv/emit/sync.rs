use super::*;

impl Emitter {
    /// Emit one such instruction.
    pub(super) fn emit_sync(
        &mut self,
        function: &Function,
        instruction: &crate::ir::Instruction,
        values: &HashMap<ValueId, u32>,
        result_id: Option<u32>,
    ) -> Result<(), TranslateError> {
        match &instruction.kind {
            InstructionKind::GlobalInvocationId => {
                let result = instruction
                    .result
                    .ok_or_else(|| emission("global invocation ID has no result"))?;
                let ty = self.type_id(&function.values[result.0 as usize])?;
                self.builder
                    .load(
                        ty,
                        result_id,
                        self.global_invocation_id
                            .ok_or_else(|| emission("global invocation ID was not declared"))?,
                        None,
                        [],
                    )
                    .map_err(build_error)?;
            }
            // `discard_fragment` keeps executing in Metal, so demote the
            // invocation rather than terminating the block with OpKill.
            InstructionKind::DiscardFragment => {
                self.capability(Capability::DemoteToHelperInvocation)?;
                if self.spirv_version < (1, 6) {
                    self.extension("SPV_EXT_demote_to_helper_invocation");
                }
                // rspirv classifies this as a terminator and would close the
                // block. SPIR-V lets execution continue after it, as Metal
                // does, so append it without ending the block.
                self.builder
                    .insert_into_block(
                        InsertPoint::End,
                        rspirv::dr::Instruction::new(
                            rspirv::spirv::Op::DemoteToHelperInvocation,
                            None,
                            None,
                            Vec::new(),
                        ),
                    )
                    .map_err(build_error)?;
            }
            InstructionKind::Barrier { scope, memory } => {
                // `air.wg.barrier` is Metal's `threadgroup_barrier`: every
                // invocation of the threadgroup waits, whatever memory scope
                // the call names. So the execution scope is the workgroup and
                // only the memory scope comes from the operand - a barrier
                // executed at SIMD-group scope would let the rest of a
                // threadgroup run past it, which is what Metal's barrier
                // exists to prevent.
                let execution = self.scope_id(BarrierScope::Threadgroup)?;
                let memory_scope = self.scope_id(*scope)?;
                // Metal's barrier orders the memory its flags name; SPIR-V
                // spells that as memory semantics against a memory scope.
                let semantics = match memory_semantics(*memory) {
                    0 => 0,
                    bits => bits | MemorySemantics::ACQUIRE_RELEASE.bits(),
                };
                let semantics = self.constant(&Type::u32(), u64::from(semantics))?;
                if *scope == BarrierScope::Simdgroup {
                    self.capability(Capability::GroupNonUniform)?;
                }
                self.builder
                    .control_barrier(execution, memory_scope, semantics)
                    .map_err(build_error)?;
            }
            InstructionKind::Atomic {
                op,
                pointer,
                value,
                order,
                scope,
            } => {
                let result = instruction
                    .result
                    .ok_or_else(|| emission("atomic has no result"))?;
                let ty = self.type_id(&function.values[result.0 as usize])?;
                let pointer_id = self.operand(pointer, values)?;
                let value_id = self.operand(value, values)?;
                let scope_id = self.scope_id(*scope)?;
                // The address space the pointer is in is the memory this
                // ordering applies to; Metal names no other.
                let space = match operand_type(function, pointer) {
                    Some(Type::Pointer { space, .. }) => space,
                    _ => return Err(emission("atomic operand is not a pointer")),
                };
                let semantics = match order {
                    MemoryOrder::Relaxed => 0,
                    MemoryOrder::SequentiallyConsistent => {
                        MemorySemantics::SEQUENTIALLY_CONSISTENT.bits()
                            | match space {
                                AddressSpace::Threadgroup => {
                                    MemorySemantics::WORKGROUP_MEMORY.bits()
                                }
                                _ => MemorySemantics::UNIFORM_MEMORY.bits(),
                            }
                    }
                };
                let semantics = self.constant(&Type::u32(), u64::from(semantics))?;
                let build =
                    |builder: &mut Builder| match op {
                        AtomicOp::Add => builder
                            .atomic_i_add(ty, result_id, pointer_id, scope_id, semantics, value_id),
                        AtomicOp::Subtract => builder
                            .atomic_i_sub(ty, result_id, pointer_id, scope_id, semantics, value_id),
                        AtomicOp::MinimumSigned => builder
                            .atomic_s_min(ty, result_id, pointer_id, scope_id, semantics, value_id),
                        AtomicOp::MinimumUnsigned => builder
                            .atomic_u_min(ty, result_id, pointer_id, scope_id, semantics, value_id),
                        AtomicOp::MaximumSigned => builder
                            .atomic_s_max(ty, result_id, pointer_id, scope_id, semantics, value_id),
                        AtomicOp::MaximumUnsigned => builder
                            .atomic_u_max(ty, result_id, pointer_id, scope_id, semantics, value_id),
                        AtomicOp::And => builder
                            .atomic_and(ty, result_id, pointer_id, scope_id, semantics, value_id),
                        AtomicOp::Or => builder
                            .atomic_or(ty, result_id, pointer_id, scope_id, semantics, value_id),
                        AtomicOp::Xor => builder
                            .atomic_xor(ty, result_id, pointer_id, scope_id, semantics, value_id),
                        AtomicOp::Exchange => builder.atomic_exchange(
                            ty, result_id, pointer_id, scope_id, semantics, value_id,
                        ),
                    };
                build(&mut self.builder).map_err(build_error)?;
            }
            InstructionKind::FunctionConstantDefined { .. } => {
                return Err(emission(
                    "a function constant query survived specialization",
                ));
            }
            _ => unreachable!("not a sync instruction"),
        }
        Ok(())
    }
}
