use super::*;

mod arith;
mod call;
mod composite;
mod control;
mod inline;
mod memory;

/// The bit of an LLVM call's marker field which says a function type precedes
/// the callee.
const CALL_EXPLICIT_TYPE: u32 = 15;
/// The bit which says fast-math flags precede it.
const CALL_FMF: u32 = 17;

/// What one LLVM body record lowered to.
///
/// `Value` is as large as an `InstructionKind`, which is what the lowering
/// used to hand back as a bare tuple; boxing it would put an allocation in
/// front of every instruction to shrink a value that never outlives its
/// caller.
#[allow(clippy::large_enum_variant)]
enum Lowered {
    /// The record defines a value of the given LLVM type.
    Value(usize, InstructionKind),
    /// The record needed no result of its own: a terminator, or one which
    /// defined its value itself.
    Done,
}

/// The state one function body is lowered into.
struct BodyLowering<'a> {
    /// The module's value numbering, which LLVM's relative references resolve
    /// against.
    table: ValueTable,
    types: &'a [DecodedType],
    /// The function's LLVM result type index.
    result_type: usize,
    /// The module's value-symbol-table names, which a call resolves
    /// its callee through.
    names: &'a HashMap<usize, String>,
    function: Function,
    /// Metal IR operands by LLVM value id.
    operands: HashMap<usize, Operand>,
    /// Metal IR values reserved for LLVM instructions which are used before
    /// the record stream defines them.
    forward: HashMap<usize, (ValueId, usize)>,
    /// Pointers produced by a widening bitcast keep the pointee type they
    /// really address; a load through one is adapted to the requested type.
    aliases: HashMap<usize, PointerAlias>,
    phi_fixups: Vec<PhiFixup>,
    switches: Vec<PendingSwitch>,
    /// The instructions of the block being lowered.
    instructions: Vec<Instruction>,
    terminator: Option<Terminator>,
    block_index: usize,
    block_count: usize,
    /// The value id the first instruction result of this body takes, for
    /// counting how far lowering got when a record needs work.
    first_result: usize,
    callees: Option<&'a super::super::bitcode::Callees<'a>>,
    /// The texel of each texture access whose result also carries a status,
    /// by the LLVM value of that result.
    texels_with_status: HashMap<usize, Operand>,
    /// Blocks made by inlining, numbered from the LLVM blocks' end in the
    /// order they are made.
    inlined_blocks: Vec<Block>,
    /// The part of the LLVM block being lowered before its first inlined
    /// call, and the branch into that callee.
    split: Option<(Vec<Instruction>, Terminator)>,
    /// The block the rest of the LLVM block goes to after an inlined call.
    continuation: Option<BlockId>,
}

impl ValueTable {
    /// Lower one LLVM function body to a Metal IR function.
    ///
    /// `opaque_types` gives Metal types for arguments whose LLVM type is
    /// opaque, keyed by argument position: AIR spells textures and samplers
    /// only in entry-point metadata.
    pub(in crate::air) fn lower_body(
        self,
        body: &BitcodeBlock,
        types: &[DecodedType],
        signature: &Signature<'_>,
    ) -> Result<Function, TranslateError> {
        let &Signature {
            names,
            ref name,
            parameter_types,
            result_type,
            opaque_types,
            callees,
            inline_samplers,
        } = signature;
        let name = name.clone();
        let records: Vec<_> = body
            .elements
            .iter()
            .filter_map(BitcodeElement::as_record)
            .collect();
        let [declaration, rest @ ..] = records.as_slice() else {
            return Err(semantic("LLVM function body is empty"));
        };
        if declaration.id != 1 || record_values(declaration).len() != 1 {
            return Err(semantic(
                "LLVM function must begin with one DECLAREBLOCKS record",
            ));
        }
        let block_count = usize::try_from(record_values(declaration)[0])
            .map_err(|_| semantic("LLVM block count does not fit in memory"))?;
        if block_count == 0 {
            return Err(semantic("LLVM function declares no blocks"));
        }
        // LLVM stores a body as one flat record list; a terminator ends the
        // current basic block. Split it before lowering so value numbering
        // and block membership are decided separately.
        let mut block_records: Vec<Vec<&llvm_bitcode::bitcode::Record>> =
            vec![Vec::new(); block_count];
        let mut index = 0;
        for record in rest {
            if record.id == 1 {
                return Err(semantic("LLVM body has more than one DECLAREBLOCKS record"));
            }
            // Debug-location records annotate the preceding instruction.
            if record.id == 33 || record.id == 35 {
                continue;
            }
            let block = block_records
                .get_mut(index)
                .ok_or_else(|| semantic("LLVM body has more blocks than it declares"))?;
            block.push(record);
            if is_terminator(record.id) {
                index += 1;
            }
        }
        if index != block_count {
            return Err(semantic(
                "LLVM body's terminators do not match its declared block count",
            ));
        }
        let mut function = Function {
            name,
            result: ir_type(result_type, types)?,
            parameters: Vec::new(),
            values: Vec::new(),
            blocks: Vec::new(),
        };
        let mut operands = HashMap::new();
        for (index, &ty) in parameter_types.iter().enumerate() {
            // An opaque LLVM argument is a Metal texture or sampler; its
            // shape is only in metadata. Any other argument keeps its LLVM
            // type, so a metadata mismatch is reported rather than obeyed.
            let opaque = match &types[ty] {
                DecodedType::Opaque(_) => true,
                DecodedType::Pointer { pointee, .. } => {
                    matches!(types[*pointee], DecodedType::Opaque(_))
                }
                _ => false,
            };
            let ty = match (opaque, opaque_types.get(&(index as u32))) {
                (true, Some(ty)) => ty.clone(),
                (true, None) => {
                    return Err(semantic(
                        "AIR opaque argument has no interface metadata type",
                    ));
                }
                (false, _) => ir_type(ty, types)?,
            };
            let value = ValueId(function.values.len() as u32);
            function.values.push(ty.clone());
            function.parameters.push(Parameter {
                value,
                name: format!("arg{index}"),
                ty,
            });
            operands.insert(self.argument_start + index, Operand::Value(value));
        }
        for &state in inline_samplers {
            let value = ValueId(function.values.len() as u32);
            function.values.push(Type::Sampler);
            function.parameters.push(Parameter {
                value,
                name: format!("inline_sampler{}", function.parameters.len()),
                ty: Type::Sampler,
            });
            operands.insert(state, Operand::Value(value));
        }
        // Composite constants need instructions. They are emitted at the head
        // of the entry block so every later use is dominated by them.
        let mut constants = Vec::new();
        let mut blocks: Vec<Block> = Vec::new();
        let first_result = self.values.len();
        let mut lowering = BodyLowering {
            table: self,
            types,
            result_type,
            names,
            function,
            operands,
            forward: HashMap::new(),
            aliases: HashMap::new(),
            phi_fixups: Vec::new(),
            switches: Vec::new(),
            instructions: Vec::new(),
            terminator: None,
            block_index: 0,
            block_count,
            first_result,
            callees,
            texels_with_status: HashMap::new(),
            inlined_blocks: Vec::new(),
            split: None,
            continuation: None,
        };
        // The block each LLVM block's terminator ended up in, when an inlined
        // call moved it out of the block's own.
        let mut moved_terminators = HashMap::new();
        for (block_index, block) in block_records.iter().enumerate() {
            lowering.block_index = block_index;
            lowering.instructions = Vec::new();
            lowering.terminator = None;
            for record in block {
                let fields = record_values(record);
                if lowering.terminator.is_some() {
                    return Err(semantic("LLVM instruction after terminator"));
                }
                let (ty, kind) = match lowering.lower(record, &fields)? {
                    Lowered::Value(ty, kind) => (ty, kind),
                    Lowered::Done => continue,
                };
                let value = emit(
                    &mut lowering.function,
                    &mut lowering.instructions,
                    ir_type(ty, types)?,
                    kind,
                );
                define(
                    &mut lowering.forward,
                    lowering.table.values.len(),
                    value,
                    &mut lowering.operands,
                    &lowering.function,
                    &mut lowering.instructions,
                )?;
                lowering.table.values.push(Value { ty, constant: None });
            }
            let name = if block_index == 0 {
                "entry".to_string()
            } else {
                format!("block{block_index}")
            };
            let instructions = std::mem::take(&mut lowering.instructions);
            let terminator = lowering
                .terminator
                .take()
                .ok_or_else(|| semantic("LLVM basic block has no terminator"))?;
            match lowering.split.take() {
                None => blocks.push(Block {
                    id: BlockId(block_index as u32),
                    name,
                    instructions,
                    terminator,
                }),
                Some((head, into_callee)) => {
                    let continuation = lowering
                        .continuation
                        .take()
                        .ok_or_else(|| semantic("inlined call has no continuation"))?;
                    lowering.inlined_blocks.push(Block {
                        id: continuation,
                        name: format!("{name}_after_call"),
                        instructions,
                        terminator,
                    });
                    blocks.push(Block {
                        id: BlockId(block_index as u32),
                        name,
                        instructions: head,
                        terminator: into_callee,
                    });
                    moved_terminators.insert(BlockId(block_index as u32), continuation);
                }
            }
        }
        let BodyLowering {
            table,
            mut function,
            mut operands,
            forward,
            phi_fixups,
            switches,
            mut inlined_blocks,
            ..
        } = lowering;
        if !forward.is_empty() {
            return Err(semantic(
                "LLVM forward reference names a value the body never defines",
            ));
        }
        for fixup in phi_fixups {
            let mut incomings = Vec::new();
            for (predecessor, id) in fixup.sources {
                let operand = table.materialize(id, types, &mut operands, &mut Vec::new())?;
                incomings.push((predecessor, operand));
            }
            blocks[fixup.block].instructions[fixup.instruction].kind =
                InstructionKind::Phi { incomings };
        }
        // Phi instructions must precede every other instruction in a block.
        for block in &mut blocks {
            let phis = block
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction.kind, InstructionKind::Phi { .. }))
                .count();
            if block.instructions[..phis]
                .iter()
                .any(|instruction| !matches!(instruction.kind, InstructionKind::Phi { .. }))
            {
                return Err(semantic(
                    "LLVM phi does not precede its block's instructions",
                ));
            }
        }
        // A phi names the block its value arrives from, which is the one now
        // holding the predecessor's terminator.
        for block in blocks.iter_mut().chain(&mut inlined_blocks) {
            for instruction in &mut block.instructions {
                if let InstructionKind::Phi { incomings } = &mut instruction.kind {
                    for (predecessor, _) in incomings {
                        if let Some(moved) = moved_terminators.get(predecessor) {
                            *predecessor = *moved;
                        }
                    }
                }
            }
        }
        blocks.extend(inlined_blocks);
        if blocks
            .iter()
            .enumerate()
            .any(|(index, block)| block.id.0 as usize != index)
        {
            return Err(semantic("inlined blocks are not numbered in order"));
        }
        for mut switch in switches {
            if let Some(moved) = moved_terminators.get(&BlockId(switch.block as u32)) {
                switch.block = moved.0 as usize;
            }
            expand_switch(&mut function, &mut blocks, switch);
        }
        constants.append(&mut blocks[0].instructions);
        blocks[0].instructions = constants;
        function.blocks = blocks;
        Ok(function)
    }
}

impl BodyLowering<'_> {
    /// The Metal IR operand for one LLVM value reference, and the LLVM type
    /// index it has.
    ///
    /// `declared` is the type a record repeats for an operand it names before
    /// the record stream defines it: LLVM lays a function's blocks out in an
    /// order which need not follow dominance. Reserve a Metal IR value for
    /// such an operand and fill it in when the definition arrives.
    fn operand(
        &mut self,
        encoded: u64,
        declared: Option<&u64>,
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<(usize, Operand), TranslateError> {
        let id = self.table.reference(encoded)?;
        if id < self.table.values.len() {
            let op = self
                .table
                .materialize(id, self.types, &mut self.operands, &mut Vec::new())?;
            return Ok((self.table.values[id].ty, op));
        }
        let Some(declared) = declared else {
            // Naming the record is what tells a reader whether the decoder
            // read the wrong field or the shader really names a value nothing
            // defines.
            return Err(semantic(format!(
                "LLVM value reference resolves to undefined value {id} at {}, in body record {} \
                 with operands {fields:?}",
                self.table.values.len(),
                record.id
            )));
        };
        let ty = type_reference(Some(declared), self.types.len())?;
        let value = match self.forward.get(&id) {
            Some((value, _)) => *value,
            None => {
                let value = ValueId(self.function.values.len() as u32);
                self.function.values.push(ir_type(ty, self.types)?);
                self.forward.insert(id, (value, ty));
                value
            }
        };
        Ok((ty, Operand::Value(value)))
    }

    /// Split one LLVM value-and-type operand off the front of `fields`. The
    /// type is written only after a value the record stream has not reached.
    fn typed_operand_fields<'f>(
        &self,
        fields: &'f [u64],
    ) -> Result<(&'f u64, Option<&'f u64>, &'f [u64]), TranslateError> {
        let [value, rest @ ..] = fields else {
            return Err(semantic("LLVM record is missing an operand"));
        };
        if self.table.reference(*value)? < self.table.values.len() {
            return Ok((value, None, rest));
        }
        let [declared, rest @ ..] = rest else {
            return Err(semantic("LLVM forward operand has no type"));
        };
        Ok((value, Some(declared), rest))
    }

    /// Lower one LLVM body record.
    fn lower(
        &mut self,
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<Lowered, TranslateError> {
        match (record.id, fields) {
            (6, [vector, index]) => self.extract_element(vector, index, record, fields),
            (7, [vector, object, index]) => {
                self.insert_element(vector, object, index, record, fields)
            }
            (8, [first, second, mask] | [first, _, second, mask]) => {
                self.shuffle_vector(first, second, mask, record, fields)
            }
            (3, [source_value, target, opcode]) => {
                self.cast(source_value, target, opcode, record, fields)
            }
            (19, [allocated, size_type, size, packed]) => {
                self.alloca(allocated, size_type, size, packed)
            }
            (30, [pointer, indices @ ..]) if !indices.is_empty() => {
                self.element_pointer(None, pointer, indices, record, fields)
            }
            // New LLVM GEP: inbounds, explicit source element type, pointer,
            // then the same relative value references as the older record.
            (43, [inbounds @ 0..=1, source, pointer, indices @ ..]) if !indices.is_empty() => {
                let _ = inbounds;
                self.element_pointer(Some(source), pointer, indices, record, fields)
            }
            (24 | 44, [pointer, value, alignment, volatile])
                if *volatile == 0 && *alignment <= 32 =>
            {
                self.store(pointer, value, record, fields)
            }
            (20, [pointer, alignment, volatile]) if *volatile == 0 && *alignment <= 32 => {
                self.load(pointer, None, record, fields)
            }
            // With opaque pointers enabled, LOAD carries the result type
            // explicitly between the pointer and its alignment.
            (20, [pointer, result, alignment, volatile]) if *volatile == 0 && *alignment <= 32 => {
                self.load(pointer, Some(result), record, fields)
            }
            (2, [lhs, rhs, opcode] | [lhs, rhs, opcode, _] | [lhs, _, rhs, opcode, _]) => {
                self.binary(lhs, rhs, opcode, record, fields)
            }
            (34, [_, cc, callee, args @ ..])
                if cc & ((1 << CALL_EXPLICIT_TYPE) | (1 << CALL_FMF)) == 0 =>
            {
                self.call(None, callee, args, record, fields)
            }
            (34, [_, cc, signature, callee, args @ ..])
                if cc & (1 << CALL_EXPLICIT_TYPE) != 0 && cc & (1 << CALL_FMF) == 0 =>
            {
                self.call(Some(signature), callee, args, record, fields)
            }
            // Fast-math flags describe optimization latitude rather than a
            // different operation in Metal IR. LLVM places them before the
            // optional explicit function type.
            (34, [_, cc, _, signature, callee, args @ ..])
                if cc & (1 << CALL_EXPLICIT_TYPE) != 0 && cc & (1 << CALL_FMF) != 0 =>
            {
                self.call(Some(signature), callee, args, record, fields)
            }
            (34, [_, cc, _, callee, args @ ..])
                if cc & (1 << CALL_EXPLICIT_TYPE) == 0 && cc & (1 << CALL_FMF) != 0 =>
            {
                self.call(None, callee, args, record, fields)
            }
            (9 | 28, _) => self.compare(record, fields),
            (26, [aggregate, indices @ ..]) if !indices.is_empty() => {
                self.extract_value(aggregate, indices, record, fields)
            }
            (27, rest) if rest.len() >= 3 => self.insert_value(rest, record, fields),
            (29, _) => self.select(record, fields),
            (10, []) => self.return_void(),
            (10, [value]) => self.return_value(value, record, fields),
            (11, [target]) => self.branch(target),
            (11, [true_block, false_block, condition]) => {
                self.cond_branch(true_block, false_block, condition, record, fields)
            }
            (12, [_condition_type, condition, default, cases @ ..])
                if cases.len().is_multiple_of(2) =>
            {
                self.switch(condition, default, cases, record, fields)
            }
            (15, []) => self.unreachable_record(),
            (16, [ty, incoming @ ..]) if !incoming.is_empty() => self.phi(ty, incoming),
            _ => Err(semantic(format!(
                "LLVM body record {} with operands {fields:?} needs lowering after {} decoded \
                 instruction result(s)",
                record.id,
                self.table.values.len() - self.first_result
            ))),
        }
    }
}
