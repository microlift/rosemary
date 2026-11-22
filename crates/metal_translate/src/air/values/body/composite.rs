use super::*;

impl BodyLowering<'_> {
    pub(super) fn extract_element(
        &mut self,
        vector: &u64,
        index: &u64,
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<Lowered, TranslateError> {
        let (ty, kind) = {
            let (ty, composite) = self.operand(*vector, None, record, fields)?;
            let DecodedType::Vector { element, lanes } = self.types[ty] else {
                return Err(semantic("extractelement requires vector"));
            };
            // Metal's `v[i]` need not name a constant lane.
            let index_id = self.table.operand_id(*index)?;
            match self.table.integer(index_id, self.types) {
                Ok(lane) if lane < u64::from(lanes) => (
                    element,
                    InstructionKind::CompositeExtract {
                        composite,
                        index: lane as u32,
                    },
                ),
                Ok(_) => {
                    return Err(semantic("extractelement lane is out of range"));
                }
                Err(error) => {
                    // Only a lane whose value is not yet known is
                    // a dynamic index; one of the wrong type is
                    // still the malformed record it was.
                    if !matches!(
                        self.table
                            .values
                            .get(index_id)
                            .map(|value| &self.types[value.ty]),
                        Some(DecodedType::Integer(_))
                    ) {
                        return Err(error);
                    }
                    let (_, index) = self.operand(*index, None, record, fields)?;
                    (
                        element,
                        InstructionKind::VectorExtractDynamic {
                            vector: composite,
                            index,
                        },
                    )
                }
            }
        };
        Ok(Lowered::Value(ty, kind))
    }

    pub(super) fn insert_element(
        &mut self,
        vector: &u64,
        object: &u64,
        index: &u64,
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<Lowered, TranslateError> {
        let (ty, kind) = {
            let (ty, composite) = self.operand(*vector, None, record, fields)?;
            let DecodedType::Vector { element, lanes } = self.types[ty] else {
                return Err(semantic("insertelement requires vector"));
            };
            // LLVM writes no type for the inserted element, even
            // when it names a value the record stream has not
            // reached: the vector's element type is it.
            let (object_ty, object) =
                self.operand(*object, Some(&(element as u64)), record, fields)?;
            if object_ty != element {
                return Err(semantic("insertelement element type mismatch"));
            }
            let index_id = self.table.operand_id(*index)?;
            match self.table.integer(index_id, self.types) {
                Ok(lane) if lane < u64::from(lanes) => (
                    ty,
                    InstructionKind::CompositeInsert {
                        composite,
                        object,
                        index: lane as u32,
                    },
                ),
                Ok(_) => {
                    return Err(semantic("insertelement lane is out of range"));
                }
                Err(error) => {
                    // Only a lane whose value is not yet known is
                    // a dynamic index; one of the wrong type is
                    // still the malformed record it was.
                    if !matches!(
                        self.table
                            .values
                            .get(index_id)
                            .map(|value| &self.types[value.ty]),
                        Some(DecodedType::Integer(_))
                    ) {
                        return Err(error);
                    }
                    let (_, index) = self.operand(*index, None, record, fields)?;
                    (
                        ty,
                        InstructionKind::VectorInsertDynamic {
                            vector: composite,
                            object,
                            index,
                        },
                    )
                }
            }
        };
        Ok(Lowered::Value(ty, kind))
    }

    pub(super) fn shuffle_vector(
        &mut self,
        first: &u64,
        second: &u64,
        mask: &u64,
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<Lowered, TranslateError> {
        let (ty, kind) = {
            let (ty, first) = match fields {
                [_, declared, _, _] => self.operand(*first, Some(declared), record, fields)?,
                _ => self.operand(*first, None, record, fields)?,
            };
            let (second_ty, second) = self.operand(*second, None, record, fields)?;
            let DecodedType::Vector { element, lanes } = self.types[ty] else {
                return Err(semantic("shufflevector requires vectors"));
            };
            if ty != second_ty {
                return Err(semantic("shufflevector input type mismatch"));
            }
            let mask = self.table.operand_id(*mask)?;
            let components = self.table.shuffle_mask(mask, self.types)?;
            if components
                .iter()
                .any(|&index| index != u32::MAX && index >= lanes * 2)
            {
                return Err(semantic("shufflevector mask is out of range"));
            }
            let result = self.types.iter().position(|ty| matches!(ty, DecodedType::Vector { element: e, lanes: n } if *e == element && *n as usize == components.len())).ok_or_else(|| semantic("shufflevector result type missing"))?;
            (
                result,
                InstructionKind::VectorShuffle {
                    first,
                    second,
                    components,
                },
            )
        };
        Ok(Lowered::Value(ty, kind))
    }

    pub(super) fn extract_value(
        &mut self,
        aggregate: &u64,
        indices: &[u64],
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<Lowered, TranslateError> {
        let aggregate_id = self.table.operand_id(*aggregate)?;
        if let Some(texel) = self.texels_with_status.get(&aggregate_id).cloned() {
            let [0] = indices else {
                return Err(semantic(
                    "AIR texture access status needs Metal IR lowering",
                ));
            };
            let ty = member_type(&self.types[self.table.values[aggregate_id].ty], 0)?;
            define(
                &mut self.forward,
                self.table.values.len(),
                texel,
                &mut self.operands,
                &self.function,
                &mut self.instructions,
            )?;
            self.table.values.push(Value { ty, constant: None });
            return Ok(Lowered::Done);
        }
        let (mut ty, mut composite) = self.operand(*aggregate, None, record, fields)?;
        let mut extracted = None;
        for &index in indices {
            let index = u32::try_from(index)
                .map_err(|_| semantic("LLVM extractvalue index is too large"))?;
            let member = member_type(&self.types[ty], index)?;
            composite = emit(
                &mut self.function,
                &mut self.instructions,
                ir_type(member, self.types)?,
                InstructionKind::CompositeExtract { composite, index },
            );
            extracted = Some(composite.clone());
            ty = member;
        }
        let value = extracted.expect("indices are non-empty");
        define(
            &mut self.forward,
            self.table.values.len(),
            value,
            &mut self.operands,
            &self.function,
            &mut self.instructions,
        )?;
        self.table.values.push(Value { ty, constant: None });
        Ok(Lowered::Done)
    }

    pub(super) fn insert_value(
        &mut self,
        rest: &[u64],
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<Lowered, TranslateError> {
        let (ty, kind) = {
            // LLVM writes both of `insertvalue`'s values as a
            // value-type pair, which repeats the type only for a
            // value the record stream has not reached yet. Only
            // the value table can say which form a record is in.
            let aggregate_forward = self.table.reference(rest[0])? >= self.table.values.len();
            let object_at = 1 + usize::from(aggregate_forward);
            let object_forward = match rest.get(object_at) {
                Some(field) => self.table.reference(*field)? >= self.table.values.len(),
                None => return Err(semantic("LLVM insertvalue record is too short")),
            };
            let index_at = object_at + 1 + usize::from(object_forward);
            let ([aggregate, aggregate_ty @ ..], [object, object_ty @ ..]) = (
                &rest[..object_at],
                &rest[object_at..index_at.min(rest.len())],
            ) else {
                return Err(semantic("LLVM insertvalue record is too short"));
            };
            let [index] = rest[index_at.min(rest.len())..] else {
                return Err(semantic(
                    "LLVM insertvalue names more than one aggregate index",
                ));
            };
            let (ty, composite) = self.operand(*aggregate, aggregate_ty.first(), record, fields)?;
            let (object_ty, object) = self.operand(*object, object_ty.first(), record, fields)?;
            let index = u32::try_from(index)
                .map_err(|_| semantic("LLVM insertvalue index is too large"))?;
            if member_type(&self.types[ty], index)? != object_ty {
                return Err(semantic("LLVM insertvalue element type mismatch"));
            }
            (
                ty,
                InstructionKind::CompositeInsert {
                    composite,
                    object,
                    index,
                },
            )
        };
        Ok(Lowered::Value(ty, kind))
    }
}
