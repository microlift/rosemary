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
                Err(_) => module
                    .static_initializers
                    .push(Lowerer::global_initializer(self, global, index)?),
            }
        }
        Ok(())
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

fn constant_encoding(ty: &Ty) -> Option<String> {
    let (scalar, lanes) = match ty {
        Ty::Scalar(scalar) => (scalar, 1),
        Ty::Vector(scalar, lanes) => (scalar, *lanes),
        _ => return None,
    };
    let encoding = match *scalar {
        Scalar::Bool => "b",
        Scalar::Float { bits: 16 } => "Dh",
        Scalar::Float { bits: 32 } => "f",
        Scalar::Int {
            signed: true,
            bits: 8,
        } => "c",
        Scalar::Int {
            signed: false,
            bits: 8,
        } => "h",
        Scalar::Int {
            signed: true,
            bits: 16,
        } => "s",
        Scalar::Int {
            signed: false,
            bits: 16,
        } => "t",
        Scalar::Int {
            signed: true,
            bits: 32,
        } => "i",
        Scalar::Int {
            signed: false,
            bits: 32,
        } => "j",
        Scalar::Int {
            signed: true,
            bits: 64,
        } => "l",
        Scalar::Int {
            signed: false,
            bits: 64,
        } => "m",
        _ => return None,
    };
    match lanes {
        1 => Some(encoding.to_owned()),
        2..=4 => Some(format!("Dv{lanes}_{encoding}")),
        _ => None,
    }
}
