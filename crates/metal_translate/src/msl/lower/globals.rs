//! Program-scope constants and the initializers specialization evaluates.

use super::*;

impl Context<'_> {
    pub(super) fn lower_globals(&mut self, module: &mut Module) -> Result<()> {
        let mut indices = std::collections::HashSet::new();
        for global in &self.program.globals {
            if global.function_constant.is_some() && global.init.is_some() {
                return Err(syntax_error(
                    global.line,
                    "a function constant cannot have a source initializer",
                ));
            }
            let function_constant = global
                .function_constant
                .as_ref()
                .map(|expression| {
                    let index = self.const_index(expression)?;
                    if index > u32::from(u16::MAX) {
                        return Err(syntax_error(
                            expression.line,
                            "a function constant index must fit in 16 bits",
                        ));
                    }
                    if !indices.insert(index) {
                        return Err(syntax_error(
                            expression.line,
                            "duplicate function constant indices are not supported",
                        ));
                    }
                    Ok(ir::FunctionConstantSlot {
                        index,
                        name: global.name.clone(),
                        encoding: constant_encoding(&global.ty).ok_or_else(|| {
                            syntax_error(
                                global.line,
                                "a function constant must have a supported scalar or vector type",
                            )
                        })?,
                    })
                })
                .transpose()?;
            let index = module.globals.len() as u32;
            module.globals.push(GlobalVariable {
                name: global.name.clone(),
                space: AddressSpace::Constant,
                ty: self.ir(&global.ty),
                initializer: None,
                function_constant,
            });
            self.globals
                .insert(global.name.clone(), (index, global.ty.clone()));
            let Some(expression) = &global.init else {
                continue;
            };
            match self.constant(expression, &global.ty, global.line) {
                Ok(value) => module.globals[index as usize].initializer = Some(value),
                // Only a value read from another program-scope constant can
                // wait for specialization; anything else is malformed here.
                Err(_) if self.reads_global(expression) => module
                    .static_initializers
                    .push(Lowerer::global_initializer(self, global, index)?),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn reads_global(&self, expr: &Expr) -> bool {
        let any = |exprs: &[Expr]| exprs.iter().any(|expr| self.reads_global(expr));
        match &expr.kind {
            ExprKind::Ident(name) => self.globals.contains_key(name),
            ExprKind::Int { .. } | ExprKind::Float { .. } | ExprKind::Bool(_) => false,
            ExprKind::Unary(_, inner)
            | ExprKind::Postfix(_, inner)
            | ExprKind::Cast(_, inner)
            | ExprKind::AtomicCast(_, inner)
            | ExprKind::PointerCast(_, _, inner)
            | ExprKind::Member(inner, _) => self.reads_global(inner),
            ExprKind::Binary(_, lhs, rhs)
            | ExprKind::Assign(_, lhs, rhs)
            | ExprKind::Index(lhs, rhs) => self.reads_global(lhs) || self.reads_global(rhs),
            ExprKind::Ternary(condition, yes, no) => {
                self.reads_global(condition) || self.reads_global(yes) || self.reads_global(no)
            }
            ExprKind::Call { args, .. }
            | ExprKind::Construct(_, args)
            | ExprKind::InitList(args) => any(args),
            ExprKind::CompoundLiteral(_, fields) => {
                fields.iter().any(|(_, expr)| self.reads_global(expr))
            }
            ExprKind::Method { object, args, .. } => self.reads_global(object) || any(args),
        }
    }
}

impl Lowerer<'_, '_> {
    fn global_initializer(context: &Context<'_>, global: &Global, index: u32) -> Result<Function> {
        let mut lowerer = Lowerer::new(context, Stage::Kernel);
        let expression = global
            .init
            .as_ref()
            .expect("a derived constant has an initializer");
        let value = lowerer.rvalue(expression)?;
        let value = lowerer.convert(value, &global.ty, global.line)?;
        if !lowerer.globals.is_empty() {
            return Err(syntax_error(
                global.line,
                "threadgroup memory in a global initializer",
            ));
        }
        lowerer.emit_void(InstructionKind::Store {
            pointer: Operand::Global {
                index,
                ty: Type::Pointer {
                    space: AddressSpace::Constant,
                    pointee: Box::new(context.ir(&global.ty)),
                },
            },
            value: value.op,
        });
        lowerer.terminate(Terminator::Return(None));
        Ok(lowerer.finish(&format!("initializer for {}", global.name), Vec::new()))
    }

    pub(super) fn function_constant_defined(
        &mut self,
        args: &[Expr],
        line: usize,
    ) -> Result<Value> {
        let [
            Expr {
                kind: ExprKind::Ident(name),
                ..
            },
        ] = args
        else {
            return Err(syntax_error(
                line,
                "is_function_constant_defined needs a function constant name",
            ));
        };
        if !self
            .context
            .program
            .globals
            .iter()
            .any(|global| &global.name == name && global.function_constant.is_some())
        {
            return Err(syntax_error(
                line,
                format!("{name} is not a function constant"),
            ));
        }
        let Some((index, ty)) = self.context.globals.get(name) else {
            return Err(syntax_error(
                line,
                format!("function constant {name} is not declared yet"),
            ));
        };
        let constant = Operand::Global {
            index: *index,
            ty: Type::Pointer {
                space: AddressSpace::Constant,
                pointee: Box::new(self.context.ir(ty)),
            },
        };
        let op = self.emit(
            Type::Bool,
            InstructionKind::FunctionConstantDefined { constant },
        );
        Ok(Value {
            op,
            ty: Ty::Scalar(Scalar::Bool),
        })
    }
}

/// The Itanium spelling of a function constant's type, as AIR names it.
fn constant_encoding(ty: &Ty) -> Option<String> {
    use crate::specialize::{
        MTL_DATA_TYPE_BOOL, MTL_DATA_TYPE_CHAR, MTL_DATA_TYPE_FLOAT, MTL_DATA_TYPE_HALF,
        MTL_DATA_TYPE_INT, MTL_DATA_TYPE_LONG, MTL_DATA_TYPE_SHORT, MTL_DATA_TYPE_UCHAR,
        MTL_DATA_TYPE_UINT, MTL_DATA_TYPE_ULONG, MTL_DATA_TYPE_USHORT, metal_data_type_encoding,
    };
    let (scalar, lanes) = match ty {
        Ty::Scalar(scalar) => (scalar, 1),
        Ty::Vector(scalar, lanes) => (scalar, *lanes),
        _ => return None,
    };
    let scalar = match *scalar {
        Scalar::Bool => MTL_DATA_TYPE_BOOL,
        Scalar::Float { bits: 16 } => MTL_DATA_TYPE_HALF,
        Scalar::Float { bits: 32 } => MTL_DATA_TYPE_FLOAT,
        Scalar::Int { signed, bits } => match (signed, bits) {
            (true, 8) => MTL_DATA_TYPE_CHAR,
            (false, 8) => MTL_DATA_TYPE_UCHAR,
            (true, 16) => MTL_DATA_TYPE_SHORT,
            (false, 16) => MTL_DATA_TYPE_USHORT,
            (true, 32) => MTL_DATA_TYPE_INT,
            (false, 32) => MTL_DATA_TYPE_UINT,
            (true, 64) => MTL_DATA_TYPE_LONG,
            (false, 64) => MTL_DATA_TYPE_ULONG,
            _ => return None,
        },
        _ => return None,
    };
    // An `MTLDataType`'s vectors of two to four lanes follow its scalar.
    (1..=4)
        .contains(&lanes)
        .then(|| metal_data_type_encoding(scalar + u64::from(lanes) - 1))
        .flatten()
}
