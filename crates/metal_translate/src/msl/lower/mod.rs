//! Lower a parsed Metal source library to the Metal IR.
//!
//! Locals become `thread` variables read and written through loads and
//! stores, as an unoptimized compiler leaves them; the driver's own compiler
//! promotes them. A structure-typed local is split into one variable per
//! field, because a structure type carries Metal's buffer layout and a
//! Vulkan function variable may not.

mod expr;
mod library;

use std::collections::HashMap;

use super::ast::*;
use super::syntax_error;
use crate::TranslateError;
use crate::ir::{
    self, Access, AddressSpace, Block, BlockId, Builtin, EntryPoint, Function, GlobalVariable,
    Instruction, InstructionKind, Interpolation, Module, Operand, Parameter, Resource,
    ResourceKind, Sampling, Stage, StageBinding, StageInput, StageOutput, StructField, StructType,
    Terminator, Type, ValueId,
};

type Result<T> = std::result::Result<T, TranslateError>;

pub(super) fn lower(program: &Program) -> Result<Module> {
    let mut module = Module {
        functions: Vec::new(),
        entries: Vec::new(),
        globals: Vec::new(),
        static_initializers: Vec::new(),
    };
    let context = Context::new(program, &mut module)?;
    for function in &program.functions {
        let Some(stage) = function.stage else {
            // Nothing calls a function: the IR has no call, and an inlined
            // helper is the only way one could be honoured.
            return Err(syntax_error(
                function.line,
                format!(
                    "function {} is not an entry point; helper functions are not supported",
                    function.name
                ),
            ));
        };
        let (lowered, entry) = Lowerer::entry(
            &context,
            function,
            stage,
            module.functions.len(),
            &mut module.globals,
        )?;
        module.functions.push(lowered);
        module.entries.push(entry);
    }
    Ok(module)
}

/// What every function of the library shares: structure layouts and the
/// file-scope constants.
pub(super) struct Context<'p> {
    program: &'p Program,
    layouts: Vec<StructType>,
    globals: HashMap<String, (u32, Ty)>,
}

impl<'p> Context<'p> {
    fn new(program: &'p Program, module: &mut Module) -> Result<Self> {
        let mut context = Context {
            program,
            layouts: Vec::new(),
            globals: HashMap::new(),
        };
        for index in 0..program.structs.len() {
            let layout = context.layout_struct(index)?;
            context.layouts.push(layout);
        }
        for global in &program.globals {
            let initializer = context.constant(&global.init, &global.ty, global.line)?;
            let index = module.globals.len() as u32;
            module.globals.push(GlobalVariable {
                name: global.name.clone(),
                space: AddressSpace::Constant,
                ty: context.ir(&global.ty),
                initializer: Some(initializer),
                function_constant: None,
            });
            context
                .globals
                .insert(global.name.clone(), (index, global.ty.clone()));
        }
        Ok(context)
    }

    /// The IR type of a source type. Integers are signless.
    pub(super) fn ir(&self, ty: &Ty) -> Type {
        match ty {
            Ty::Void => Type::Void,
            Ty::Scalar(scalar) => scalar_ir(*scalar),
            Ty::Vector(scalar, lanes) => Type::Vector {
                element: Box::new(scalar_ir(*scalar)),
                lanes: *lanes,
            },
            Ty::Array(element, count) => Type::Array {
                element: Box::new(self.ir(element)),
                count: *count,
            },
            Ty::Struct(index) => Type::Struct(self.layouts[*index].clone()),
            Ty::Texture(texture) => Type::Texture(texture.clone()),
            Ty::Sampler => Type::Sampler,
        }
    }

    /// Metal's size and alignment of a type in memory.
    fn size_align(&self, ty: &Ty, line: usize) -> Result<(u32, u32)> {
        Ok(match ty {
            Ty::Scalar(scalar) => {
                let size = (u32::from(scalar.bits()) / 8).max(1);
                (size, size)
            }
            Ty::Vector(scalar, lanes) => {
                let element = (u32::from(scalar.bits()) / 8).max(1);
                // A three-lane vector occupies four lanes.
                let size = element * u32::from(*lanes).next_power_of_two();
                (size, size)
            }
            Ty::Array(element, count) => {
                let (size, align) = self.size_align(element, line)?;
                (size.next_multiple_of(align) * count, align)
            }
            Ty::Struct(index) => {
                let layout = self
                    .layouts
                    .get(*index)
                    .ok_or_else(|| syntax_error(line, "a structure contains itself"))?;
                (layout.size, layout.alignment)
            }
            _ => return Err(syntax_error(line, "a resource type has no size in memory")),
        })
    }

    fn layout_struct(&self, index: usize) -> Result<StructType> {
        let definition = &self.program.structs[index];
        let mut offset = 0u32;
        let mut alignment = 1u32;
        let mut fields = Vec::new();
        for field in &definition.fields {
            let ty = match &field.ty {
                // A resource member is an argument buffer, which nothing
                // here implements; a stage struct's fields are never laid out
                // in memory, so give them a nominal size.
                Ty::Texture(_) | Ty::Sampler => {
                    return Err(syntax_error(
                        field.line,
                        "a structure with a texture or sampler member is an argument buffer, which is not supported",
                    ));
                }
                ty => ty,
            };
            let (size, align) = self.size_align(ty, field.line)?;
            offset = offset.next_multiple_of(align);
            fields.push(StructField {
                name: Some(field.name.clone()),
                ty: self.ir(ty),
                offset,
            });
            offset += size;
            alignment = alignment.max(align);
        }
        Ok(StructType {
            fields,
            size: offset.next_multiple_of(alignment).max(alignment),
            alignment,
            packed: false,
        })
    }

    /// A constant initializer: literals, constructors of literals, and lists
    /// of either.
    fn constant(&self, expr: &Expr, ty: &Ty, line: usize) -> Result<Operand> {
        match (ty, &expr.kind) {
            (Ty::Array(element, count), ExprKind::InitList(items)) => {
                if items.len() != *count as usize {
                    return Err(syntax_error(
                        line,
                        format!("an array of {count} initialized with {} items", items.len()),
                    ));
                }
                Ok(Operand::Composite {
                    ty: self.ir(ty),
                    elements: items
                        .iter()
                        .map(|item| self.constant(item, element, line))
                        .collect::<Result<_>>()?,
                })
            }
            (Ty::Struct(index), ExprKind::InitList(items)) => {
                let fields = &self.program.structs[*index].fields;
                if items.len() != fields.len() {
                    return Err(syntax_error(
                        line,
                        "a structure initializer of the wrong length",
                    ));
                }
                Ok(Operand::Composite {
                    ty: self.ir(ty),
                    elements: items
                        .iter()
                        .zip(fields)
                        .map(|(item, field)| self.constant(item, &field.ty, line))
                        .collect::<Result<_>>()?,
                })
            }
            (Ty::Vector(scalar, lanes), ExprKind::Construct(_, args)) => {
                let mut scalars = Vec::new();
                for arg in args {
                    match &arg.kind {
                        ExprKind::Construct(Ty::Vector(_, inner), inner_args) => {
                            if inner_args.len() != usize::from(*inner) {
                                return Err(syntax_error(
                                    line,
                                    "a constant vector must spell every lane",
                                ));
                            }
                            scalars.extend(inner_args.iter());
                        }
                        _ => scalars.push(arg),
                    }
                }
                let elements = match scalars.len() {
                    1 => vec![
                        self.constant(scalars[0], &Ty::Scalar(*scalar), line)?;
                        usize::from(*lanes)
                    ],
                    n if n == usize::from(*lanes) => scalars
                        .iter()
                        .map(|arg| self.constant(arg, &Ty::Scalar(*scalar), line))
                        .collect::<Result<_>>()?,
                    _ => {
                        return Err(syntax_error(
                            line,
                            "a constant vector with the wrong lane count",
                        ));
                    }
                };
                Ok(Operand::Composite {
                    ty: self.ir(ty),
                    elements,
                })
            }
            (Ty::Scalar(scalar), _) => {
                let value = self
                    .const_number(expr)
                    .ok_or_else(|| syntax_error(line, "a file-scope constant must be a literal"))?;
                Ok(number_operand(*scalar, value))
            }
            _ => Err(syntax_error(line, "unsupported constant initializer")),
        }
    }

    /// The value of a constant scalar expression.
    pub(super) fn const_number(&self, expr: &Expr) -> Option<Number> {
        Some(match &expr.kind {
            ExprKind::Int { value, .. } => Number::Int(*value as i64),
            ExprKind::Float { value, .. } => Number::Float(*value),
            ExprKind::Bool(value) => Number::Int(i64::from(*value)),
            ExprKind::Unary("-", inner) => match self.const_number(inner)? {
                Number::Int(value) => Number::Int(-value),
                Number::Float(value) => Number::Float(-value),
            },
            ExprKind::Unary("+", inner) => self.const_number(inner)?,
            ExprKind::Construct(Ty::Scalar(scalar), args) if args.len() == 1 => {
                let value = self.const_number(&args[0])?;
                match (scalar, value) {
                    (Scalar::Float { .. }, Number::Int(value)) => Number::Float(value as f64),
                    (Scalar::Int { .. } | Scalar::Bool, Number::Float(value)) => {
                        Number::Int(value as i64)
                    }
                    (_, value) => value,
                }
            }
            ExprKind::Index(array, index) => {
                let ExprKind::Ident(name) = &array.kind else {
                    return None;
                };
                let global = self
                    .program
                    .globals
                    .iter()
                    .find(|global| &global.name == name)?;
                let Number::Int(index) = self.const_number(index)? else {
                    return None;
                };
                let ExprKind::InitList(items) = &global.init.kind else {
                    return None;
                };
                self.const_number(items.get(usize::try_from(index).ok()?)?)?
            }
            ExprKind::Binary(op, lhs, rhs) => {
                let (Number::Int(lhs), Number::Int(rhs)) =
                    (self.const_number(lhs)?, self.const_number(rhs)?)
                else {
                    return None;
                };
                Number::Int(match *op {
                    "+" => lhs.checked_add(rhs)?,
                    "-" => lhs.checked_sub(rhs)?,
                    "*" => lhs.checked_mul(rhs)?,
                    "/" => lhs.checked_div(rhs)?,
                    "%" => lhs.checked_rem(rhs)?,
                    "<<" => lhs.checked_shl(u32::try_from(rhs).ok()?)?,
                    ">>" => lhs.checked_shr(u32::try_from(rhs).ok()?)?,
                    "&" => lhs & rhs,
                    "|" => lhs | rhs,
                    _ => return None,
                })
            }
            _ => return None,
        })
    }

    /// A constant integer, as an attribute's argument must be.
    fn const_index(&self, expr: &Expr) -> Result<u32> {
        match self.const_number(expr) {
            Some(Number::Int(value)) => {
                u32::try_from(value).map_err(|_| syntax_error(expr.line, "an index out of range"))
            }
            _ => Err(syntax_error(
                expr.line,
                "an attribute argument must be a constant integer",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Number {
    Int(i64),
    Float(f64),
}

pub(super) fn scalar_ir(scalar: Scalar) -> Type {
    match scalar {
        Scalar::Bool => Type::Bool,
        Scalar::Int { bits, .. } => Type::Int {
            bits,
            signed: false,
        },
        Scalar::Float { bits } => Type::Float { bits },
    }
}

/// A scalar constant of `scalar`'s type.
pub(super) fn number_operand(scalar: Scalar, value: Number) -> Operand {
    match (scalar, value) {
        (Scalar::Bool, Number::Int(value)) => Operand::Bool(value != 0),
        (Scalar::Bool, Number::Float(value)) => Operand::Bool(value != 0.0),
        (Scalar::Int { bits, .. }, value) => {
            let value = match value {
                Number::Int(value) => value,
                Number::Float(value) => value as i64,
            };
            let mask = if bits >= 64 {
                u64::MAX
            } else {
                (1u64 << bits) - 1
            };
            Operand::Integer {
                ty: scalar_ir(scalar),
                value: (value as u64) & mask,
            }
        }
        (Scalar::Float { bits }, value) => {
            let value = match value {
                Number::Int(value) => value as f64,
                Number::Float(value) => value,
            };
            Operand::Float {
                ty: Type::Float { bits },
                bits: match bits {
                    16 => u64::from(half::f16::from_f64(value).to_bits()),
                    64 => value.to_bits(),
                    _ => u64::from((value as f32).to_bits()),
                },
            }
        }
    }
}

/// An rvalue: an IR operand and its source type.
#[derive(Clone, Debug)]
pub(super) struct Value {
    pub op: Operand,
    pub ty: Ty,
}

/// Addressable memory: a root pointer and the indices from it, materialized
/// as one `BufferAddress` when it is read or written.
#[derive(Clone, Debug)]
pub(super) struct Place {
    pub root: Operand,
    pub space: AddressSpace,
    /// The first index steps the root pointer itself, as LLVM's does.
    pub indices: Vec<Operand>,
    pub ty: Ty,
}

/// Something an assignment can write.
#[derive(Clone, Debug)]
pub(super) enum LValue {
    Memory(Place),
    /// Some lanes of a vector, by constant index.
    Lanes(Box<LValue>, Vec<u32>, Ty),
    /// One lane of a vector, by an index known only when the shader runs.
    Dynamic(Box<LValue>, Operand, Ty),
    /// A structure local, split into one place per field.
    Fields(usize, Vec<LValue>),
}

impl LValue {
    pub fn ty(&self) -> Ty {
        match self {
            LValue::Memory(place) => place.ty.clone(),
            LValue::Lanes(_, _, ty) | LValue::Dynamic(_, _, ty) => ty.clone(),
            LValue::Fields(index, _) => Ty::Struct(*index),
        }
    }
}

/// What a name in scope stands for.
#[derive(Clone, Debug)]
pub(super) enum Binding {
    LValue(LValue),
    /// A read-only value: a texture, a sampler or a built-in input.
    Value(Value),
    /// A buffer pointer, which only indexing turns into a place.
    Pointer(Place),
}

pub(super) struct Lowerer<'c, 'p> {
    pub context: &'c Context<'p>,
    pub stage: Stage,
    pub values: Vec<Type>,
    pub blocks: Vec<Block>,
    current: usize,
    /// Whether the current block already has its terminator.
    terminated: bool,
    /// `thread` variables, declared in the entry block.
    locals: Vec<Instruction>,
    scopes: Vec<HashMap<String, Binding>>,
    /// The continue and break targets of each enclosing loop.
    /// Each enclosing loop's check block and its `break` flag.
    loops: Vec<(BlockId, Place)>,
    /// The source return type and how each returned field reaches the IR
    /// result.
    returns: Returns,
    result: Type,
    /// The `threadgroup` variables this function declares, which are module
    /// globals numbered from `globals_base`.
    globals: Vec<GlobalVariable>,
    globals_base: u32,
}

/// How a `return` builds the entry point's result.
enum Returns {
    Void,
    /// One value, widened to the interface type.
    Single {
        source: Ty,
        interface: Ty,
    },
    /// A structure, each field widened to its interface type.
    Struct {
        index: usize,
        interface: Vec<Ty>,
    },
}

impl<'c, 'p> Lowerer<'c, 'p> {
    fn new(context: &'c Context<'p>, stage: Stage) -> Self {
        Lowerer {
            context,
            stage,
            values: Vec::new(),
            blocks: vec![Block {
                id: BlockId(0),
                name: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Unreachable,
            }],
            current: 0,
            terminated: false,
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            loops: Vec::new(),
            returns: Returns::Void,
            result: Type::Void,
            globals: Vec::new(),
            globals_base: 0,
        }
    }

    pub fn value(&mut self, ty: Type) -> ValueId {
        self.values.push(ty);
        ValueId(self.values.len() as u32 - 1)
    }

    /// Append an instruction with a result of `ty` to the current block.
    pub fn emit(&mut self, ty: Type, kind: InstructionKind) -> Operand {
        let result = self.value(ty);
        self.blocks[self.current].instructions.push(Instruction {
            result: Some(result),
            kind,
        });
        Operand::Value(result)
    }

    /// Append an instruction with no result.
    pub fn emit_void(&mut self, kind: InstructionKind) {
        self.blocks[self.current]
            .instructions
            .push(Instruction { result: None, kind });
    }

    pub fn new_block(&mut self, name: &str) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(Block {
            id,
            name: format!("{name}{}", id.0),
            instructions: Vec::new(),
            terminator: Terminator::Unreachable,
        });
        id
    }

    pub fn switch_to(&mut self, block: BlockId) {
        self.current = block.0 as usize;
        self.terminated = false;
    }

    /// End the current block. Code after a `return`, `break` or `continue`
    /// goes to a block nothing reaches, which a pass removes.
    pub fn terminate(&mut self, terminator: Terminator) {
        if !self.terminated {
            self.blocks[self.current].terminator = terminator;
            self.terminated = true;
        }
    }

    fn after_jump(&mut self) {
        let dead = self.new_block("dead");
        self.switch_to(dead);
    }

    /// A `thread` variable of `ty`, declared in the entry block.
    pub fn local(&mut self, ty: &Ty) -> Place {
        let ir_ty = self.context.ir(ty);
        let result = self.value(Type::Pointer {
            space: AddressSpace::Thread,
            pointee: Box::new(ir_ty.clone()),
        });
        self.locals.push(Instruction {
            result: Some(result),
            kind: InstructionKind::LocalVariable { ty: ir_ty },
        });
        Place {
            root: Operand::Value(result),
            space: AddressSpace::Thread,
            indices: Vec::new(),
            ty: ty.clone(),
        }
    }

    /// A local for a value of `ty`: one variable, or one per field of a
    /// structure.
    pub fn local_lvalue(&mut self, ty: &Ty) -> LValue {
        match ty {
            Ty::Struct(index) => {
                let fields = self.context.program.structs[*index]
                    .fields
                    .iter()
                    .map(|field| field.ty.clone())
                    .collect::<Vec<_>>();
                LValue::Fields(
                    *index,
                    fields
                        .iter()
                        .map(|field| self.local_lvalue(field))
                        .collect(),
                )
            }
            ty => LValue::Memory(self.local(ty)),
        }
    }

    pub fn bind(&mut self, name: &str, binding: Binding) {
        self.scopes
            .last_mut()
            .expect("a scope")
            .insert(name.to_string(), binding);
    }

    pub fn lookup(&self, name: &str) -> Option<Binding> {
        for scope in self.scopes.iter().rev() {
            if let Some(binding) = scope.get(name) {
                return Some(binding.clone());
            }
        }
        let (index, ty) = self.context.globals.get(name)?;
        Some(Binding::LValue(LValue::Memory(Place {
            root: Operand::Global {
                index: *index,
                ty: Type::Pointer {
                    space: AddressSpace::Constant,
                    pointee: Box::new(self.context.ir(ty)),
                },
            },
            space: AddressSpace::Constant,
            indices: vec![Operand::u32(0)],
            ty: ty.clone(),
        })))
    }

    fn finish(mut self, name: &str, parameters: Vec<Parameter>) -> Function {
        // The blocks code after a jump went to are reached by nothing, and
        // a block nothing reaches would make every block it branches to look
        // undominated.
        let mut reached = vec![false; self.blocks.len()];
        let mut pending = vec![0usize];
        while let Some(at) = pending.pop() {
            if std::mem::replace(&mut reached[at], true) {
                continue;
            }
            pending.extend(
                ir::successors(&self.blocks[at].terminator)
                    .into_iter()
                    .map(|block| block.0 as usize),
            );
        }
        let mut index = 0;
        self.blocks.retain(|_| {
            index += 1;
            reached[index - 1]
        });
        // The entry block's variables come first, before any of its code.
        let mut entry = std::mem::take(&mut self.locals);
        entry.append(&mut self.blocks[0].instructions);
        self.blocks[0].instructions = entry;
        Function {
            name: name.to_string(),
            result: self.result,
            parameters,
            values: self.values,
            blocks: self.blocks,
        }
    }

    fn statements(&mut self, statements: &[Stmt]) -> Result<()> {
        self.scopes.push(HashMap::new());
        for statement in statements {
            self.statement(statement)?;
        }
        self.scopes.pop();
        Ok(())
    }

    fn statement(&mut self, statement: &Stmt) -> Result<()> {
        match statement {
            Stmt::Threadgroup { ty, name, line } => {
                if self.stage != Stage::Kernel {
                    return Err(syntax_error(
                        *line,
                        "a threadgroup variable outside a kernel",
                    ));
                }
                if matches!(ty, Ty::Texture(_) | Ty::Sampler | Ty::Void) {
                    return Err(syntax_error(
                        *line,
                        "a threadgroup variable of a non-data type",
                    ));
                }
                let index = self.globals_base + self.globals.len() as u32;
                let pointee = self.context.ir(ty);
                self.globals.push(GlobalVariable {
                    name: name.clone(),
                    space: AddressSpace::Threadgroup,
                    ty: pointee.clone(),
                    initializer: None,
                    function_constant: None,
                });
                self.bind(
                    name,
                    Binding::LValue(LValue::Memory(Place {
                        root: Operand::Global {
                            index,
                            ty: Type::Pointer {
                                space: AddressSpace::Threadgroup,
                                pointee: Box::new(pointee),
                            },
                        },
                        space: AddressSpace::Threadgroup,
                        indices: vec![Operand::u32(0)],
                        ty: ty.clone(),
                    })),
                );
            }
            Stmt::Decl {
                ty,
                name,
                init,
                line,
            } => {
                if matches!(ty, Ty::Texture(_) | Ty::Sampler) {
                    // `constexpr sampler s(...)` is a sampler the shader
                    // describes itself, which needs an immutable sampler.
                    return Err(syntax_error(
                        *line,
                        "a texture or sampler local is not supported",
                    ));
                }
                let lvalue = self.local_lvalue(ty);
                if let Some(init) = init {
                    match (&init.kind, ty) {
                        (ExprKind::InitList(items), Ty::Array(element, _)) => {
                            for (index, item) in items.iter().enumerate() {
                                let value = self.rvalue(item)?;
                                let value = self.convert(value, element, item.line)?;
                                let LValue::Memory(place) = &lvalue else {
                                    unreachable!("an array local is one variable");
                                };
                                let mut element_place = place.clone();
                                if element_place.indices.is_empty() {
                                    element_place.indices.push(Operand::u32(0));
                                }
                                element_place.indices.push(Operand::u32(index as u32));
                                element_place.ty = (**element).clone();
                                self.store(&LValue::Memory(element_place), value, item.line)?;
                            }
                        }
                        // `S s = { a, b };` is `(S){ a, b }`: the fields in
                        // declaration order.
                        (ExprKind::InitList(items), Ty::Struct(_)) => {
                            let literal = Expr {
                                kind: ExprKind::CompoundLiteral(
                                    ty.clone(),
                                    items.iter().map(|item| (None, item.clone())).collect(),
                                ),
                                line: init.line,
                            };
                            let value = self.rvalue(&literal)?;
                            self.store(&lvalue, value, *line)?;
                        }
                        _ => {
                            let value = self.rvalue(init)?;
                            let value = self.convert(value, ty, *line)?;
                            self.store(&lvalue, value, *line)?;
                        }
                    }
                }
                self.bind(name, Binding::LValue(lvalue));
            }
            Stmt::Expr(expr) => {
                self.effect(expr)?;
            }
            Stmt::Block(statements) => self.statements(statements)?,
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                let condition = self.condition(condition)?;
                let then_block = self.new_block("then");
                let merge = self.new_block("endif");
                let else_block = match otherwise {
                    Some(_) => self.new_block("else"),
                    None => merge,
                };
                self.terminate(Terminator::CondBranch {
                    condition,
                    true_block: then_block,
                    false_block: else_block,
                });
                self.switch_to(then_block);
                self.statements(then)?;
                self.terminate(Terminator::Branch(merge));
                if let Some(otherwise) = otherwise {
                    self.switch_to(else_block);
                    self.statements(otherwise)?;
                    self.terminate(Terminator::Branch(merge));
                }
                self.switch_to(merge);
            }
            Stmt::While { condition, body } => {
                let header = self.new_block("while");
                let body_block = self.new_block("body");
                let exit = self.new_block("endwhile");
                let flag = self.loop_flag();
                self.terminate(Terminator::Branch(header));
                self.switch_to(header);
                let condition = self.condition(condition)?;
                self.terminate(Terminator::CondBranch {
                    condition,
                    true_block: body_block,
                    false_block: exit,
                });
                self.switch_to(body_block);
                self.loop_body(body, flag, header, exit)?;
                self.switch_to(exit);
            }
            Stmt::DoWhile { body, condition } => {
                let body_block = self.new_block("do");
                let latch = self.new_block("dolatch");
                let exit = self.new_block("enddo");
                let flag = self.loop_flag();
                self.terminate(Terminator::Branch(body_block));
                self.switch_to(body_block);
                self.loop_body(body, flag, latch, exit)?;
                self.switch_to(latch);
                let condition = self.condition(condition)?;
                self.terminate(Terminator::CondBranch {
                    condition,
                    true_block: body_block,
                    false_block: exit,
                });
                self.switch_to(exit);
            }
            Stmt::For {
                init,
                condition,
                step,
                body,
            } => {
                self.scopes.push(HashMap::new());
                for statement in init {
                    self.statement(statement)?;
                }
                let header = self.new_block("for");
                let body_block = self.new_block("forbody");
                let latch = self.new_block("forstep");
                let exit = self.new_block("endfor");
                let flag = self.loop_flag();
                self.terminate(Terminator::Branch(header));
                self.switch_to(header);
                match condition {
                    Some(condition) => {
                        let condition = self.condition(condition)?;
                        self.terminate(Terminator::CondBranch {
                            condition,
                            true_block: body_block,
                            false_block: exit,
                        });
                    }
                    None => self.terminate(Terminator::Branch(body_block)),
                }
                self.switch_to(body_block);
                self.loop_body(body, flag, latch, exit)?;
                self.switch_to(latch);
                if let Some(step) = step {
                    self.effect(step)?;
                }
                self.terminate(Terminator::Branch(header));
                self.switch_to(exit);
                self.scopes.pop();
            }
            Stmt::Break(line) => {
                let (check, flag) = self
                    .loops
                    .last()
                    .cloned()
                    .ok_or_else(|| syntax_error(*line, "break outside a loop"))?;
                let truth = Value {
                    op: Operand::Bool(true),
                    ty: Ty::Scalar(Scalar::Bool),
                };
                self.store(&LValue::Memory(flag), truth, *line)?;
                self.terminate(Terminator::Branch(check));
                self.after_jump();
            }
            Stmt::Continue(line) => {
                let (check, _) = self
                    .loops
                    .last()
                    .cloned()
                    .ok_or_else(|| syntax_error(*line, "continue outside a loop"))?;
                self.terminate(Terminator::Branch(check));
                self.after_jump();
            }
            Stmt::Return(value, line) => {
                let result = self.return_value(value.as_ref(), *line)?;
                self.terminate(Terminator::Return(result));
                self.after_jump();
            }
        }
        Ok(())
    }

    /// A loop's `break` flag, cleared where the loop is entered.
    fn loop_flag(&mut self) -> Place {
        let ty = Ty::Scalar(Scalar::Bool);
        let flag = self.local(&ty);
        let pointer = flag.root.clone();
        self.emit_void(InstructionKind::Store {
            pointer,
            value: Operand::Bool(false),
        });
        flag
    }

    /// A loop body. `break`, `continue` and the end of the body all reach
    /// one check block, which alone leaves the loop: a natural loop holds
    /// only blocks which reach its back edge, so a `break` straight to the
    /// exit would make every breaking block an exit of its own.
    fn loop_body(
        &mut self,
        body: &[Stmt],
        flag: Place,
        next: BlockId,
        exit: BlockId,
    ) -> Result<()> {
        let check = self.new_block("loopcheck");
        self.loops.push((check, flag.clone()));
        self.statements(body)?;
        self.loops.pop();
        self.terminate(Terminator::Branch(check));
        self.switch_to(check);
        let broke = self.emit(Type::Bool, InstructionKind::Load { pointer: flag.root });
        self.terminate(Terminator::CondBranch {
            condition: broke,
            true_block: exit,
            false_block: next,
        });
        Ok(())
    }

    /// The operand an entry point returns: the source value converted to the
    /// return type, with each interface value widened.
    fn return_value(&mut self, value: Option<&Expr>, line: usize) -> Result<Option<Operand>> {
        match (&self.returns, value) {
            (Returns::Void, None) => Ok(None),
            (Returns::Void, Some(_)) => {
                Err(syntax_error(line, "a value returned from a void function"))
            }
            (_, None) => Err(syntax_error(
                line,
                "no value returned from a non-void function",
            )),
            (Returns::Single { source, interface }, Some(value)) => {
                let (source, interface) = (source.clone(), interface.clone());
                let value = self.rvalue(value)?;
                let value = self.convert(value, &source, line)?;
                Ok(Some(self.convert(value, &interface, line)?.op))
            }
            (Returns::Struct { index, interface }, Some(value)) => {
                let (index, interface) = (*index, interface.clone());
                let fields = self.struct_fields(value, index, line)?;
                let mut result = Operand::Undef(self.result.clone());
                for (position, (field, interface)) in fields.into_iter().zip(&interface).enumerate()
                {
                    let field = self.convert(field, interface, line)?;
                    result = self.emit(
                        self.result.clone(),
                        InstructionKind::CompositeInsert {
                            composite: result,
                            object: field.op,
                            index: position as u32,
                        },
                    );
                }
                Ok(Some(result))
            }
        }
    }

    /// Each field of a structure-typed expression, in order.
    pub fn struct_fields(&mut self, expr: &Expr, index: usize, line: usize) -> Result<Vec<Value>> {
        if let Some(lvalue) = self.lvalue(expr)?
            && let LValue::Fields(found, fields) = &lvalue
            && *found == index
        {
            return fields.iter().map(|field| self.load(field, line)).collect();
        }
        let value = self.rvalue(expr)?;
        if value.ty != Ty::Struct(index) {
            return Err(syntax_error(line, "a structure of the wrong type"));
        }
        self.split_struct(value, index)
    }

    /// The fields of a structure value.
    pub fn split_struct(&mut self, value: Value, index: usize) -> Result<Vec<Value>> {
        let fields = self.context.program.structs[index].fields.clone();
        Ok(fields
            .iter()
            .enumerate()
            .map(|(position, field)| Value {
                op: self.emit(
                    self.context.ir(&field.ty),
                    InstructionKind::CompositeExtract {
                        composite: value.op.clone(),
                        index: position as u32,
                    },
                ),
                ty: field.ty.clone(),
            })
            .collect())
    }

    /// An entry point and its IR function.
    fn entry(
        context: &'c Context<'p>,
        function: &FunctionDef,
        stage: Stage,
        index: usize,
        globals: &mut Vec<GlobalVariable>,
    ) -> Result<(Function, EntryPoint)> {
        let mut lowerer = Lowerer::new(context, stage);
        lowerer.globals_base = globals.len() as u32;
        let mut parameters = Vec::new();
        let mut entry = EntryPoint {
            function: index,
            stage,
            resources: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        };
        // Varyings are numbered in the order they are declared; the runtime
        // links a vertex function's to a fragment function's by name.
        let mut next_location = 0u32;
        for param in &function.params {
            lowerer.parameter(param, &mut parameters, &mut entry, &mut next_location)?;
        }
        lowerer.returns(function, &mut entry)?;
        lowerer.statements(&function.body)?;
        // Falling off the end of a function returns nothing, which only a
        // void function may do.
        if !lowerer.terminated {
            if matches!(lowerer.returns, Returns::Void) {
                lowerer.terminate(Terminator::Return(None));
            } else {
                lowerer.terminate(Terminator::Unreachable);
            }
        }
        globals.append(&mut lowerer.globals);
        let lowered = lowerer.finish(&function.name, parameters);
        Ok((lowered, entry))
    }

    /// Add an IR parameter of `ty`, answering its value.
    fn add_parameter(
        &mut self,
        parameters: &mut Vec<Parameter>,
        name: &str,
        ty: Type,
    ) -> (u32, Operand) {
        let value = self.value(ty.clone());
        parameters.push(Parameter {
            value,
            name: name.to_string(),
            ty,
        });
        (parameters.len() as u32 - 1, Operand::Value(value))
    }

    fn parameter(
        &mut self,
        param: &Param,
        parameters: &mut Vec<Parameter>,
        entry: &mut EntryPoint,
        next_location: &mut u32,
    ) -> Result<()> {
        let line = param.line;
        let attribute = param.attributes.first();
        if param.attributes.len() > 1 {
            return Err(syntax_error(line, "more than one attribute on a parameter"));
        }
        let Some(attribute) = attribute else {
            return Err(syntax_error(
                line,
                format!("parameter {} has no attribute", param.name),
            ));
        };
        match attribute.name.as_str() {
            "buffer" => {
                let index = self.context.const_index(single(attribute)?)?;
                if param.indirection == Indirection::Value {
                    return Err(syntax_error(line, "a buffer passed by value"));
                }
                let space = match param.space {
                    Some(Space::Constant) => AddressSpace::Constant,
                    Some(Space::Device) => AddressSpace::Device,
                    _ => {
                        return Err(syntax_error(
                            line,
                            "a buffer outside the constant or device address space",
                        ));
                    }
                };
                let ty = Type::Pointer {
                    space,
                    pointee: Box::new(self.context.ir(&param.ty)),
                };
                let (argument, root) = self.add_parameter(parameters, &param.name, ty.clone());
                entry.resources.push(Resource {
                    argument,
                    kind: ResourceKind::Buffer,
                    index,
                    count: 1,
                    // Whether the source wrote `const` is not kept; a
                    // `constant` buffer is read-only whatever it says.
                    access: if space == AddressSpace::Constant {
                        Access::Read
                    } else {
                        Access::ReadWrite
                    },
                    ty,
                    name: Some(param.name.clone()),
                    inline_sampler: None,
                });
                let place = Place {
                    root,
                    space,
                    indices: Vec::new(),
                    ty: param.ty.clone(),
                };
                let binding = match param.indirection {
                    Indirection::Reference => Binding::LValue(LValue::Memory(Place {
                        indices: vec![Operand::u32(0)],
                        ..place
                    })),
                    _ => Binding::Pointer(place),
                };
                self.bind(&param.name, binding);
            }
            INLINE_SAMPLER => {
                let ExprKind::Int { value, .. } = single(attribute)?.kind else {
                    unreachable!("the parser writes the index as a literal");
                };
                let sampler = self.context.program.inline_samplers[value as usize];
                let index = u32::try_from(value)
                    .ok()
                    .and_then(|value| value.checked_add(crate::ir::INLINE_SAMPLER_BASE))
                    .filter(|&index| index < 256)
                    .ok_or_else(|| syntax_error(line, "too many constexpr samplers"))?;
                let ty = self.context.ir(&param.ty);
                let (argument, op) = self.add_parameter(parameters, &param.name, ty.clone());
                entry.resources.push(Resource {
                    argument,
                    kind: ResourceKind::Sampler,
                    index,
                    count: 1,
                    access: Access::Read,
                    ty,
                    name: Some(param.name.clone()),
                    inline_sampler: Some(sampler),
                });
                self.bind(
                    &param.name,
                    Binding::Value(Value {
                        op,
                        ty: param.ty.clone(),
                    }),
                );
            }
            "texture" | "sampler" => {
                let index = self.context.const_index(single(attribute)?)?;
                let (kind, access) = match (&param.ty, attribute.name.as_str()) {
                    (Ty::Texture(texture), "texture") => (ResourceKind::Texture, texture.access),
                    (Ty::Sampler, "sampler") => (ResourceKind::Sampler, Access::Read),
                    _ => return Err(syntax_error(line, "a resource attribute on the wrong type")),
                };
                if param.indirection != Indirection::Value {
                    return Err(syntax_error(
                        line,
                        "a texture or sampler passed by reference",
                    ));
                }
                let ty = self.context.ir(&param.ty);
                let (argument, op) = self.add_parameter(parameters, &param.name, ty.clone());
                entry.resources.push(Resource {
                    argument,
                    kind,
                    index,
                    count: 1,
                    access,
                    ty,
                    name: Some(param.name.clone()),
                    inline_sampler: None,
                });
                self.bind(
                    &param.name,
                    Binding::Value(Value {
                        op,
                        ty: param.ty.clone(),
                    }),
                );
            }
            "stage_in" => {
                let Ty::Struct(index) = param.ty else {
                    return Err(syntax_error(
                        line,
                        "a [[stage_in]] parameter must be a structure",
                    ));
                };
                let fields = self.context.program.structs[index].fields.clone();
                let mut lvalues = Vec::new();
                for field in &fields {
                    let input = self.stage_input(field, next_location)?;
                    let interface = interface_ty(&field.ty, field.line)?;
                    let (argument, op) =
                        self.add_parameter(parameters, &field.name, self.context.ir(&interface));
                    entry.inputs.push(StageInput {
                        argument,
                        binding: input.0,
                        name: Some(input.1),
                        signed: matches!(field.ty.scalar(), Some(Scalar::Int { signed: true, .. }))
                            && self.stage == Stage::Vertex,
                    });
                    let value = Value { op, ty: interface };
                    let value = self.convert(value, &field.ty, field.line)?;
                    let lvalue = self.local_lvalue(&field.ty);
                    self.store(&lvalue, value, field.line)?;
                    lvalues.push(lvalue);
                }
                self.bind(&param.name, Binding::LValue(LValue::Fields(index, lvalues)));
            }
            name => {
                let builtin = builtin_named(name, self.stage).ok_or_else(|| {
                    syntax_error(line, format!("unsupported parameter attribute [[{name}]]"))
                })?;
                let natural = builtin_ty(builtin);
                let (argument, op) =
                    self.add_parameter(parameters, &param.name, self.context.ir(&natural));
                entry.inputs.push(StageInput {
                    argument,
                    binding: StageBinding::Builtin(builtin),
                    name: Some(param.name.clone()),
                    signed: false,
                });
                let value = self.builtin_value(Value { op, ty: natural }, &param.ty, line)?;
                let lvalue = self.local_lvalue(&param.ty);
                self.store(&lvalue, value, line)?;
                self.bind(&param.name, Binding::LValue(lvalue));
            }
        }
        Ok(())
    }

    /// A built-in input reshaped to the type the source declared: a
    /// position in the grid declared `uint2` reads two of its three lanes.
    fn builtin_value(&mut self, value: Value, declared: &Ty, line: usize) -> Result<Value> {
        match (&value.ty, declared) {
            (Ty::Vector(_, 3), Ty::Vector(scalar, lanes)) if *lanes < 3 => {
                let lanes = (0..u32::from(*lanes)).collect::<Vec<_>>();
                let shuffled = self.swizzle(value, &lanes, line)?;
                self.convert(shuffled, &Ty::Vector(*scalar, lanes.len() as u8), line)
            }
            (Ty::Vector(_, 3), Ty::Scalar(scalar)) => {
                let lane = self.swizzle(value, &[0], line)?;
                self.convert(lane, &Ty::Scalar(*scalar), line)
            }
            _ => self.convert(value, declared, line),
        }
    }

    /// Where a field of a `[[stage_in]]` structure comes from, and its name.
    fn stage_input(
        &self,
        field: &Field,
        next_location: &mut u32,
    ) -> Result<(StageBinding, String)> {
        let mut binding = None;
        let mut interpolation = None;
        for attribute in &field.attributes {
            match attribute.name.as_str() {
                "attribute" if self.stage == Stage::Vertex => {
                    binding = Some((
                        StageBinding::Location {
                            location: self.context.const_index(single(attribute)?)?,
                            interpolation: None,
                        },
                        field.name.clone(),
                    ));
                }
                "user" if self.stage == Stage::Fragment => {
                    binding = Some((self.varying(next_location, None), user_name(attribute)?));
                }
                "flat" => interpolation = Some((Sampling::Center, true, true)),
                "center_perspective" => interpolation = Some((Sampling::Center, true, false)),
                "center_no_perspective" => interpolation = Some((Sampling::Center, false, false)),
                "centroid_perspective" => interpolation = Some((Sampling::Centroid, true, false)),
                "centroid_no_perspective" => {
                    interpolation = Some((Sampling::Centroid, false, false))
                }
                "sample_perspective" => interpolation = Some((Sampling::Sample, true, false)),
                "sample_no_perspective" => interpolation = Some((Sampling::Sample, false, false)),
                name => {
                    let builtin = builtin_named(name, self.stage).ok_or_else(|| {
                        syntax_error(
                            attribute.line,
                            format!("unsupported stage input [[{name}]]"),
                        )
                    })?;
                    binding = Some((StageBinding::Builtin(builtin), field.name.clone()));
                }
            }
        }
        let (binding, name) = match binding {
            Some(found) => found,
            // A fragment input with no attribute is a varying named by its
            // field, as the vertex output it comes from is.
            None if self.stage == Stage::Fragment => (
                self.varying(next_location, None),
                format!("user({})", field.name),
            ),
            None => {
                return Err(syntax_error(
                    field.line,
                    format!("stage input {} has no attribute", field.name),
                ));
            }
        };
        let binding = match (binding, interpolation) {
            (StageBinding::Location { location, .. }, Some((sampling, perspective, flat)))
                if self.stage == Stage::Fragment =>
            {
                StageBinding::Location {
                    location,
                    interpolation: Some(Interpolation {
                        sampling,
                        perspective,
                        flat,
                    }),
                }
            }
            (binding, _) => binding,
        };
        Ok((binding, name))
    }

    fn varying(
        &self,
        next_location: &mut u32,
        interpolation: Option<Interpolation>,
    ) -> StageBinding {
        let location = *next_location;
        *next_location += 1;
        StageBinding::Location {
            location,
            interpolation,
        }
    }

    /// Declare the entry point's outputs from its return type.
    fn returns(&mut self, function: &FunctionDef, entry: &mut EntryPoint) -> Result<()> {
        let line = function.line;
        match (&function.ret, self.stage) {
            (Ty::Void, _) => {}
            (Ty::Struct(index), Stage::Vertex | Stage::Fragment) => {
                let fields = self.context.program.structs[*index].fields.clone();
                let mut interface = Vec::new();
                let mut layout = Vec::new();
                let mut next_location = 0u32;
                for (position, field) in fields.iter().enumerate() {
                    let (binding, name) = self.stage_output(field, &mut next_location)?;
                    entry.outputs.push(StageOutput {
                        field: Some(position as u32),
                        binding,
                        name: Some(name),
                    });
                    let ty = interface_ty(&field.ty, field.line)?;
                    layout.push(StructField {
                        name: Some(field.name.clone()),
                        ty: self.context.ir(&ty),
                        offset: 16 * position as u32,
                    });
                    interface.push(ty);
                }
                let count = layout.len() as u32;
                self.result = Type::Struct(StructType {
                    fields: layout,
                    size: 16 * count.max(1),
                    alignment: 16,
                    packed: false,
                });
                self.returns = Returns::Struct {
                    index: *index,
                    interface,
                };
            }
            (ty @ (Ty::Scalar(_) | Ty::Vector(..)), Stage::Vertex | Stage::Fragment) => {
                let binding = match self.stage {
                    Stage::Vertex => StageBinding::Builtin(Builtin::Position),
                    _ => StageBinding::RenderTarget { index: 0 },
                };
                entry.outputs.push(StageOutput {
                    field: None,
                    binding,
                    name: None,
                });
                let interface = interface_ty(ty, line)?;
                self.result = self.context.ir(&interface);
                self.returns = Returns::Single {
                    source: ty.clone(),
                    interface,
                };
            }
            _ => return Err(syntax_error(line, "unsupported entry point return type")),
        }
        Ok(())
    }

    fn stage_output(
        &self,
        field: &Field,
        next_location: &mut u32,
    ) -> Result<(StageBinding, String)> {
        let mut found = None;
        for attribute in &field.attributes {
            let binding = match (attribute.name.as_str(), self.stage) {
                ("user", Stage::Vertex) => {
                    (self.varying(next_location, None), user_name(attribute)?)
                }
                ("color", Stage::Fragment) => (
                    StageBinding::RenderTarget {
                        index: self.context.const_index(attribute.args.first().ok_or_else(
                            || syntax_error(attribute.line, "color() needs an index"),
                        )?)?,
                    },
                    field.name.clone(),
                ),
                // Fragment interpolation qualifiers matter on the input side.
                (
                    "flat"
                    | "center_perspective"
                    | "center_no_perspective"
                    | "centroid_perspective"
                    | "centroid_no_perspective"
                    | "sample_perspective"
                    | "sample_no_perspective"
                    | "invariant",
                    Stage::Vertex,
                ) => continue,
                ("depth", Stage::Fragment) => {
                    (StageBinding::Builtin(Builtin::Depth), field.name.clone())
                }
                (name, stage) => {
                    let builtin = builtin_named(name, stage).ok_or_else(|| {
                        syntax_error(
                            attribute.line,
                            format!("unsupported stage output [[{name}]]"),
                        )
                    })?;
                    (StageBinding::Builtin(builtin), field.name.clone())
                }
            };
            found = Some(binding);
        }
        match found {
            Some(found) => Ok(found),
            // A vertex output with no attribute is a varying named by its
            // field.
            None if self.stage == Stage::Vertex => Ok((
                self.varying(next_location, None),
                format!("user({})", field.name),
            )),
            None => Err(syntax_error(
                field.line,
                format!("stage output {} has no attribute", field.name),
            )),
        }
    }
}

fn single(attribute: &Attribute) -> Result<&Expr> {
    match attribute.args.as_slice() {
        [arg] => Ok(arg),
        _ => Err(syntax_error(
            attribute.line,
            format!("[[{}]] takes one argument", attribute.name),
        )),
    }
}

/// `user(NAME)` spelled as Metal links it.
fn user_name(attribute: &Attribute) -> Result<String> {
    match single(attribute)?.kind {
        ExprKind::Ident(ref name) => Ok(format!("user({name})")),
        _ => Err(syntax_error(attribute.line, "user() takes a name")),
    }
}

/// The type a stage interface variable is declared with: 32-bit wherever the
/// source used a narrower scalar, so the interface never needs Vulkan's
/// 16-bit input/output feature. The value is converted at the boundary.
fn interface_ty(ty: &Ty, line: usize) -> Result<Ty> {
    let widen = |scalar: Scalar| match scalar {
        Scalar::Float { .. } => Scalar::FLOAT,
        Scalar::Int { signed, .. } => Scalar::Int { signed, bits: 32 },
        Scalar::Bool => Scalar::Bool,
    };
    match ty {
        Ty::Scalar(scalar) => Ok(Ty::Scalar(widen(*scalar))),
        Ty::Vector(scalar, lanes) => Ok(Ty::Vector(widen(*scalar), *lanes)),
        _ => Err(syntax_error(
            line,
            "a stage interface value must be a scalar or vector",
        )),
    }
}

fn builtin_named(name: &str, stage: Stage) -> Option<Builtin> {
    Some(match (name, stage) {
        ("position", Stage::Vertex | Stage::Fragment) => Builtin::Position,
        ("point_size", Stage::Vertex) => Builtin::PointSize,
        ("vertex_id", Stage::Vertex) => Builtin::VertexId,
        ("instance_id", Stage::Vertex) => Builtin::InstanceId,
        ("base_vertex", Stage::Vertex) => Builtin::BaseVertex,
        ("base_instance", Stage::Vertex) => Builtin::BaseInstance,
        ("render_target_array_index", _) => Builtin::RenderTargetArrayIndex,
        ("viewport_array_index", _) => Builtin::ViewportArrayIndex,
        ("front_facing", Stage::Fragment) => Builtin::FrontFacing,
        ("point_coord", Stage::Fragment) => Builtin::PointCoord,
        ("sample_id", Stage::Fragment) => Builtin::SampleId,
        ("sample_mask", Stage::Fragment) => Builtin::SampleMask,
        ("thread_position_in_grid", Stage::Kernel) => Builtin::ThreadPositionInGrid,
        ("thread_position_in_threadgroup", Stage::Kernel) => Builtin::ThreadPositionInThreadgroup,
        ("threadgroup_position_in_grid", Stage::Kernel) => Builtin::ThreadgroupPositionInGrid,
        ("threadgroups_per_grid", Stage::Kernel) => Builtin::ThreadgroupsPerGrid,
        ("thread_index_in_threadgroup", Stage::Kernel) => Builtin::ThreadIndexInThreadgroup,
        ("threads_per_threadgroup", Stage::Kernel) => Builtin::ThreadsPerThreadgroup,
        _ => return None,
    })
}

/// The type the device delivers a built-in as.
fn builtin_ty(builtin: Builtin) -> Ty {
    match builtin {
        Builtin::Position => Ty::Vector(Scalar::FLOAT, 4),
        Builtin::PointSize | Builtin::Depth => Ty::Scalar(Scalar::FLOAT),
        Builtin::PointCoord => Ty::Vector(Scalar::FLOAT, 2),
        Builtin::FrontFacing => Ty::Scalar(Scalar::Bool),
        Builtin::ThreadPositionInGrid
        | Builtin::ThreadPositionInThreadgroup
        | Builtin::ThreadgroupPositionInGrid
        | Builtin::ThreadgroupsPerGrid
        | Builtin::ThreadsPerThreadgroup => Ty::Vector(Scalar::UINT, 3),
        _ => Ty::Scalar(Scalar::UINT),
    }
}
