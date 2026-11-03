//! Matrices: values are arrays of their column vectors, and every operator
//! is lowered to arithmetic on the columns.

use super::expr::{describe, is_constant};
use super::*;

impl Lowerer<'_, '_> {
    /// The column vectors of a matrix value.
    fn columns(&mut self, value: &Value) -> Vec<Value> {
        let Ty::Matrix { columns, .. } = value.ty else {
            unreachable!("only matrices have columns");
        };
        let column = value.ty.column().expect("a matrix");
        (0..u32::from(columns))
            .map(|index| {
                let op = self.emit(
                    self.context.ir(&column),
                    InstructionKind::CompositeExtract {
                        composite: value.op.clone(),
                        index,
                    },
                );
                Value {
                    op,
                    ty: column.clone(),
                }
            })
            .collect()
    }

    /// The matrix of type `ty` with these columns, already of its column type.
    fn with_columns(&mut self, ty: &Ty, columns: Vec<Operand>) -> Value {
        let ir = self.context.ir(ty);
        if columns.iter().all(is_constant) {
            return Value {
                op: Operand::Composite {
                    ty: ir,
                    elements: columns,
                },
                ty: ty.clone(),
            };
        }
        let mut op = Operand::Undef(ir.clone());
        for (index, column) in columns.into_iter().enumerate() {
            op = self.emit(
                ir.clone(),
                InstructionKind::CompositeInsert {
                    composite: op,
                    object: column,
                    index: index as u32,
                },
            );
        }
        Value { op, ty: ty.clone() }
    }

    /// Apply `op` to each column of `value`, giving a matrix of `ty`.
    fn per_column(
        &mut self,
        ty: &Ty,
        value: &Value,
        mut op: impl FnMut(&mut Self, Value) -> Result<Value>,
    ) -> Result<Value> {
        let columns = self
            .columns(value)
            .into_iter()
            .map(|column| op(self, column).map(|column| column.op))
            .collect::<Result<Vec<_>>>()?;
        Ok(self.with_columns(ty, columns))
    }

    /// `floatCxR(...)`: from columns, from `C * R` scalars in column order,
    /// from one scalar on the diagonal, or from a matrix of the same shape.
    pub(super) fn construct_matrix(
        &mut self,
        ty: &Ty,
        args: &[Expr],
        line: usize,
    ) -> Result<Value> {
        let Ty::Matrix {
            scalar,
            columns,
            rows,
        } = *ty
        else {
            unreachable!("a matrix type");
        };
        let column = ty.column().expect("a matrix");
        let values = args
            .iter()
            .map(|arg| self.rvalue(arg))
            .collect::<Result<Vec<_>>>()?;
        match values.as_slice() {
            [] => {
                let zero = self.constant_of(scalar, rows, Number::Int(0));
                return Ok(self.with_columns(ty, vec![zero; usize::from(columns)]));
            }
            [value] if matches!(value.ty, Ty::Matrix { .. }) => {
                return self.convert(value.clone(), ty, line);
            }
            [value] if value.ty.lanes() == Some(1) => {
                let diagonal = self.convert(value.clone(), &Ty::Scalar(scalar), line)?;
                let zero = number_operand(scalar, Number::Int(0));
                let columns = (0..columns)
                    .map(|index| {
                        let elements = (0..rows)
                            .map(|row| match row == index {
                                true => diagonal.op.clone(),
                                false => zero.clone(),
                            })
                            .collect::<Vec<_>>();
                        self.vector_of(scalar, elements)
                    })
                    .collect();
                return Ok(self.with_columns(ty, columns));
            }
            _ => {}
        }
        if values.iter().all(|value| value.ty.lanes() == Some(rows))
            && values.len() == usize::from(columns)
        {
            let columns = values
                .into_iter()
                .map(|value| self.convert(value, &column, line).map(|value| value.op))
                .collect::<Result<Vec<_>>>()?;
            return Ok(self.with_columns(ty, columns));
        }
        if values.len() == usize::from(columns) * usize::from(rows)
            && values.iter().all(|value| value.ty.lanes() == Some(1))
        {
            let scalars = values
                .into_iter()
                .map(|value| {
                    self.convert(value, &Ty::Scalar(scalar), line)
                        .map(|value| value.op)
                })
                .collect::<Result<Vec<_>>>()?;
            let columns = scalars
                .chunks(usize::from(rows))
                .map(|elements| self.vector_of(scalar, elements.to_vec()))
                .collect();
            return Ok(self.with_columns(ty, columns));
        }
        Err(syntax_error(
            line,
            format!(
                "a {} is constructed from {columns} columns or {} scalars",
                describe(ty),
                usize::from(columns) * usize::from(rows)
            ),
        ))
    }

    /// The vector of `scalar` with these lanes.
    pub(super) fn vector_of(&mut self, scalar: Scalar, elements: Vec<Operand>) -> Operand {
        let ty = Ty::Vector(scalar, elements.len() as u8);
        let ir = self.context.ir(&ty);
        if elements.iter().all(is_constant) {
            return Operand::Composite { ty: ir, elements };
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
        op
    }

    /// A matrix converted to another of the same shape, column by column.
    pub(super) fn convert_matrix(&mut self, value: Value, to: &Ty, line: usize) -> Result<Value> {
        match (&value.ty, to) {
            (
                Ty::Matrix { columns, rows, .. },
                Ty::Matrix {
                    columns: to_columns,
                    rows: to_rows,
                    ..
                },
            ) if columns == to_columns && rows == to_rows => {
                let column = to.column().expect("a matrix");
                self.per_column(to, &value, |this, value| this.convert(value, &column, line))
            }
            (from, to) => Err(syntax_error(
                line,
                format!("no conversion from {} to {}", describe(from), describe(to)),
            )),
        }
    }

    /// `-m`.
    pub(super) fn negate_matrix(&mut self, value: Value, line: usize) -> Result<Value> {
        let ty = value.ty.clone();
        let Some(scalar) = matrix_scalar(&ty) else {
            unreachable!("a matrix");
        };
        let zero = Value {
            op: number_operand(scalar, Number::Int(0)),
            ty: Ty::Scalar(scalar),
        };
        self.per_column(&ty, &value, |this, column| {
            this.arithmetic("-", zero.clone(), column, line)
        })
    }

    /// A binary operator with a matrix on either side.
    pub(super) fn matrix_arithmetic(
        &mut self,
        op: &str,
        a: Value,
        b: Value,
        line: usize,
    ) -> Result<Value> {
        match (a.ty.clone(), b.ty.clone(), op) {
            // Matrix times column vector: the columns weighted by its lanes.
            (Ty::Matrix { columns, rows, .. }, Ty::Vector(_, lanes), "*") if lanes == columns => {
                let scalar = matrix_scalar(&a.ty).expect("a matrix");
                let vector = self.convert(b, &Ty::Vector(scalar, columns), line)?;
                let result_ty = Ty::Vector(scalar, rows);
                let mut sum: Option<Value> = None;
                for (index, column) in self.columns(&a).into_iter().enumerate() {
                    let weight = self.swizzle(vector.clone(), &[index as u32], line)?;
                    let term = self.arithmetic("*", column, weight, line)?;
                    sum = Some(match sum {
                        Some(sum) => self.arithmetic("+", sum, term, line)?,
                        None => term,
                    });
                }
                let sum = sum.expect("a matrix has columns");
                debug_assert_eq!(sum.ty, result_ty);
                Ok(sum)
            }
            // Row vector times matrix: one dot product per column.
            (Ty::Vector(_, lanes), Ty::Matrix { columns, rows, .. }, "*") if lanes == rows => {
                let scalar = matrix_scalar(&b.ty).expect("a matrix");
                let vector = self.convert(a, &Ty::Vector(scalar, rows), line)?;
                let mut lanes = Vec::new();
                for column in self.columns(&b) {
                    lanes.push(self.call_values("dot", &[vector.clone(), column], line)?.op);
                }
                let op = self.vector_of(scalar, lanes);
                Ok(Value {
                    op,
                    ty: Ty::Vector(scalar, columns),
                })
            }
            // Matrix product: each column of the right transformed by the left.
            (
                Ty::Matrix {
                    columns: inner,
                    rows,
                    scalar,
                },
                Ty::Matrix {
                    columns,
                    rows: inner_rows,
                    ..
                },
                "*",
            ) if inner == inner_rows => {
                let ty = Ty::Matrix {
                    scalar,
                    columns,
                    rows,
                };
                self.per_column(&ty, &b, |this, column| {
                    this.matrix_arithmetic("*", a.clone(), column, line)
                })
            }
            // Elementwise sum and difference of matrices of one shape.
            (Ty::Matrix { .. }, Ty::Matrix { .. }, "+" | "-") => {
                let ty = a.ty.clone();
                let b = self.convert(b, &ty, line)?;
                let right = self.columns(&b);
                let mut index = 0;
                self.per_column(&ty, &a, |this, column| {
                    index += 1;
                    this.arithmetic(op, column, right[index - 1].clone(), line)
                })
            }
            // A scalar scales every element.
            (Ty::Matrix { .. }, Ty::Scalar(_), "*" | "/" | "+" | "-") => {
                let ty = a.ty.clone();
                self.per_column(&ty, &a, |this, column| {
                    this.arithmetic(op, column, b.clone(), line)
                })
            }
            (Ty::Scalar(_), Ty::Matrix { .. }, "*" | "+" | "-") => {
                let ty = b.ty.clone();
                self.per_column(&ty, &b, |this, column| {
                    this.arithmetic(op, a.clone(), column, line)
                })
            }
            _ => Err(syntax_error(
                line,
                format!(
                    "no operator {op} on {} and {}",
                    describe(&a.ty),
                    describe(&b.ty)
                ),
            )),
        }
    }

    /// `transpose(m)`.
    pub(super) fn transpose(&mut self, value: Value, line: usize) -> Result<Value> {
        let Ty::Matrix {
            scalar,
            columns,
            rows,
        } = value.ty
        else {
            return Err(syntax_error(line, "transpose of something not a matrix"));
        };
        let source = self.columns(&value);
        let mut result = Vec::new();
        for row in 0..rows {
            let mut elements = Vec::new();
            for column in &source {
                elements.push(self.swizzle(column.clone(), &[u32::from(row)], line)?.op);
            }
            result.push(self.vector_of(scalar, elements));
        }
        let ty = Ty::Matrix {
            scalar,
            columns: rows,
            rows: columns,
        };
        Ok(self.with_columns(&ty, result))
    }
}

fn matrix_scalar(ty: &Ty) -> Option<Scalar> {
    match ty {
        Ty::Matrix { scalar, .. } => Some(*scalar),
        _ => None,
    }
}
