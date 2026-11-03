//! Pointers, and the buffers of bytes a cast views as other types.
//!
//! Metal reads any type at any byte offset of a `char` or `void` buffer, but
//! SPIR-V's logical addressing types every access. Such a view is therefore
//! read and written a byte at a time, assembling each scalar from its bytes
//! in the little-endian order of Metal's memory. Only loads, stores and
//! conversions touch the bytes themselves, which 8-bit storage allows without
//! 8-bit arithmetic.

use super::*;
use crate::ir::{ConvertOp, IntegerBinaryOp, IntegerPredicate};

#[derive(Clone, Debug)]
pub(in crate::msl) enum Pointer {
    /// Into a buffer of `place.ty`s, at the element `place.indices` holds
    /// or, with no index, the first.
    Elements(Place),
    Bytes(Bytes),
}

impl Pointer {
    pub fn space(&self) -> AddressSpace {
        match self {
            Pointer::Elements(place) => place.space,
            Pointer::Bytes(bytes) => bytes.space,
        }
    }

    /// The type pointed to, as memory holds it.
    pub fn element(&self) -> &Ty {
        match self {
            Pointer::Elements(place) => &place.ty,
            Pointer::Bytes(bytes) => &bytes.ty,
        }
    }
}

/// A `ty` at a byte offset into a buffer of bytes.
#[derive(Clone, Debug)]
pub(in crate::msl) struct Bytes {
    /// The buffer, a pointer to bytes.
    pub root: Operand,
    pub space: AddressSpace,
    /// A 32-bit integer.
    pub offset: Operand,
    pub ty: Ty,
}

impl Bytes {
    fn at(&self, offset: Operand, ty: Ty) -> Bytes {
        Bytes {
            offset,
            ty,
            ..self.clone()
        }
    }
}

const BYTE: Type = Type::Int {
    bits: 8,
    signed: false,
};

/// Whether a buffer of `ty` is one of bytes, which a cast may view as any
/// other type.
fn is_bytes(ty: &Ty) -> bool {
    matches!(ty, Ty::Void | Ty::Scalar(Scalar::Int { bits: 8, .. }))
}

impl Lowerer<'_, '_> {
    /// The pointer an expression evaluates to; `None`, having emitted
    /// nothing, for an expression which is not one.
    pub fn pointer(&mut self, expr: &Expr) -> Result<Option<Pointer>> {
        let line = expr.line;
        Ok(Some(match &expr.kind {
            ExprKind::Ident(name) => match self.lookup(name) {
                Some(Binding::Pointer(pointer)) => pointer,
                _ => return Ok(None),
            },
            ExprKind::Binary(op @ ("+" | "-"), lhs, rhs) => {
                let (pointer, steps) = if let Some(pointer) = self.pointer(lhs)? {
                    (pointer, rhs)
                } else if *op == "+"
                    && let Some(pointer) = self.pointer(rhs)?
                {
                    (pointer, lhs)
                } else {
                    return Ok(None);
                };
                let mut steps = self.index_operand(steps)?;
                if *op == "-" {
                    steps = self.emit(
                        Type::u32(),
                        InstructionKind::IntegerBinary {
                            op: IntegerBinaryOp::Subtract,
                            lhs: Operand::u32(0),
                            rhs: steps,
                        },
                    );
                }
                self.step(pointer, steps, line)?
            }
            ExprKind::Unary("&", operand) => match self.lvalue(operand)? {
                Some(LValue::Bytes(bytes)) => Pointer::Bytes(bytes),
                Some(LValue::Memory(place))
                    if place.indices.len() == 1
                        && matches!(place.space, AddressSpace::Device | AddressSpace::Constant)
                        && !matches!(place.root, Operand::Global { .. }) =>
                {
                    Pointer::Elements(place)
                }
                _ => {
                    return Err(syntax_error(
                        line,
                        "the address of something other than a buffer's element",
                    ));
                }
            },
            ExprKind::PointerCast(space, ty, operand) => {
                let pointer = self.pointer(operand)?.ok_or_else(|| {
                    syntax_error(line, "a pointer cast of something not a pointer")
                })?;
                if let Some(space) = space
                    && address_space(*space) != pointer.space()
                {
                    return Err(syntax_error(
                        line,
                        "a pointer cast to another address space",
                    ));
                }
                let ty = self.resolve(ty, line)?.in_memory();
                self.cast(pointer, ty, line)?
            }
            _ => return Ok(None),
        }))
    }

    /// `pointer` viewed as a pointer to `ty`.
    fn cast(&mut self, pointer: Pointer, ty: Ty, line: usize) -> Result<Pointer> {
        if *pointer.element() == ty {
            return Ok(pointer);
        }
        match pointer {
            Pointer::Bytes(bytes) => Ok(Pointer::Bytes(Bytes { ty, ..bytes })),
            Pointer::Elements(place) if is_bytes(&place.ty) => Ok(Pointer::Bytes(Bytes {
                offset: place.indices.first().cloned().unwrap_or(Operand::u32(0)),
                root: place.root,
                space: place.space,
                ty,
            })),
            Pointer::Elements(place) => Err(syntax_error(
                line,
                format!(
                    "a cast of a pointer to {} to one to {}, which only a buffer of bytes allows",
                    expr::describe(&place.ty),
                    expr::describe(&ty)
                ),
            )),
        }
    }

    /// `pointer + steps`.
    fn step(&mut self, pointer: Pointer, steps: Operand, line: usize) -> Result<Pointer> {
        Ok(match pointer {
            Pointer::Elements(place) => {
                if place.ty == Ty::Void {
                    return Err(syntax_error(line, "arithmetic on a void pointer"));
                }
                let index = match place.indices.first() {
                    Some(base) => self.add(base.clone(), steps),
                    None => steps,
                };
                Pointer::Elements(Place {
                    indices: vec![index],
                    ..place
                })
            }
            Pointer::Bytes(bytes) => {
                let offset = self.byte_offset(&bytes, steps, line)?;
                Pointer::Bytes(bytes.at(offset, bytes.ty.clone()))
            }
        })
    }

    /// `pointer[index]`.
    pub fn index_pointer(&mut self, pointer: Pointer, index: &Expr, line: usize) -> Result<LValue> {
        let index = self.index_operand(index)?;
        self.element_at(pointer, index, line)
    }

    /// `*pointer`.
    pub fn deref(&mut self, operand: &Expr, line: usize) -> Result<LValue> {
        let pointer = self
            .pointer(operand)?
            .ok_or_else(|| syntax_error(line, "`*` of something not a pointer"))?;
        self.element_at(pointer, Operand::u32(0), line)
    }

    fn element_at(&mut self, pointer: Pointer, index: Operand, line: usize) -> Result<LValue> {
        if *pointer.element() == Ty::Void {
            return Err(syntax_error(line, "a void pointer dereferenced"));
        }
        Ok(match self.step(pointer, index, line)? {
            Pointer::Elements(place) => LValue::Memory(Place {
                indices: if place.indices.is_empty() {
                    vec![Operand::u32(0)]
                } else {
                    place.indices
                },
                ..place
            }),
            Pointer::Bytes(bytes) => LValue::Bytes(bytes),
        })
    }

    /// The offset `steps` elements of `bytes.ty` past `bytes`.
    fn byte_offset(&mut self, bytes: &Bytes, steps: Operand, line: usize) -> Result<Operand> {
        if bytes.ty == Ty::Void {
            return Err(syntax_error(line, "arithmetic on a void pointer"));
        }
        let (size, align) = self.context.size_align(&bytes.ty, line)?;
        let stride = self.multiply(steps, size.next_multiple_of(align));
        Ok(self.add(bytes.offset.clone(), stride))
    }

    /// A field of a structure, or an element of an array or matrix, viewed
    /// in a buffer of bytes.
    pub fn bytes_member(&mut self, bytes: &Bytes, member: &str, line: usize) -> Result<LValue> {
        let Ty::Struct(index) = bytes.ty else {
            unreachable!("only a structure has named members");
        };
        let position = self.field_position(index, member, line)?;
        let offset = self.context.layouts[index].fields[position].offset;
        let ty = self.context.program.structs[index].fields[position]
            .ty
            .clone();
        let offset = self.add(bytes.offset.clone(), Operand::u32(offset));
        Ok(LValue::Bytes(bytes.at(offset, ty)))
    }

    pub fn bytes_element(&mut self, bytes: &Bytes, index: Operand, line: usize) -> Result<LValue> {
        let element = match &bytes.ty {
            Ty::Array(element, _) => (**element).clone(),
            ty => ty.column().expect("an array or a matrix"),
        };
        let offset = self.byte_offset(
            &bytes.at(bytes.offset.clone(), element.clone()),
            index,
            line,
        )?;
        Ok(LValue::Bytes(bytes.at(offset, element)))
    }

    pub fn load_bytes(&mut self, bytes: &Bytes, line: usize) -> Result<Value> {
        let op = self.read_memory(bytes, line)?;
        Ok(self.unpack(Value {
            op,
            ty: bytes.ty.clone(),
        }))
    }

    pub fn store_bytes(&mut self, bytes: &Bytes, value: Value, line: usize) -> Result<()> {
        if bytes.space == AddressSpace::Constant {
            return Err(syntax_error(line, "an assignment to constant memory"));
        }
        let op = self.pack(value, &bytes.ty);
        self.write_memory(bytes, op, line)
    }

    /// The value at `bytes`, as `ir` types memory of `bytes.ty`.
    fn read_memory(&mut self, bytes: &Bytes, line: usize) -> Result<Operand> {
        if let Some(scalar) = memory_scalar(&bytes.ty) {
            return Ok(self.read_scalar(bytes, scalar));
        }
        let parts = self.parts(&bytes.ty, line)?;
        let ir = self.context.ir(&bytes.ty);
        let mut op = Operand::Undef(ir.clone());
        for (index, (offset, ty)) in parts.into_iter().enumerate() {
            let offset = self.add(bytes.offset.clone(), Operand::u32(offset));
            let part = self.read_memory(&bytes.at(offset, ty), line)?;
            op = self.emit(
                ir.clone(),
                InstructionKind::CompositeInsert {
                    composite: op,
                    object: part,
                    index: index as u32,
                },
            );
        }
        Ok(op)
    }

    fn write_memory(&mut self, bytes: &Bytes, value: Operand, line: usize) -> Result<()> {
        if let Some(scalar) = memory_scalar(&bytes.ty) {
            self.write_scalar(bytes, scalar, value);
            return Ok(());
        }
        let parts = self.parts(&bytes.ty, line)?;
        for (index, (offset, ty)) in parts.into_iter().enumerate() {
            let part = self.emit(
                self.context.ir(&ty),
                InstructionKind::CompositeExtract {
                    composite: value.clone(),
                    index: index as u32,
                },
            );
            let offset = self.add(bytes.offset.clone(), Operand::u32(offset));
            self.write_memory(&bytes.at(offset, ty), part, line)?;
        }
        Ok(())
    }

    /// The members of an aggregate in memory: each one's offset and type.
    fn parts(&self, ty: &Ty, line: usize) -> Result<Vec<(u32, Ty)>> {
        let repeat = |element: Ty, count: u32| -> Result<Vec<(u32, Ty)>> {
            let (size, align) = self.context.size_align(&element, line)?;
            let stride = size.next_multiple_of(align);
            Ok((0..count)
                .map(|index| (index * stride, element.clone()))
                .collect())
        };
        match ty {
            Ty::Vector(scalar, lanes) | Ty::Packed(scalar, lanes) => {
                repeat(Ty::Scalar(*scalar), u32::from(*lanes))
            }
            Ty::Array(element, count) => repeat((**element).clone(), *count),
            Ty::Matrix { columns, .. } => {
                repeat(ty.column().expect("a matrix"), u32::from(*columns))
            }
            Ty::Struct(index) => Ok(self.context.layouts[*index]
                .fields
                .iter()
                .zip(&self.context.program.structs[*index].fields)
                .map(|(layout, field)| (layout.offset, field.ty.clone()))
                .collect()),
            ty => Err(syntax_error(
                line,
                format!("{} viewed in a buffer of bytes", expr::describe(ty)),
            )),
        }
    }

    fn read_scalar(&mut self, bytes: &Bytes, scalar: Scalar) -> Operand {
        let size = scalar_size(scalar);
        let wide = wide_type(size);
        let mut assembled = None;
        for index in 0..size {
            let byte = self.read_byte(bytes, index);
            let mut part = self.emit(
                wide.clone(),
                InstructionKind::Convert {
                    op: ConvertOp::ZeroExtend,
                    value: byte,
                },
            );
            if index > 0 {
                part = self.emit(
                    wide.clone(),
                    InstructionKind::IntegerBinary {
                        op: IntegerBinaryOp::ShiftLeft,
                        lhs: part,
                        rhs: Operand::Integer {
                            ty: wide.clone(),
                            value: u64::from(index * 8),
                        },
                    },
                );
            }
            assembled = Some(match assembled {
                Some(lhs) => self.emit(
                    wide.clone(),
                    InstructionKind::IntegerBinary {
                        op: IntegerBinaryOp::Or,
                        lhs,
                        rhs: part,
                    },
                ),
                None => part,
            });
        }
        let assembled = assembled.expect("a scalar has a byte");
        match scalar {
            Scalar::Bool => self.emit(
                Type::Bool,
                InstructionKind::IntegerCompare {
                    predicate: IntegerPredicate::NotEqual,
                    lhs: assembled,
                    rhs: Operand::Integer { ty: wide, value: 0 },
                },
            ),
            _ => {
                let bits = scalar.bits();
                let int = Type::Int {
                    bits,
                    signed: false,
                };
                let op = if int == wide {
                    assembled
                } else {
                    self.emit(
                        int,
                        InstructionKind::Convert {
                            op: ConvertOp::IntegerTruncate,
                            value: assembled,
                        },
                    )
                };
                match scalar {
                    Scalar::Float { bits } => self.emit(
                        Type::Float { bits },
                        InstructionKind::Convert {
                            op: ConvertOp::Bitcast,
                            value: op,
                        },
                    ),
                    _ => op,
                }
            }
        }
    }

    fn write_scalar(&mut self, bytes: &Bytes, scalar: Scalar, value: Operand) {
        let size = scalar_size(scalar);
        let wide = wide_type(size);
        let bits = match scalar {
            Scalar::Bool => self.emit(
                wide.clone(),
                InstructionKind::Select {
                    condition: value,
                    on_true: Operand::Integer {
                        ty: wide.clone(),
                        value: 1,
                    },
                    on_false: Operand::Integer {
                        ty: wide.clone(),
                        value: 0,
                    },
                },
            ),
            _ => {
                let int = Type::Int {
                    bits: scalar.bits(),
                    signed: false,
                };
                let value = match scalar {
                    Scalar::Float { .. } => self.emit(
                        int.clone(),
                        InstructionKind::Convert {
                            op: ConvertOp::Bitcast,
                            value,
                        },
                    ),
                    _ => value,
                };
                if int == wide {
                    value
                } else {
                    self.emit(
                        wide.clone(),
                        InstructionKind::Convert {
                            op: ConvertOp::ZeroExtend,
                            value,
                        },
                    )
                }
            }
        };
        for index in 0..size {
            let part = if index == 0 {
                bits.clone()
            } else {
                self.emit(
                    wide.clone(),
                    InstructionKind::IntegerBinary {
                        op: IntegerBinaryOp::ShiftRightLogical,
                        lhs: bits.clone(),
                        rhs: Operand::Integer {
                            ty: wide.clone(),
                            value: u64::from(index * 8),
                        },
                    },
                )
            };
            let byte = self.emit(
                BYTE,
                InstructionKind::Convert {
                    op: ConvertOp::IntegerTruncate,
                    value: part,
                },
            );
            let pointer = self.byte_address(bytes, index);
            self.emit_void(InstructionKind::Store {
                pointer,
                value: byte,
            });
        }
    }

    fn read_byte(&mut self, bytes: &Bytes, index: u32) -> Operand {
        let pointer = self.byte_address(bytes, index);
        self.emit(BYTE, InstructionKind::Load { pointer })
    }

    fn byte_address(&mut self, bytes: &Bytes, index: u32) -> Operand {
        let offset = self.add(bytes.offset.clone(), Operand::u32(index));
        self.emit(
            Type::Pointer {
                space: bytes.space,
                pointee: Box::new(BYTE),
            },
            InstructionKind::BufferAddress {
                pointer: bytes.root.clone(),
                indices: vec![offset],
            },
        )
    }

    /// The sum of two 32-bit integers.
    fn add(&mut self, lhs: Operand, rhs: Operand) -> Operand {
        match (&lhs, &rhs) {
            (_, Operand::Integer { value: 0, .. }) => lhs,
            (Operand::Integer { value: 0, .. }, _) => rhs,
            (Operand::Integer { value: a, .. }, Operand::Integer { value: b, .. }) => {
                Operand::u32((a + b) as u32)
            }
            _ => self.emit(
                Type::u32(),
                InstructionKind::IntegerBinary {
                    op: IntegerBinaryOp::Add,
                    lhs,
                    rhs,
                },
            ),
        }
    }

    fn multiply(&mut self, lhs: Operand, factor: u32) -> Operand {
        match lhs {
            _ if factor == 1 => lhs,
            Operand::Integer { value, .. } => Operand::u32((value as u32).wrapping_mul(factor)),
            lhs => self.emit(
                Type::u32(),
                InstructionKind::IntegerBinary {
                    op: IntegerBinaryOp::Multiply,
                    lhs,
                    rhs: Operand::u32(factor),
                },
            ),
        }
    }
}

/// The scalar memory of `ty` holds, if it holds one: a `bool` is a byte.
fn memory_scalar(ty: &Ty) -> Option<Scalar> {
    match ty {
        Ty::Scalar(scalar) => Some(*scalar),
        Ty::ByteBool => Some(Scalar::UCHAR),
        _ => None,
    }
}

fn scalar_size(scalar: Scalar) -> u32 {
    (u32::from(scalar.bits()) / 8).max(1)
}

/// The integer a scalar of `size` bytes is assembled in: at least 32 bits,
/// so that only conversions touch a narrower one.
fn wide_type(size: u32) -> Type {
    Type::Int {
        bits: if size > 4 { 64 } else { 32 },
        signed: false,
    }
}
