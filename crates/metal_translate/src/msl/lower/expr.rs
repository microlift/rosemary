//! Expressions: places, values, conversions and operators.

use super::*;
use crate::ir::{
    ConvertOp, FloatBinaryOp, FloatPredicate, FloatUnaryOp, IntegerBinaryOp, IntegerPredicate,
    IntegerUnaryOp,
};

impl Lowerer<'_, '_> {
    /// A name nothing in scope declares: the reason its declaration was
    /// skipped, where it was.
    fn unknown_name(&self, name: &str, line: usize) -> TranslateError {
        match self
            .context
            .program
            .failed
            .iter()
            .find(|failed| failed.name.as_deref() == Some(name))
        {
            Some(failed) => failed.error.clone(),
            None => syntax_error(line, format!("unknown name {name}")),
        }
    }

    /// Evaluate an expression for its effect alone.
    pub fn effect(&mut self, expr: &Expr) -> Result<()> {
        match &expr.kind {
            // `discard_fragment()` and other void calls have no value.
            ExprKind::Call { name, args, .. } if name == "discard_fragment" => {
                if !args.is_empty() || self.stage != Stage::Fragment {
                    return Err(syntax_error(
                        expr.line,
                        "discard_fragment() outside a fragment function",
                    ));
                }
                self.emit_void(InstructionKind::DiscardFragment);
                Ok(())
            }
            ExprKind::Method { name, .. } if name == "write" => {
                self.texture_method(expr)?;
                Ok(())
            }
            ExprKind::Call { name, args, .. }
                if name == "threadgroup_barrier" || name == "simdgroup_barrier" =>
            {
                self.barrier(name, args, expr.line)
            }
            // `(void) x;` marks a value as deliberately unused; MojoShader
            // writes one after every constant it declares.
            ExprKind::Cast(Ty::Void, operand) => self.effect(operand),
            _ => self.rvalue(expr).map(|_| ()),
        }
    }

    /// A condition, as a scalar `bool`.
    pub fn condition(&mut self, expr: &Expr) -> Result<Operand> {
        let value = self.rvalue(expr)?;
        if value.ty.lanes() != Some(1) {
            return Err(syntax_error(expr.line, "a condition must be a scalar"));
        }
        Ok(self
            .convert(value, &Ty::Scalar(Scalar::Bool), expr.line)?
            .op)
    }

    /// The place an expression names, where it names one.
    pub fn lvalue(&mut self, expr: &Expr) -> Result<Option<LValue>> {
        let line = expr.line;
        Ok(Some(match &expr.kind {
            ExprKind::Ident(name) => match self.lookup(name) {
                Some(Binding::LValue(lvalue)) => lvalue,
                Some(_) => return Ok(None),
                None => return Err(self.unknown_name(name, line)),
            },
            ExprKind::Member(object, member) => {
                // A member of a buffer pointer must go through an index.
                let Some(object) = self.lvalue(object)? else {
                    return Ok(None);
                };
                self.member_place(object, member, line)?
            }
            ExprKind::Index(object, index) => {
                if let Some(pointer) = self.pointer(object)? {
                    return Ok(Some(self.index_pointer(pointer, index, line)?));
                }
                let Some(object) = self.lvalue(object)? else {
                    return Ok(None);
                };
                self.index_place(object, index, line)?
            }
            ExprKind::Unary("*", operand) => self.deref(operand, line)?,
            _ => return Ok(None),
        }))
    }

    pub(super) fn index_operand(&mut self, index: &Expr) -> Result<Operand> {
        let line = index.line;
        let value = self.rvalue(index)?;
        if !matches!(value.ty, Ty::Scalar(Scalar::Int { .. } | Scalar::Bool)) {
            return Err(syntax_error(line, "an index must be an integer"));
        }
        let ty = match value.ty {
            Ty::Scalar(Scalar::Int { signed, .. }) => Ty::Scalar(Scalar::Int { signed, bits: 32 }),
            _ => Ty::Scalar(Scalar::UINT),
        };
        Ok(self.convert(value, &ty, line)?.op)
    }

    fn member_place(&mut self, object: LValue, member: &str, line: usize) -> Result<LValue> {
        match object {
            LValue::Fields(index, fields) => {
                let position = self.field_position(index, member, line)?;
                Ok(fields[position].clone())
            }
            LValue::Bytes(bytes) if matches!(bytes.ty, Ty::Struct(_)) => {
                self.bytes_member(&bytes, member, line)
            }
            LValue::Memory(place) if matches!(place.ty, Ty::Struct(_)) => {
                let Ty::Struct(index) = place.ty else {
                    unreachable!()
                };
                let position = self.field_position(index, member, line)?;
                let ty = self.context.program.structs[index].fields[position]
                    .ty
                    .clone();
                let mut indices = place.indices;
                if indices.is_empty() {
                    indices.push(Operand::u32(0));
                }
                indices.push(Operand::u32(position as u32));
                Ok(LValue::Memory(Place {
                    indices,
                    ty,
                    ..place
                }))
            }
            object => {
                let ty = object.ty();
                let (scalar, lanes) = match ty {
                    Ty::Vector(scalar, lanes) => (scalar, lanes),
                    _ => return Err(syntax_error(line, format!("no member {member}"))),
                };
                let selected = swizzle_lanes(member, lanes)
                    .ok_or_else(|| syntax_error(line, format!("bad swizzle .{member}")))?;
                let ty = Ty::of(scalar, selected.len() as u8);
                Ok(LValue::Lanes(Box::new(object), selected, ty))
            }
        }
    }

    fn index_place(&mut self, object: LValue, index: &Expr, line: usize) -> Result<LValue> {
        let ty = object.ty();
        let element = match &ty {
            Ty::Array(element, _) => Some((**element).clone()),
            Ty::Matrix { .. } => ty.column(),
            _ => None,
        };
        if element.is_some()
            && let LValue::Bytes(bytes) = &object
        {
            let index = self.index_operand(index)?;
            return self.bytes_element(bytes, index, line);
        }
        if let Some(element) = element {
            let LValue::Memory(place) = object else {
                return Err(syntax_error(line, "an array which is not in memory"));
            };
            let index = self.index_operand(index)?;
            let mut indices = place.indices;
            if indices.is_empty() {
                indices.push(Operand::u32(0));
            }
            indices.push(index);
            return Ok(LValue::Memory(Place {
                indices,
                ty: element,
                ..place
            }));
        }
        match ty {
            Ty::Vector(scalar, lanes) => {
                if let Some(Number::Int(lane)) = self.context.const_number(index) {
                    if !(0..i64::from(lanes)).contains(&lane) {
                        return Err(syntax_error(line, "a vector index out of range"));
                    }
                    return Ok(LValue::Lanes(
                        Box::new(object),
                        vec![lane as u32],
                        Ty::Scalar(scalar),
                    ));
                }
                let index = self.index_operand(index)?;
                Ok(LValue::Dynamic(Box::new(object), index, Ty::Scalar(scalar)))
            }
            _ => Err(syntax_error(
                line,
                "indexing something not an array or vector",
            )),
        }
    }

    pub fn field_position(&self, index: usize, member: &str, line: usize) -> Result<usize> {
        self.context.program.structs[index]
            .fields
            .iter()
            .position(|field| field.name == member)
            .ok_or_else(|| syntax_error(line, format!("no field {member}")))
    }

    /// A pointer to a place, materialized.
    pub fn address(&mut self, place: &Place) -> Operand {
        if place.indices.is_empty()
            || (place.indices.len() == 1
                && matches!(place.indices[0], Operand::Integer { value: 0, .. })
                && !matches!(place.root, Operand::Global { .. })
                && place.space == AddressSpace::Thread)
        {
            return place.root.clone();
        }
        let ty = Type::Pointer {
            space: place.space,
            pointee: Box::new(self.context.ir(&place.ty)),
        };
        self.emit(
            ty,
            InstructionKind::BufferAddress {
                pointer: place.root.clone(),
                indices: place.indices.clone(),
            },
        )
    }

    pub fn load(&mut self, lvalue: &LValue, line: usize) -> Result<Value> {
        match lvalue {
            LValue::Memory(place) => {
                if let Ty::Struct(index) = place.ty
                    && place.space == AddressSpace::Thread
                {
                    let _ = index;
                    return Err(syntax_error(line, "a structure in thread memory"));
                }
                let op = match local_elements(place) {
                    // An aggregate local is read element by element: see
                    // `local_elements`.
                    Some((element, count)) => {
                        let ir = self.context.ir(&place.ty);
                        let mut op = Operand::Undef(ir.clone());
                        for index in 0..count {
                            let part = self.load(&element_of(place, index, &element), line)?;
                            let part = self.pack(part, &element);
                            op = self.emit(
                                ir.clone(),
                                InstructionKind::CompositeInsert {
                                    composite: op,
                                    object: part,
                                    index,
                                },
                            );
                        }
                        op
                    }
                    None => {
                        let pointer = self.address(place);
                        self.emit(
                            self.context.ir(&place.ty),
                            InstructionKind::Load { pointer },
                        )
                    }
                };
                Ok(self.unpack(Value {
                    op,
                    ty: place.ty.clone(),
                }))
            }
            LValue::Lanes(base, lanes, _) => {
                let base = self.load(base, line)?;
                self.swizzle(base, lanes, line)
            }
            LValue::Dynamic(base, index, ty) => {
                let base = self.load(base, line)?;
                let op = self.emit(
                    self.context.ir(ty),
                    InstructionKind::VectorExtractDynamic {
                        vector: base.op,
                        index: index.clone(),
                    },
                );
                Ok(Value { op, ty: ty.clone() })
            }
            LValue::Fields(index, fields) => {
                let ty = Ty::Struct(*index);
                let mut op = Operand::Undef(self.context.ir(&ty));
                for (position, field) in fields.iter().enumerate() {
                    let field = self.load(field, line)?;
                    op = self.emit(
                        self.context.ir(&ty),
                        InstructionKind::CompositeInsert {
                            composite: op,
                            object: field.op,
                            index: position as u32,
                        },
                    );
                }
                Ok(Value { op, ty })
            }
            LValue::Bytes(bytes) => self.load_bytes(bytes, line),
        }
    }

    /// Store `value`, already of the place's type.
    pub fn store(&mut self, lvalue: &LValue, value: Value, line: usize) -> Result<()> {
        match lvalue {
            LValue::Memory(place) => {
                if place.space == AddressSpace::Constant {
                    return Err(syntax_error(line, "an assignment to constant memory"));
                }
                let value = self.pack(value, &place.ty);
                if let Some((element, count)) = local_elements(place) {
                    for index in 0..count {
                        let part = self.emit(
                            self.context.ir(&element),
                            InstructionKind::CompositeExtract {
                                composite: value.clone(),
                                index,
                            },
                        );
                        let part = self.unpack(Value {
                            op: part,
                            ty: element.clone(),
                        });
                        self.store(&element_of(place, index, &element), part, line)?;
                    }
                    return Ok(());
                }
                let pointer = self.address(place);
                self.emit_void(InstructionKind::Store { pointer, value });
                Ok(())
            }
            LValue::Lanes(base, lanes, _) => {
                let old = self.load(base, line)?;
                let Ty::Vector(scalar, _) = old.ty else {
                    return Err(syntax_error(line, "a swizzle of something not a vector"));
                };
                let mut op = old.op;
                for (position, lane) in lanes.iter().enumerate() {
                    let element = if lanes.len() == 1 {
                        value.op.clone()
                    } else {
                        self.emit(
                            scalar_ir(scalar),
                            InstructionKind::CompositeExtract {
                                composite: value.op.clone(),
                                index: position as u32,
                            },
                        )
                    };
                    op = self.emit(
                        self.context.ir(&old.ty),
                        InstructionKind::CompositeInsert {
                            composite: op,
                            object: element,
                            index: *lane,
                        },
                    );
                }
                let ty = old.ty;
                self.store(base, Value { op, ty }, line)
            }
            LValue::Dynamic(base, index, _) => {
                let old = self.load(base, line)?;
                let op = self.emit(
                    self.context.ir(&old.ty),
                    InstructionKind::VectorInsertDynamic {
                        vector: old.op,
                        object: value.op,
                        index: index.clone(),
                    },
                );
                let ty = old.ty;
                self.store(base, Value { op, ty }, line)
            }
            LValue::Fields(index, fields) => {
                let parts = self.split_struct(value, *index)?;
                for (field, part) in fields.iter().zip(parts) {
                    self.store(field, part, line)?;
                }
                Ok(())
            }
            LValue::Bytes(bytes) => self.store_bytes(bytes, value, line),
        }
    }

    /// A packed vector or a `bool` read from memory, as the value it is.
    pub(super) fn unpack(&mut self, value: Value) -> Value {
        if value.ty == Ty::ByteBool {
            let byte = self.emit(
                Type::u32(),
                InstructionKind::Convert {
                    op: ConvertOp::ZeroExtend,
                    value: value.op,
                },
            );
            let op = self.emit(
                Type::Bool,
                InstructionKind::IntegerCompare {
                    predicate: IntegerPredicate::NotEqual,
                    lhs: byte,
                    rhs: Operand::u32(0),
                },
            );
            return Value {
                op,
                ty: Ty::Scalar(Scalar::Bool),
            };
        }
        let Ty::Packed(scalar, lanes) = value.ty else {
            return value;
        };
        let elements = (0..u32::from(lanes))
            .map(|index| {
                self.emit(
                    scalar_ir(scalar),
                    InstructionKind::CompositeExtract {
                        composite: value.op.clone(),
                        index,
                    },
                )
            })
            .collect();
        let op = self.vector_of(scalar, elements);
        Value {
            op,
            ty: Ty::Vector(scalar, lanes),
        }
    }

    /// A value as memory of type `ty` holds it: a vector stored to a packed
    /// one becomes its array of lanes, and a `bool` a byte. The byte is chosen
    /// at 32 bits, so that only a conversion touches the narrow type.
    pub(super) fn pack(&mut self, value: Value, ty: &Ty) -> Operand {
        if *ty == Ty::ByteBool {
            let word = self.emit(
                Type::u32(),
                InstructionKind::Select {
                    condition: value.op,
                    on_true: Operand::u32(1),
                    on_false: Operand::u32(0),
                },
            );
            return self.emit(
                scalar_ir(Scalar::UCHAR),
                InstructionKind::Convert {
                    op: ConvertOp::IntegerTruncate,
                    value: word,
                },
            );
        }
        let Ty::Packed(scalar, lanes) = *ty else {
            return value.op;
        };
        let ir = self.context.ir(ty);
        let mut op = Operand::Undef(ir.clone());
        for index in 0..u32::from(lanes) {
            let lane = self.emit(
                scalar_ir(scalar),
                InstructionKind::CompositeExtract {
                    composite: value.op.clone(),
                    index,
                },
            );
            op = self.emit(
                ir.clone(),
                InstructionKind::CompositeInsert {
                    composite: op,
                    object: lane,
                    index,
                },
            );
        }
        op
    }

    /// Select lanes of a vector value.
    pub fn swizzle(&mut self, value: Value, lanes: &[u32], line: usize) -> Result<Value> {
        let Ty::Vector(scalar, _) = value.ty else {
            return Err(syntax_error(line, "a swizzle of something not a vector"));
        };
        if let [lane] = lanes {
            let op = self.emit(
                scalar_ir(scalar),
                InstructionKind::CompositeExtract {
                    composite: value.op,
                    index: *lane,
                },
            );
            return Ok(Value {
                op,
                ty: Ty::Scalar(scalar),
            });
        }
        let ty = Ty::Vector(scalar, lanes.len() as u8);
        let op = self.emit(
            self.context.ir(&ty),
            InstructionKind::VectorShuffle {
                first: value.op.clone(),
                second: value.op,
                components: lanes.to_vec(),
            },
        );
        Ok(Value { op, ty })
    }

    pub fn rvalue(&mut self, expr: &Expr) -> Result<Value> {
        let line = expr.line;
        match &expr.kind {
            ExprKind::Int { value, unsigned } => Ok(Value {
                op: Operand::u32(*value as u32),
                ty: Ty::Scalar(if *unsigned { Scalar::UINT } else { Scalar::INT }),
            }),
            ExprKind::Float { value, half } => {
                let scalar = Scalar::Float {
                    bits: if *half { 16 } else { 32 },
                };
                Ok(Value {
                    op: number_operand(scalar, Number::Float(*value)),
                    ty: Ty::Scalar(scalar),
                })
            }
            ExprKind::Bool(value) => Ok(Value {
                op: Operand::Bool(*value),
                ty: Ty::Scalar(Scalar::Bool),
            }),
            ExprKind::Ident(name) => match self.lookup(name) {
                Some(Binding::Value(value)) => Ok(value),
                Some(Binding::LValue(lvalue)) => self.load(&lvalue, line),
                Some(Binding::Pointer(_)) => {
                    Err(syntax_error(line, "a buffer pointer used as a value"))
                }
                None => Err(self.unknown_name(name, line)),
            },
            ExprKind::Member(object, member) => {
                if let Some(lvalue) = self.lvalue(expr)? {
                    return self.load(&lvalue, line);
                }
                let value = self.rvalue(object)?;
                match value.ty {
                    Ty::Vector(_, lanes) => {
                        let selected = swizzle_lanes(member, lanes)
                            .ok_or_else(|| syntax_error(line, format!("bad swizzle .{member}")))?;
                        self.swizzle(value, &selected, line)
                    }
                    Ty::Struct(index) => {
                        let position = self.field_position(index, member, line)?;
                        let ty = self.context.program.structs[index].fields[position]
                            .ty
                            .clone();
                        let op = self.emit(
                            self.context.ir(&ty),
                            InstructionKind::CompositeExtract {
                                composite: value.op,
                                index: position as u32,
                            },
                        );
                        Ok(self.unpack(Value { op, ty }))
                    }
                    _ => Err(syntax_error(line, format!("no member {member}"))),
                }
            }
            ExprKind::Index(object, index) => {
                if let Some(lvalue) = self.lvalue(expr)? {
                    return self.load(&lvalue, line);
                }
                let value = self.rvalue(object)?;
                match value.ty {
                    Ty::Vector(scalar, _) => {
                        let index = self.index_operand(index)?;
                        let op = self.emit(
                            scalar_ir(scalar),
                            InstructionKind::VectorExtractDynamic {
                                vector: value.op,
                                index,
                            },
                        );
                        Ok(Value {
                            op,
                            ty: Ty::Scalar(scalar),
                        })
                    }
                    _ => {
                        // An array value: index it through a temporary.
                        let place = self.local(&value.ty);
                        let lvalue = LValue::Memory(place);
                        self.store(&lvalue, value, line)?;
                        let element = self.index_place(lvalue, index, line)?;
                        self.load(&element, line)
                    }
                }
            }
            ExprKind::Unary(op, operand) => self.unary(op, operand, line),
            ExprKind::Postfix(op, operand) => self.increment(op, operand, false, line),
            ExprKind::Binary(op, lhs, rhs) => self.binary(op, lhs, rhs, line),
            ExprKind::Assign(op, lhs, rhs) => self.assign(*op, lhs, rhs, line),
            ExprKind::Ternary(condition, yes, no) => self.ternary(condition, yes, no, line),
            ExprKind::Construct(ty, args) => {
                let ty = self.resolve(ty, line)?;
                self.construct(&ty, args, line)
            }
            ExprKind::Cast(ty, operand) => {
                let ty = self.resolve(ty, line)?;
                let value = self.rvalue(operand)?;
                self.convert(value, &ty, line)
            }
            ExprKind::CompoundLiteral(ty, items) => {
                let ty = self.resolve(ty, line)?;
                self.compound_literal(&ty, items, line)
            }
            ExprKind::InitList(_) => Err(syntax_error(line, "an initializer list used as a value")),
            ExprKind::Call {
                name,
                template,
                args,
            } => {
                let template = template
                    .as_ref()
                    .map(|ty| self.resolve(ty, line))
                    .transpose()?;
                self.call(name, template.as_ref(), args, line)
            }
            ExprKind::Method { .. } => self.texture_method(expr),
            ExprKind::AtomicCast(..) => Err(syntax_error(
                line,
                "an atomic pointer used other than by an atomic function",
            )),
            ExprKind::PointerCast(..) => Err(syntax_error(line, "a pointer used as a value")),
        }
    }

    /// Convert a value to `to` as an implicit or explicit C++ conversion
    /// does.
    pub fn convert(&mut self, value: Value, to: &Ty, line: usize) -> Result<Value> {
        if &value.ty == to {
            return Ok(value);
        }
        match (&value.ty, to) {
            (Ty::Scalar(from), Ty::Scalar(to_scalar)) => {
                let op = self.convert_scalar(value.op, *from, *to_scalar, 1);
                Ok(Value { op, ty: to.clone() })
            }
            (Ty::Scalar(_), Ty::Vector(scalar, lanes)) => {
                let element = self.convert(value, &Ty::Scalar(*scalar), line)?;
                let op = self.splat(element.op, *scalar, *lanes);
                Ok(Value { op, ty: to.clone() })
            }
            (Ty::Vector(from, lanes), Ty::Vector(to_scalar, to_lanes)) if lanes == to_lanes => {
                let op = self.convert_scalar(value.op, *from, *to_scalar, *lanes);
                Ok(Value { op, ty: to.clone() })
            }
            (Ty::Struct(a), Ty::Struct(b)) if a == b => Ok(value),
            (Ty::Matrix { .. }, Ty::Matrix { .. }) => self.convert_matrix(value, to, line),
            (from, to) => Err(syntax_error(
                line,
                format!("no conversion from {} to {}", describe(from), describe(to)),
            )),
        }
    }

    pub fn splat(&mut self, element: Operand, scalar: Scalar, lanes: u8) -> Operand {
        if is_constant(&element) {
            return Operand::Composite {
                ty: Type::Vector {
                    element: Box::new(scalar_ir(scalar)),
                    lanes,
                },
                elements: vec![element; usize::from(lanes)],
            };
        }
        self.emit(
            Type::Vector {
                element: Box::new(scalar_ir(scalar)),
                lanes,
            },
            InstructionKind::VectorSplat {
                value: element,
                lanes,
            },
        )
    }

    /// A constant of `scalar` in `lanes` lanes.
    pub fn constant_of(&mut self, scalar: Scalar, lanes: u8, value: Number) -> Operand {
        let element = number_operand(scalar, value);
        if lanes == 1 {
            element
        } else {
            self.splat(element, scalar, lanes)
        }
    }

    fn convert_scalar(&mut self, op: Operand, from: Scalar, to: Scalar, lanes: u8) -> Operand {
        if from == to {
            return op;
        }
        if let Some(number) = constant_number(&op).filter(|_| lanes == 1) {
            return number_operand(to, number);
        }
        let result = if lanes == 1 {
            scalar_ir(to)
        } else {
            Type::Vector {
                element: Box::new(scalar_ir(to)),
                lanes,
            }
        };
        match (from, to) {
            (Scalar::Bool, Scalar::Int { .. } | Scalar::Float { .. }) => {
                let one = self.constant_of(to, lanes, Number::Int(1));
                let zero = self.constant_of(to, lanes, Number::Int(0));
                self.emit(
                    result,
                    InstructionKind::Select {
                        condition: op,
                        on_true: one,
                        on_false: zero,
                    },
                )
            }
            (Scalar::Int { .. }, Scalar::Bool) => {
                let zero = self.constant_of(from, lanes, Number::Int(0));
                self.emit(
                    result,
                    InstructionKind::IntegerCompare {
                        predicate: IntegerPredicate::NotEqual,
                        lhs: op,
                        rhs: zero,
                    },
                )
            }
            (Scalar::Float { .. }, Scalar::Bool) => {
                let zero = self.constant_of(from, lanes, Number::Float(0.0));
                self.emit(
                    result,
                    InstructionKind::FloatCompare {
                        predicate: FloatPredicate::UnorderedNotEqual,
                        lhs: op,
                        rhs: zero,
                    },
                )
            }
            (
                Scalar::Int {
                    signed,
                    bits: from_bits,
                },
                Scalar::Int { bits: to_bits, .. },
            ) => {
                if from_bits == to_bits {
                    return op;
                }
                let convert = if to_bits < from_bits {
                    ConvertOp::IntegerTruncate
                } else if signed {
                    ConvertOp::SignExtend
                } else {
                    ConvertOp::ZeroExtend
                };
                self.emit(
                    result,
                    InstructionKind::Convert {
                        op: convert,
                        value: op,
                    },
                )
            }
            (Scalar::Int { signed, .. }, Scalar::Float { .. }) => self.emit(
                result,
                InstructionKind::Convert {
                    op: if signed {
                        ConvertOp::SignedToFloat
                    } else {
                        ConvertOp::UnsignedToFloat
                    },
                    value: op,
                },
            ),
            (Scalar::Float { .. }, Scalar::Int { signed, .. }) => self.emit(
                result,
                InstructionKind::Convert {
                    op: if signed {
                        ConvertOp::FloatToSigned
                    } else {
                        ConvertOp::FloatToUnsigned
                    },
                    value: op,
                },
            ),
            (Scalar::Float { .. }, Scalar::Float { .. }) => self.emit(
                result,
                InstructionKind::Convert {
                    op: ConvertOp::FloatResize,
                    value: op,
                },
            ),
            (Scalar::Bool, Scalar::Bool) => op,
        }
    }

    /// The type two operands of an arithmetic operator meet at.
    pub fn common(&self, a: &Ty, b: &Ty, line: usize) -> Result<Ty> {
        let (Some(sa), Some(la), Some(sb), Some(lb)) =
            (a.scalar(), a.lanes(), b.scalar(), b.lanes())
        else {
            return Err(syntax_error(
                line,
                format!("no arithmetic on {} and {}", describe(a), describe(b)),
            ));
        };
        if la != lb && la != 1 && lb != 1 {
            return Err(syntax_error(line, "vectors of different lengths"));
        }
        let lanes = la.max(lb);
        let vector = lanes > 1;
        let scalar = match (sa, sb) {
            (Scalar::Float { bits: x }, Scalar::Float { bits: y }) => {
                Scalar::Float { bits: x.max(y) }
            }
            // A vector's element type wins over a scalar's.
            (Scalar::Float { .. }, _) if !vector || la > 1 => sa,
            (_, Scalar::Float { .. }) if !vector || lb > 1 => sb,
            (Scalar::Float { .. }, _) => sb,
            (_, Scalar::Float { .. }) => sa,
            _ if vector && la > 1 && lb == 1 => sa,
            _ if vector && lb > 1 && la == 1 => sb,
            (x, y) => {
                let promote = |s: Scalar| match s {
                    Scalar::Int { signed, bits } if bits >= 32 => Scalar::Int { signed, bits },
                    _ if vector => s,
                    _ => Scalar::INT,
                };
                let (x, y) = (promote(x), promote(y));
                match (x, y) {
                    (Scalar::Int { signed: a, bits: p }, Scalar::Int { signed: b, bits: q }) => {
                        Scalar::Int {
                            signed: a && b,
                            bits: p.max(q),
                        }
                    }
                    (x, _) => x,
                }
            }
        };
        Ok(Ty::of(scalar, lanes))
    }

    fn unary(&mut self, op: &str, operand: &Expr, line: usize) -> Result<Value> {
        if op == "++" || op == "--" {
            return self.increment(op, operand, true, line);
        }
        if op == "&" {
            return Err(syntax_error(line, "a pointer used as a value"));
        }
        if op == "*" {
            let lvalue = self.deref(operand, line)?;
            return self.load(&lvalue, line);
        }
        let value = self.rvalue(operand)?;
        if matches!(value.ty, Ty::Matrix { .. }) {
            return match op {
                "+" => Ok(value),
                "-" => self.negate_matrix(value, line),
                _ => Err(syntax_error(line, format!("unary {op} on a matrix"))),
            };
        }
        let (Some(scalar), Some(lanes)) = (value.ty.scalar(), value.ty.lanes()) else {
            return Err(syntax_error(
                line,
                format!("unary {op} on a non-numeric value"),
            ));
        };
        match op {
            "+" => Ok(value),
            "-" => {
                if let Some(number) = constant_number(&value.op) {
                    let negated = match number {
                        Number::Int(n) => Number::Int(-n),
                        Number::Float(f) => Number::Float(-f),
                    };
                    return Ok(Value {
                        op: number_operand(scalar, negated),
                        ty: value.ty,
                    });
                }
                let ty = self.context.ir(&value.ty);
                let op = match scalar {
                    Scalar::Float { .. } => self.emit(
                        ty,
                        InstructionKind::FloatUnary {
                            op: FloatUnaryOp::Negate,
                            value: value.op,
                        },
                    ),
                    Scalar::Int { .. } => {
                        let zero = self.constant_of(scalar, lanes, Number::Int(0));
                        self.emit(
                            ty,
                            InstructionKind::IntegerBinary {
                                op: IntegerBinaryOp::Subtract,
                                lhs: zero,
                                rhs: value.op,
                            },
                        )
                    }
                    Scalar::Bool => return Err(syntax_error(line, "negation of a bool")),
                };
                Ok(Value { op, ty: value.ty })
            }
            "!" => {
                let ty = Ty::of(Scalar::Bool, lanes);
                let value = self.convert(value, &ty, line)?;
                let truth = self.constant_of(Scalar::Bool, lanes, Number::Int(1));
                let op = self.emit(
                    self.context.ir(&ty),
                    InstructionKind::IntegerBinary {
                        op: IntegerBinaryOp::Xor,
                        lhs: value.op,
                        rhs: truth,
                    },
                );
                Ok(Value { op, ty })
            }
            "~" => {
                if !scalar.is_integer() {
                    return Err(syntax_error(line, "~ on a non-integer"));
                }
                let op = self.emit(
                    self.context.ir(&value.ty),
                    InstructionKind::IntegerUnary {
                        op: IntegerUnaryOp::Not,
                        value: value.op,
                    },
                );
                Ok(Value { op, ty: value.ty })
            }
            _ => Err(syntax_error(line, format!("unsupported operator {op}"))),
        }
    }

    fn increment(&mut self, op: &str, operand: &Expr, prefix: bool, line: usize) -> Result<Value> {
        let lvalue = self
            .lvalue(operand)?
            .ok_or_else(|| syntax_error(line, format!("{op} of something not assignable")))?;
        let old = self.load(&lvalue, line)?;
        let one = Value {
            op: Operand::u32(1),
            ty: Ty::Scalar(Scalar::INT),
        };
        let new = self.arithmetic(if op == "++" { "+" } else { "-" }, old.clone(), one, line)?;
        let new = self.convert(new, &lvalue.ty(), line)?;
        self.store(&lvalue, new.clone(), line)?;
        Ok(if prefix { new } else { old })
    }

    fn binary(&mut self, op: &str, lhs: &Expr, rhs: &Expr, line: usize) -> Result<Value> {
        if op == "&&" || op == "||" {
            return self.logical(op, lhs, rhs, line);
        }
        let a = self.rvalue(lhs)?;
        let b = self.rvalue(rhs)?;
        self.arithmetic(op, a, b, line)
    }

    /// `&&` and `||`, which skip their right operand where it could have an
    /// effect.
    fn logical(&mut self, op: &str, lhs: &Expr, rhs: &Expr, line: usize) -> Result<Value> {
        let bool_ty = Ty::Scalar(Scalar::Bool);
        // On vectors the operators are elementwise and evaluate both sides,
        // as for any vector of the C language extensions Metal is built on.
        let a = self.rvalue(lhs)?;
        if a.ty.lanes().is_some_and(|lanes| lanes > 1) {
            let b = self.rvalue(rhs)?;
            let ty = self.common(&a.ty, &b.ty, line)?;
            let lanes = ty.lanes().expect("numeric");
            let ty = Ty::of(Scalar::Bool, lanes);
            let a = self.convert(a, &ty, line)?;
            let b = self.convert(b, &ty, line)?;
            let op = self.emit(
                self.context.ir(&ty),
                InstructionKind::IntegerBinary {
                    op: if op == "&&" {
                        IntegerBinaryOp::And
                    } else {
                        IntegerBinaryOp::Or
                    },
                    lhs: a.op,
                    rhs: b.op,
                },
            );
            return Ok(Value { op, ty });
        }
        let a = self.convert(a, &bool_ty, line)?.op;
        if !has_effect(rhs) {
            let b = self.condition(rhs)?;
            let op = self.emit(
                Type::Bool,
                InstructionKind::IntegerBinary {
                    op: if op == "&&" {
                        IntegerBinaryOp::And
                    } else {
                        IntegerBinaryOp::Or
                    },
                    lhs: a,
                    rhs: b,
                },
            );
            return Ok(Value { op, ty: bool_ty });
        }
        let result = LValue::Memory(self.local(&bool_ty));
        self.store(
            &result,
            Value {
                op: a.clone(),
                ty: bool_ty.clone(),
            },
            line,
        )?;
        let evaluate = self.new_block("rhs");
        let merge = self.new_block("endlogical");
        let (true_block, false_block) = if op == "&&" {
            (evaluate, merge)
        } else {
            (merge, evaluate)
        };
        self.terminate(Terminator::CondBranch {
            condition: a,
            true_block,
            false_block,
        });
        self.switch_to(evaluate);
        let b = self.condition(rhs)?;
        self.store(&result, Value { op: b, ty: bool_ty }, line)?;
        self.terminate(Terminator::Branch(merge));
        self.switch_to(merge);
        self.load(&result, line)
    }

    /// A binary operator on two values.
    pub fn arithmetic(&mut self, op: &str, a: Value, b: Value, line: usize) -> Result<Value> {
        if op == "<<" || op == ">>" {
            let (Some(scalar), Some(lanes)) = (a.ty.scalar(), a.ty.lanes()) else {
                return Err(syntax_error(line, "a shift of a non-integer"));
            };
            let scalar = match scalar {
                Scalar::Int { bits, .. } if bits < 32 && lanes == 1 => Scalar::INT,
                Scalar::Int { .. } => scalar,
                _ => return Err(syntax_error(line, "a shift of a non-integer")),
            };
            let ty = Ty::of(scalar, lanes);
            let a = self.convert(a, &ty, line)?;
            let b = self.convert(b, &ty, line)?;
            let shift = match (op, scalar.is_signed()) {
                ("<<", _) => IntegerBinaryOp::ShiftLeft,
                (_, true) => IntegerBinaryOp::ShiftRightArithmetic,
                _ => IntegerBinaryOp::ShiftRightLogical,
            };
            let op = self.emit(
                self.context.ir(&ty),
                InstructionKind::IntegerBinary {
                    op: shift,
                    lhs: a.op,
                    rhs: b.op,
                },
            );
            return Ok(Value { op, ty });
        }
        if matches!(a.ty, Ty::Matrix { .. }) || matches!(b.ty, Ty::Matrix { .. }) {
            return self.matrix_arithmetic(op, a, b, line);
        }
        let ty = self.common(&a.ty, &b.ty, line)?;
        let a = self.convert(a, &ty, line)?;
        let b = self.convert(b, &ty, line)?;
        let scalar = ty.scalar().expect("numeric");
        let lanes = ty.lanes().expect("numeric");
        let ir = self.context.ir(&ty);
        let bool_ty = Ty::of(Scalar::Bool, lanes);
        let bool_ir = self.context.ir(&bool_ty);
        let compare = matches!(op, "==" | "!=" | "<" | ">" | "<=" | ">=");
        let kind = match scalar {
            Scalar::Float { .. } => {
                if compare {
                    let predicate = match op {
                        "==" => FloatPredicate::OrderedEqual,
                        // C's != is true where either side is NaN.
                        "!=" => FloatPredicate::UnorderedNotEqual,
                        "<" => FloatPredicate::OrderedLessThan,
                        ">" => FloatPredicate::OrderedGreaterThan,
                        "<=" => FloatPredicate::OrderedLessEqual,
                        _ => FloatPredicate::OrderedGreaterEqual,
                    };
                    let op = self.emit(
                        bool_ir,
                        InstructionKind::FloatCompare {
                            predicate,
                            lhs: a.op,
                            rhs: b.op,
                        },
                    );
                    return Ok(Value { op, ty: bool_ty });
                }
                let float_op = match op {
                    "+" => FloatBinaryOp::Add,
                    "-" => FloatBinaryOp::Subtract,
                    "*" => FloatBinaryOp::Multiply,
                    "/" => FloatBinaryOp::Divide,
                    _ => {
                        return Err(syntax_error(
                            line,
                            format!("operator {op} on floating values"),
                        ));
                    }
                };
                InstructionKind::FloatBinary {
                    op: float_op,
                    lhs: a.op,
                    rhs: b.op,
                }
            }
            Scalar::Int { signed, .. } => {
                if compare {
                    let predicate = match (op, signed) {
                        ("==", _) => IntegerPredicate::Equal,
                        ("!=", _) => IntegerPredicate::NotEqual,
                        ("<", true) => IntegerPredicate::SignedLessThan,
                        (">", true) => IntegerPredicate::SignedGreaterThan,
                        ("<=", true) => IntegerPredicate::SignedLessEqual,
                        (">=", true) => IntegerPredicate::SignedGreaterEqual,
                        ("<", false) => IntegerPredicate::UnsignedLessThan,
                        (">", false) => IntegerPredicate::UnsignedGreaterThan,
                        ("<=", false) => IntegerPredicate::UnsignedLessEqual,
                        _ => IntegerPredicate::UnsignedGreaterEqual,
                    };
                    let op = self.emit(
                        bool_ir,
                        InstructionKind::IntegerCompare {
                            predicate,
                            lhs: a.op,
                            rhs: b.op,
                        },
                    );
                    return Ok(Value { op, ty: bool_ty });
                }
                let int_op = match (op, signed) {
                    ("+", _) => IntegerBinaryOp::Add,
                    ("-", _) => IntegerBinaryOp::Subtract,
                    ("*", _) => IntegerBinaryOp::Multiply,
                    ("/", true) => IntegerBinaryOp::DivideSigned,
                    ("/", false) => IntegerBinaryOp::DivideUnsigned,
                    ("%", true) => IntegerBinaryOp::RemainderSigned,
                    ("%", false) => IntegerBinaryOp::RemainderUnsigned,
                    ("&", _) => IntegerBinaryOp::And,
                    ("|", _) => IntegerBinaryOp::Or,
                    ("^", _) => IntegerBinaryOp::Xor,
                    _ => return Err(syntax_error(line, format!("unsupported operator {op}"))),
                };
                InstructionKind::IntegerBinary {
                    op: int_op,
                    lhs: a.op,
                    rhs: b.op,
                }
            }
            Scalar::Bool => {
                let bool_op = match op {
                    "&" => IntegerBinaryOp::And,
                    "|" => IntegerBinaryOp::Or,
                    "^" | "!=" => IntegerBinaryOp::Xor,
                    "==" => {
                        let differ = self.emit(
                            ir.clone(),
                            InstructionKind::IntegerBinary {
                                op: IntegerBinaryOp::Xor,
                                lhs: a.op,
                                rhs: b.op,
                            },
                        );
                        let truth = self.constant_of(Scalar::Bool, lanes, Number::Int(1));
                        let op = self.emit(
                            ir,
                            InstructionKind::IntegerBinary {
                                op: IntegerBinaryOp::Xor,
                                lhs: differ,
                                rhs: truth,
                            },
                        );
                        return Ok(Value { op, ty });
                    }
                    _ => {
                        // Arithmetic on bools promotes them to int.
                        let int = Ty::of(Scalar::INT, lanes);
                        let a = self.convert(a, &int, line)?;
                        let b = self.convert(b, &int, line)?;
                        return self.arithmetic(op, a, b, line);
                    }
                };
                InstructionKind::IntegerBinary {
                    op: bool_op,
                    lhs: a.op,
                    rhs: b.op,
                }
            }
        };
        let op = self.emit(ir, kind);
        Ok(Value { op, ty })
    }

    fn assign(&mut self, op: Option<&str>, lhs: &Expr, rhs: &Expr, line: usize) -> Result<Value> {
        let lvalue = self
            .lvalue(lhs)?
            .ok_or_else(|| syntax_error(line, "an assignment to something not assignable"))?;
        let ty = lvalue.ty();
        let value = match op {
            None => {
                if let (Ty::Struct(index), LValue::Fields(..)) = (&ty, &lvalue) {
                    let fields = self.struct_fields(rhs, *index, line)?;
                    let LValue::Fields(_, places) = &lvalue else {
                        unreachable!()
                    };
                    for (place, field) in places.clone().iter().zip(fields) {
                        self.store(place, field, line)?;
                    }
                    return self.load(&lvalue, line);
                }
                self.rvalue(rhs)?
            }
            Some(op) => {
                let old = self.load(&lvalue, line)?;
                let rhs = self.rvalue(rhs)?;
                self.arithmetic(op, old, rhs, line)?
            }
        };
        let value = self.convert(value, &ty, line)?;
        self.store(&lvalue, value.clone(), line)?;
        Ok(value)
    }

    fn ternary(&mut self, condition: &Expr, yes: &Expr, no: &Expr, line: usize) -> Result<Value> {
        let condition_value = self.rvalue(condition)?;
        if !has_effect(yes) && !has_effect(no) {
            let a = self.rvalue(yes)?;
            let b = self.rvalue(no)?;
            let ty = if a.ty == b.ty {
                a.ty.clone()
            } else {
                self.common(&a.ty, &b.ty, line)?
            };
            let a = self.convert(a, &ty, line)?;
            let b = self.convert(b, &ty, line)?;
            // A vector condition selects lane by lane.
            let condition = match condition_value.ty.lanes() {
                Some(1) => {
                    self.convert(condition_value, &Ty::Scalar(Scalar::Bool), line)?
                        .op
                }
                Some(lanes) if Some(lanes) == ty.lanes() => {
                    self.convert(condition_value, &Ty::of(Scalar::Bool, lanes), line)?
                        .op
                }
                _ => return Err(syntax_error(line, "a ?: condition of the wrong shape")),
            };
            let op = self.emit(
                self.context.ir(&ty),
                InstructionKind::Select {
                    condition,
                    on_true: a.op,
                    on_false: b.op,
                },
            );
            return Ok(Value { op, ty });
        }
        let condition = self
            .convert(condition_value, &Ty::Scalar(Scalar::Bool), line)?
            .op;
        let yes_block = self.new_block("then");
        let no_block = self.new_block("else");
        let merge = self.new_block("endternary");
        self.terminate(Terminator::CondBranch {
            condition,
            true_block: yes_block,
            false_block: no_block,
        });
        self.switch_to(yes_block);
        let a = self.rvalue(yes)?;
        let ty = a.ty.clone();
        let result = self.local_lvalue(&ty);
        self.store(&result, a, line)?;
        self.terminate(Terminator::Branch(merge));
        self.switch_to(no_block);
        let b = self.rvalue(no)?;
        let b = self.convert(b, &ty, line)?;
        self.store(&result, b, line)?;
        self.terminate(Terminator::Branch(merge));
        self.switch_to(merge);
        self.load(&result, line)
    }

    /// `T(args)`.
    fn construct(&mut self, ty: &Ty, args: &[Expr], line: usize) -> Result<Value> {
        match ty {
            Ty::Scalar(_) => match args {
                [arg] => {
                    let value = self.rvalue(arg)?;
                    if value.ty.lanes() != Some(1) {
                        return Err(syntax_error(line, "a scalar constructed from a vector"));
                    }
                    self.convert(value, ty, line)
                }
                [] => Ok(Value {
                    op: number_operand(ty.scalar().expect("scalar"), Number::Int(0)),
                    ty: ty.clone(),
                }),
                _ => Err(syntax_error(
                    line,
                    "a scalar constructed from several values",
                )),
            },
            Ty::Vector(scalar, lanes) => {
                if args.is_empty() {
                    let op = self.constant_of(*scalar, *lanes, Number::Int(0));
                    return Ok(Value { op, ty: ty.clone() });
                }
                let values = args
                    .iter()
                    .map(|arg| self.rvalue(arg))
                    .collect::<Result<Vec<_>>>()?;
                if let [value] = values.as_slice() {
                    match value.ty.lanes() {
                        Some(1) => return self.convert(value.clone(), ty, line),
                        Some(n) if n == *lanes => return self.convert(value.clone(), ty, line),
                        _ => {}
                    }
                }
                let mut elements = Vec::new();
                for value in values {
                    match value.ty {
                        Ty::Scalar(_) => {
                            elements.push(self.convert(value, &Ty::Scalar(*scalar), line)?.op)
                        }
                        Ty::Vector(_, n) => {
                            let value = self.convert(value, &Ty::Vector(*scalar, n), line)?;
                            for lane in 0..u32::from(n) {
                                elements.push(
                                    if let Operand::Composite { elements, .. } = &value.op {
                                        elements[lane as usize].clone()
                                    } else {
                                        self.emit(
                                            scalar_ir(*scalar),
                                            InstructionKind::CompositeExtract {
                                                composite: value.op.clone(),
                                                index: lane,
                                            },
                                        )
                                    },
                                );
                            }
                        }
                        _ => {
                            return Err(syntax_error(
                                line,
                                "a vector constructed from a non-numeric value",
                            ));
                        }
                    }
                }
                if elements.len() != usize::from(*lanes) {
                    return Err(syntax_error(
                        line,
                        format!("{} values construct a {lanes}-lane vector", elements.len()),
                    ));
                }
                let ir = self.context.ir(ty);
                if elements.iter().all(is_constant) {
                    return Ok(Value {
                        op: Operand::Composite { ty: ir, elements },
                        ty: ty.clone(),
                    });
                }
                let mut op = Operand::Undef(ir.clone());
                for (lane, element) in elements.into_iter().enumerate() {
                    op = self.emit(
                        ir.clone(),
                        InstructionKind::CompositeInsert {
                            composite: op,
                            object: element,
                            index: lane as u32,
                        },
                    );
                }
                Ok(Value { op, ty: ty.clone() })
            }
            Ty::Matrix { .. } => self.construct_matrix(ty, args, line),
            Ty::Struct(index) => {
                let items = args
                    .iter()
                    .map(|arg| (None, arg.clone()))
                    .collect::<Vec<_>>();
                self.compound_literal(&Ty::Struct(*index), &items, line)
            }
            _ => Err(syntax_error(
                line,
                format!("cannot construct a {}", describe(ty)),
            )),
        }
    }

    /// `(T){ .a = x }`: a structure value; a field not named is zero.
    fn compound_literal(
        &mut self,
        ty: &Ty,
        items: &[(Option<String>, Expr)],
        line: usize,
    ) -> Result<Value> {
        let Ty::Struct(index) = ty else {
            if let [(None, item)] = items {
                let value = self.rvalue(item)?;
                return self.convert(value, ty, line);
            }
            return Err(syntax_error(line, "a compound literal of a non-structure"));
        };
        let fields = self.context.program.structs[*index].fields.clone();
        let mut values: Vec<Option<Value>> = vec![None; fields.len()];
        let mut next = 0usize;
        for (name, item) in items {
            let position = match name {
                Some(name) => self.field_position(*index, name, line)?,
                None => next,
            };
            let field = fields
                .get(position)
                .ok_or_else(|| syntax_error(line, "too many initializers"))?;
            let value = self.rvalue(item)?;
            values[position] = Some(self.convert(value, &field.ty.unpacked(), line)?);
            next = position + 1;
        }
        let ir = self.context.ir(ty);
        let mut op = Operand::Undef(ir.clone());
        for (position, (field, value)) in fields.iter().zip(values).enumerate() {
            let value = match value {
                Some(value) => self.pack(value, &field.ty),
                None => self.zero(&field.ty, line)?,
            };
            op = self.emit(
                ir.clone(),
                InstructionKind::CompositeInsert {
                    composite: op,
                    object: value,
                    index: position as u32,
                },
            );
        }
        Ok(Value { op, ty: ty.clone() })
    }

    pub(super) fn zero(&mut self, ty: &Ty, line: usize) -> Result<Operand> {
        match ty {
            Ty::Scalar(scalar) => Ok(number_operand(*scalar, Number::Int(0))),
            Ty::Vector(scalar, lanes) => Ok(self.constant_of(*scalar, *lanes, Number::Int(0))),
            Ty::Matrix { .. } => Ok(self.construct_matrix(ty, &[], line)?.op),
            Ty::ByteBool => Ok(number_operand(Scalar::UCHAR, Number::Int(0))),
            Ty::Packed(scalar, lanes) => {
                let element = self.zero(&Ty::Scalar(*scalar), line)?;
                Ok(Operand::Composite {
                    ty: self.context.ir(ty),
                    elements: vec![element; usize::from(*lanes)],
                })
            }
            Ty::Array(element, count) => {
                let element = self.zero(element, line)?;
                Ok(Operand::Composite {
                    ty: self.context.ir(ty),
                    elements: vec![element; *count as usize],
                })
            }
            _ => Err(syntax_error(
                line,
                "a field left out of a compound literal must be numeric",
            )),
        }
    }
}

/// The element type and count of an aggregate in `thread` or `threadgroup`
/// memory, which is moved one element at a time: a variable there has no
/// explicit layout, and so has a different Vulkan type from the laid-out
/// aggregate value a load or store would name.
fn local_elements(place: &Place) -> Option<(Ty, u32)> {
    if !matches!(
        place.space,
        AddressSpace::Thread | AddressSpace::Threadgroup
    ) {
        return None;
    }
    match &place.ty {
        Ty::Array(element, count) => Some(((**element).clone(), *count)),
        Ty::Matrix { columns, .. } => Some((place.ty.column()?, u32::from(*columns))),
        Ty::Packed(scalar, lanes) => Some((Ty::Scalar(*scalar), u32::from(*lanes))),
        _ => None,
    }
}

/// Element `index` of an aggregate place.
fn element_of(place: &Place, index: u32, element: &Ty) -> LValue {
    let mut indices = place.indices.clone();
    if indices.is_empty() {
        indices.push(Operand::u32(0));
    }
    indices.push(Operand::u32(index));
    LValue::Memory(Place {
        indices,
        ty: element.clone(),
        ..place.clone()
    })
}

/// The lanes a swizzle names: `.xyzw` or `.rgba`, never mixed.
pub(super) fn swizzle_lanes(name: &str, lanes: u8) -> Option<Vec<u32>> {
    if name.is_empty() || name.len() > 4 {
        return None;
    }
    let sets = ["xyzw", "rgba"];
    sets.iter().find_map(|set| {
        name.chars()
            .map(|c| {
                set.find(c)
                    .map(|lane| lane as u32)
                    .filter(|lane| *lane < u32::from(lanes))
            })
            .collect::<Option<Vec<_>>>()
    })
}

pub(super) fn is_constant(op: &Operand) -> bool {
    matches!(
        op,
        Operand::Integer { .. }
            | Operand::Float { .. }
            | Operand::Bool(_)
            | Operand::Composite { .. }
    )
}

fn constant_number(op: &Operand) -> Option<Number> {
    match op {
        Operand::Integer {
            ty: Type::Int { bits, .. },
            value,
        } => {
            // A signless constant reads back as the signed value of its width.
            let shift = 64 - u32::from(*bits);
            Some(Number::Int(((*value << shift) as i64) >> shift))
        }
        Operand::Bool(value) => Some(Number::Int(i64::from(*value))),
        Operand::Float {
            ty: Type::Float { bits: 32 },
            bits,
        } => Some(Number::Float(f64::from(f32::from_bits(*bits as u32)))),
        _ => None,
    }
}

/// Whether evaluating an expression could change anything, so that it must
/// not be evaluated when C++ would skip it.
pub(super) fn has_effect(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Assign(..) | ExprKind::Postfix(..) => true,
        ExprKind::Unary(op, inner) => matches!(*op, "++" | "--") || has_effect(inner),
        ExprKind::Call { name, args, .. } => {
            matches!(
                name.as_str(),
                "discard_fragment" | "threadgroup_barrier" | "simdgroup_barrier"
            ) || name.starts_with("atomic_")
                || args.iter().any(has_effect)
        }
        ExprKind::Method { name, object, args } => {
            name == "write" || has_effect(object) || args.iter().any(has_effect)
        }
        ExprKind::Binary(_, a, b) => has_effect(a) || has_effect(b),
        ExprKind::Ternary(a, b, c) => has_effect(a) || has_effect(b) || has_effect(c),
        ExprKind::Construct(_, args) | ExprKind::InitList(args) => args.iter().any(has_effect),
        ExprKind::Cast(_, inner)
        | ExprKind::AtomicCast(_, inner)
        | ExprKind::PointerCast(_, _, inner)
        | ExprKind::Member(inner, _) => has_effect(inner),
        ExprKind::Index(a, b) => has_effect(a) || has_effect(b),
        ExprKind::CompoundLiteral(_, items) => items.iter().any(|(_, item)| has_effect(item)),
        ExprKind::Ident(_) | ExprKind::Int { .. } | ExprKind::Float { .. } | ExprKind::Bool(_) => {
            false
        }
    }
}

pub(super) fn describe(ty: &Ty) -> String {
    let scalar = |s: &Scalar| match s {
        Scalar::Bool => "bool".to_string(),
        Scalar::Int { signed, bits } => format!("{}int{bits}", if *signed { "" } else { "u" }),
        Scalar::Float { bits: 16 } => "half".to_string(),
        Scalar::Float { .. } => "float".to_string(),
    };
    match ty {
        Ty::Void => "void".into(),
        Ty::Scalar(s) => scalar(s),
        Ty::Vector(s, lanes) => format!("{}{lanes}", scalar(s)),
        Ty::Matrix {
            scalar: s,
            columns,
            rows,
        } => format!("{}{columns}x{rows}", scalar(s)),
        Ty::Packed(s, lanes) => format!("packed_{}{lanes}", scalar(s)),
        Ty::ByteBool => "bool".into(),
        Ty::Param(name) => name.clone(),
        Ty::Array(element, count) => format!("{}[{count}]", describe(element)),
        Ty::Struct(_) => "a structure".into(),
        Ty::Texture(_) => "a texture".into(),
        Ty::Sampler => "a sampler".into(),
    }
}
