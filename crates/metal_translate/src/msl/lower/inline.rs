//! Calls to the library's own functions, which are inlined: the IR has no
//! call. Metal forbids recursion, so every call graph inlines to an end.

use super::expr::describe;
use super::*;

/// Where a `return` inside an inlined function goes.
pub(super) struct Frame {
    /// The function, by address: overloads share a name.
    function: *const FunctionDef,
    /// The result's local, for a function that returns a value.
    result: Option<(LValue, Ty)>,
    exit: BlockId,
    /// A function template's type parameters, as this call binds them.
    types: HashMap<String, Ty>,
}

/// `ty` with each template parameter in `types` replaced by its binding.
fn substitute(ty: &Ty, types: &HashMap<String, Ty>, line: usize) -> Result<Ty> {
    Ok(match ty {
        Ty::Param(name) => types.get(name).cloned().ok_or_else(|| {
            syntax_error(
                line,
                format!("template parameter {name} is not deduced from the call"),
            )
        })?,
        Ty::Array(element, count) => Ty::Array(Box::new(substitute(element, types, line)?), *count),
        ty => ty.clone(),
    })
}

/// Bind the template parameters `declared` names from the type an argument
/// has, as C++ deduces them.
fn deduce(declared: &Ty, actual: &Ty, types: &mut HashMap<String, Ty>, line: usize) -> Result<()> {
    match (declared, actual) {
        (Ty::Param(name), actual) => match types.get(name) {
            Some(bound) if bound != actual => Err(syntax_error(
                line,
                format!(
                    "template parameter {name} is deduced as both {} and {}",
                    describe(bound),
                    describe(actual)
                ),
            )),
            Some(_) => Ok(()),
            None => {
                types.insert(name.clone(), actual.unpacked());
                Ok(())
            }
        },
        (Ty::Array(declared, _), Ty::Array(actual, _)) => deduce(declared, actual, types, line),
        _ => Ok(()),
    }
}

impl<'p> Lowerer<'_, 'p> {
    /// The library function a call names, if it names one.
    pub(super) fn helper(
        &self,
        name: &str,
        args: usize,
        line: usize,
    ) -> Result<Option<&'p FunctionDef>> {
        let named: Vec<&'p FunctionDef> = self
            .context
            .program
            .functions
            .iter()
            .filter(|function| function.stage.is_none() && function.name == name)
            .collect();
        if named.is_empty() {
            // A helper the parser skipped fails whatever calls it.
            if let Some(failed) = self
                .context
                .program
                .failed
                .iter()
                .find(|failed| failed.name.as_deref() == Some(name))
            {
                return Err(syntax_error(
                    line,
                    format!("{name} could not be compiled: {}", failed.error),
                ));
            }
            return Ok(None);
        }
        // Overloads are told apart by how many arguments they take.
        let mut fitting = named
            .iter()
            .filter(|function| function.params.len() == args);
        match (fitting.next(), fitting.next()) {
            (Some(function), None) => Ok(Some(function)),
            (Some(_), Some(_)) => Err(syntax_error(
                line,
                format!("{name} has overloads taking {args} arguments, which are not told apart"),
            )),
            (None, _) => Err(syntax_error(
                line,
                format!("no {name} takes {args} arguments"),
            )),
        }
    }

    /// Inline one call of `function`.
    pub(super) fn inline_call(
        &mut self,
        function: &FunctionDef,
        args: &[Expr],
        line: usize,
    ) -> Result<Value> {
        if self
            .frames
            .iter()
            .any(|frame| std::ptr::eq(frame.function, function))
        {
            return Err(syntax_error(
                line,
                format!("{} calls itself, which Metal forbids", function.name),
            ));
        }
        // Arguments are evaluated in the caller's scope, before the callee's
        // replaces it.
        let mut scope = HashMap::new();
        let mut types = HashMap::new();
        for (param, arg) in function.params.iter().zip(args) {
            let binding = self.argument(param, arg, &mut types)?;
            scope.insert(param.name.clone(), binding);
        }
        let exit = self.new_block("return");
        let result = match substitute(&function.ret, &types, line)? {
            Ty::Void => None,
            ty => Some((self.local_lvalue(&ty), ty)),
        };
        self.frames.push(Frame {
            function,
            result: result.clone(),
            exit,
            types,
        });
        let scopes = std::mem::replace(&mut self.scopes, vec![scope]);
        let loops = std::mem::take(&mut self.loops);
        let lowered = self.statements(&function.body);
        self.scopes = scopes;
        self.loops = loops;
        self.frames.pop();
        lowered?;
        // Falling off the end is a `return;`.
        self.terminate(Terminator::Branch(exit));
        self.switch_to(exit);
        match result {
            Some((lvalue, _)) => self.load(&lvalue, line),
            None => Ok(Value {
                op: Operand::Undef(Type::Void),
                ty: Ty::Void,
            }),
        }
    }

    /// What a parameter names inside the callee.
    fn argument(
        &mut self,
        param: &Param,
        arg: &Expr,
        types: &mut HashMap<String, Ty>,
    ) -> Result<Binding> {
        let line = arg.line;
        match (param.indirection, &param.ty) {
            (Indirection::Value, Ty::Texture(_) | Ty::Sampler) => {
                Ok(Binding::Value(self.rvalue(arg)?))
            }
            (Indirection::Value, declared) => {
                let value = self.rvalue(arg)?;
                deduce(declared, &value.ty, types, line)?;
                let ty = &substitute(declared, types, line)?;
                let value = self.convert(value, ty, line)?;
                let lvalue = self.local_lvalue(ty);
                self.store(&lvalue, value, line)?;
                Ok(Binding::LValue(lvalue))
            }
            // A reference names the caller's own place.
            (Indirection::Reference, declared) => {
                let lvalue = self.lvalue(arg)?.ok_or_else(|| {
                    syntax_error(
                        line,
                        format!("{} is passed something not in memory", param.name),
                    )
                })?;
                deduce(declared, &lvalue.ty(), types, line)?;
                let ty = &substitute(declared, types, line)?;
                if &lvalue.ty() != ty {
                    return Err(syntax_error(
                        line,
                        format!(
                            "{} takes a reference to {}, not {}",
                            param.name,
                            describe(ty),
                            describe(&lvalue.ty())
                        ),
                    ));
                }
                Ok(Binding::LValue(lvalue))
            }
            (Indirection::Pointer, declared) => {
                let pointer = self.pointer(arg)?.ok_or_else(|| {
                    syntax_error(
                        line,
                        format!("{} is passed something not a buffer pointer", param.name),
                    )
                })?;
                if let Some(space) = param.space
                    && address_space(space) != pointer.space()
                {
                    return Err(syntax_error(
                        line,
                        format!(
                            "{} is passed a pointer into another address space",
                            param.name
                        ),
                    ));
                }
                let element = pointer.element().unpacked();
                deduce(declared, &element, types, line)?;
                let ty = substitute(declared, types, line)?;
                if ty.in_memory() != *pointer.element() {
                    return Err(syntax_error(
                        line,
                        format!(
                            "{} takes a pointer to {}, not {}",
                            param.name,
                            describe(&ty),
                            describe(&element)
                        ),
                    ));
                }
                Ok(Binding::Pointer(pointer))
            }
        }
    }

    /// `return` inside an inlined function: store the result and leave.
    /// `None` outside one, where the entry point's own return applies.
    pub(super) fn inline_return(
        &mut self,
        value: Option<&Expr>,
        line: usize,
    ) -> Result<Option<()>> {
        let Some(frame) = self.frames.last() else {
            return Ok(None);
        };
        let (result, exit) = (frame.result.clone(), frame.exit);
        match (result, value) {
            (None, None) => {}
            (Some((lvalue, ty)), Some(value)) => {
                let value = self.rvalue(value)?;
                let value = self.convert(value, &ty, line)?;
                self.store(&lvalue, value, line)?;
            }
            (None, Some(_)) => {
                return Err(syntax_error(line, "a value returned from a void function"));
            }
            (Some(_), None) => {
                return Err(syntax_error(
                    line,
                    "no value returned from a non-void function",
                ));
            }
        }
        self.terminate(Terminator::Branch(exit));
        self.after_jump();
        Ok(Some(()))
    }
}

impl Lowerer<'_, '_> {
    /// A type the source names, with the template parameters of the function
    /// being inlined bound.
    pub(super) fn resolve(&self, ty: &Ty, line: usize) -> Result<Ty> {
        match self.frames.last() {
            Some(frame) => substitute(ty, &frame.types, line),
            None => Ok(ty.clone()),
        }
    }
}
