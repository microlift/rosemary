//! Structures holding textures, samplers or buffer pointers.
//!
//! Metal lets a structure carry these by value, which shader generators such
//! as Skia's use to bundle a texture with its sampler and to keep pointers to
//! the stage's buffers. SPIR-V can store none of them in a variable, so such
//! a structure is never memory here: it is its fields, the ordinary ones each
//! a local and every texture, sampler or pointer one the resource it was
//! initialized with. That binding is fixed where the structure is declared,
//! which is what lets it be resolved while lowering; assigning one later is
//! refused.

use super::*;

impl Context<'_> {
    /// Whether a value of `ty` holds a texture, sampler or pointer.
    pub(super) fn has_handles(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Texture(_) | Ty::Sampler | Ty::Pointer(..) => true,
            Ty::Array(element, _) | Ty::ArrayParam(element, _) => self.has_handles(element),
            Ty::Struct(index) => self.program.structs[*index]
                .fields
                .iter()
                .any(|field| self.has_handles(&field.ty)),
            _ => false,
        }
    }
}

/// Why a structure holding a texture, sampler or pointer cannot be in memory.
pub(super) const IN_MEMORY: &str = "a structure with a texture, sampler or pointer member in memory is an argument buffer, which is not supported";

impl Lowerer<'_, '_> {
    /// A local of the handle-holding structure `index`, initialized from
    /// `init`: a list of its fields, or another such structure to copy.
    pub(super) fn handle_struct(&mut self, index: usize, init: &Expr) -> Result<LValue> {
        let line = init.line;
        let fields = self.context.program.structs[index].fields.clone();
        let ExprKind::InitList(items) = &init.kind else {
            let source = self.handle_fields(init).ok_or_else(|| {
                syntax_error(
                    line,
                    "a structure with a texture, sampler or pointer member must be initialized from a list or another one",
                )
            })?;
            if source.ty() != Ty::Struct(index) {
                return Err(syntax_error(
                    line,
                    format!(
                        "a structure initialized from {}",
                        expr::describe(&source.ty())
                    ),
                ));
            }
            return self.copy_handles(&source, line);
        };
        if items.len() != fields.len() {
            return Err(syntax_error(
                line,
                "a structure with a texture, sampler or pointer member must initialize every member",
            ));
        }
        let mut lvalues = Vec::new();
        for (field, item) in fields.iter().zip(items) {
            lvalues.push(self.handle_field(&field.ty, item)?);
        }
        Ok(LValue::Fields(index, lvalues))
    }

    /// One member of a handle-holding structure, initialized from `item`.
    fn handle_field(&mut self, ty: &Ty, item: &Expr) -> Result<LValue> {
        let line = item.line;
        match ty {
            Ty::Struct(index) if self.context.has_handles(ty) => self.handle_struct(*index, item),
            Ty::Texture(_) | Ty::Sampler => {
                let value = self.rvalue(item)?;
                if value.ty != *ty {
                    return Err(syntax_error(
                        line,
                        format!(
                            "a {} member initialized with {}",
                            expr::describe(ty),
                            expr::describe(&value.ty)
                        ),
                    ));
                }
                Ok(LValue::Handle(Box::new(Binding::Value(value)), ty.clone()))
            }
            Ty::Pointer(space, pointee) => {
                let pointer = self.pointer(item)?.ok_or_else(|| {
                    syntax_error(
                        line,
                        "a pointer member initialized with something not a pointer",
                    )
                })?;
                if pointer.space() != address_space(*space) || *pointer.element() != **pointee {
                    return Err(syntax_error(
                        line,
                        format!(
                            "a {} member initialized with a pointer to {}",
                            expr::describe(ty),
                            expr::describe(pointer.element())
                        ),
                    ));
                }
                Ok(LValue::Handle(
                    Box::new(Binding::Pointer(pointer)),
                    ty.clone(),
                ))
            }
            _ if self.context.has_handles(ty) => Err(syntax_error(
                line,
                "an array of textures, samplers or pointers in a structure is not supported",
            )),
            _ => self.initialized_local(ty, Some(item), line),
        }
    }

    /// A copy of a handle-holding structure: its ordinary members copied
    /// into new locals, its texture, sampler and pointer members shared.
    pub(super) fn copy_handles(&mut self, source: &LValue, line: usize) -> Result<LValue> {
        match source {
            LValue::Handle(..) => Ok(source.clone()),
            LValue::Fields(index, fields) if self.context.has_handles(&Ty::Struct(*index)) => {
                let mut copied = Vec::new();
                for field in fields {
                    copied.push(self.copy_handles(field, line)?);
                }
                Ok(LValue::Fields(*index, copied))
            }
            field => {
                let value = self.load(field, line)?;
                let local = self.local_lvalue(&value.ty);
                self.store(&local, value, line)?;
                Ok(local)
            }
        }
    }

    /// The handle-holding structure `expr` names, found without emitting
    /// anything: a local or parameter, or a member of one.
    pub(super) fn handle_fields(&self, expr: &Expr) -> Option<LValue> {
        let lvalue = self.handle_path(expr)?;
        match &lvalue {
            LValue::Fields(index, _) if self.context.has_handles(&Ty::Struct(*index)) => {
                Some(lvalue)
            }
            _ => None,
        }
    }

    /// The texture, sampler or pointer member `expr` names, found as
    /// [`Self::handle_fields`] finds a structure.
    pub(super) fn handle_member(&self, expr: &Expr) -> Option<Binding> {
        match self.handle_path(expr)? {
            LValue::Handle(binding, _) => Some(*binding),
            _ => None,
        }
    }

    fn handle_path(&self, expr: &Expr) -> Option<LValue> {
        match &expr.kind {
            ExprKind::Ident(name) => match self.lookup(name)? {
                Binding::LValue(lvalue @ LValue::Fields(..)) => Some(lvalue),
                _ => None,
            },
            ExprKind::Member(object, member) => match self.handle_path(object)? {
                LValue::Fields(index, fields) => {
                    let position = self.field_position(index, member, expr.line).ok()?;
                    fields.into_iter().nth(position)
                }
                _ => None,
            },
            _ => None,
        }
    }
}
