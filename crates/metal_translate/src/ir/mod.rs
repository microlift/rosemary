mod operands;
mod verify;

use std::collections::BTreeSet;

use crate::{Diagnostic, Phase, TranslateError};

use operands::resource_namespace;
pub(crate) use operands::{
    coordinate_lanes, instruction_name, instruction_operands, instruction_operands_mut,
    is_boolean_or_boolean_vector, operand_type, successors, terminator_operands,
};
use verify::verify_function;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ValueId(pub u32);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlockId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    Vertex,
    Fragment,
    Kernel,
    Visible,
    Intersection,
    Mesh,
    Object,
    Tile,
}

impl Stage {
    pub fn ordinal(self) -> u32 {
        match self {
            Self::Kernel => 0,
            Self::Vertex => 1,
            Self::Fragment => 2,
            Self::Visible => 3,
            Self::Intersection => 4,
            Self::Mesh => 5,
            Self::Object => 6,
            Self::Tile => 7,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AddressSpace {
    Thread,
    Device,
    Constant,
    Threadgroup,
    ThreadgroupImageblock,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Access {
    /// Metal `access::sample`: read through a sampler. Only textures have it.
    Sample,
    Read,
    Write,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    Buffer,
    Texture,
    Sampler,
    AccelerationStructure,
    VisibleFunctionTable,
    IntersectionFunctionTable,
    ArgumentBuffer,
    Imageblock,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Type {
    Void,
    Bool,
    Int {
        bits: u8,
        signed: bool,
    },
    Float {
        bits: u8,
    },
    Vector {
        element: Box<Type>,
        lanes: u8,
    },
    Matrix {
        column: Box<Type>,
        columns: u8,
    },
    Array {
        element: Box<Type>,
        count: u32,
    },
    Struct(StructType),
    Pointer {
        space: AddressSpace,
        pointee: Box<Type>,
    },
    Texture(TextureType),
    Sampler,
    AccelerationStructure,
    VisibleFunctionTable,
    IntersectionFunctionTable,
    Imageblock,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TextureDimension {
    D1,
    D2,
    D3,
    Cube,
}

/// A Metal texture type. Metal spells the element type, dimensionality and
/// access mode in the type itself; Vulkan splits the same information across
/// an image type, a descriptor type and a capability.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TextureType {
    pub dimension: TextureDimension,
    pub array: bool,
    pub multisampled: bool,
    /// A `depthNd` texture, which Metal only allows to be sampled as depth.
    pub depth: bool,
    /// The scalar type of one texel component.
    pub texel: Box<Type>,
    /// Whether an integer texel is signed. `texel` stays signless like the
    /// values AIR reads, but Vulkan requires the image's sampled type to match
    /// the signedness of the view's format.
    pub signed: bool,
    pub access: Access,
}

impl TextureType {
    /// Whether Vulkan carries this texture as a sampled image rather than a
    /// storage image.
    ///
    /// Only a texture a shader writes is a storage image. Metal's `read()` is
    /// a texel fetch with no filtering, which a sampled image answers with
    /// `OpImageFetch` exactly as a storage image answers `OpImageRead`, and
    /// reading it as a storage image instead is wrong twice over: Metal's
    /// `shaderRead` usage asks for a sampled image and nothing more, so the
    /// image was never created with `VK_IMAGE_USAGE_STORAGE_BIT`, and many of
    /// the formats a game renders into - `A2B10G10R10`, a depth format, a
    /// compressed one - cannot be storage images on the adapter at all. What
    /// comes back from such a descriptor is the surface's own layout, which for
    /// a multisampled attachment is its samples interleaved across neighbouring
    /// pixels.
    pub fn sampled_image(&self) -> bool {
        !matches!(self.access, Access::Write | Access::ReadWrite)
    }

    /// The scalar type Vulkan's image type names, which carries the
    /// signedness `texel` leaves out.
    pub fn sampled_scalar(&self) -> Type {
        match *self.texel {
            Type::Int { bits, .. } => Type::Int {
                bits,
                signed: self.signed,
            },
            ref texel => texel.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct StructType {
    pub fields: Vec<StructField>,
    pub size: u32,
    pub alignment: u32,
    pub packed: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct StructField {
    pub name: Option<String>,
    pub ty: Type,
    pub offset: u32,
}

impl Type {
    /// The signed form of an integer scalar or vector; `None` for any other
    /// type.
    pub fn signed_integer(&self) -> Option<Type> {
        match self {
            Type::Int { bits, .. } => Some(Type::Int {
                bits: *bits,
                signed: true,
            }),
            Type::Vector { element, lanes } if matches!(**element, Type::Int { .. }) => {
                Some(Type::Vector {
                    element: Box::new(element.signed_integer()?),
                    lanes: *lanes,
                })
            }
            _ => None,
        }
    }

    pub fn u32() -> Self {
        Self::Int {
            bits: 32,
            signed: false,
        }
    }

    pub fn i32() -> Self {
        Self::Int {
            bits: 32,
            signed: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resource {
    pub argument: u32,
    pub kind: ResourceKind,
    pub index: u32,
    pub count: u32,
    pub access: Access,
    pub ty: Type,
    pub name: Option<String>,
}

/// How a fragment stage input is interpolated across a primitive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Interpolation {
    pub sampling: Sampling,
    pub perspective: bool,
    pub flat: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sampling {
    Center,
    Centroid,
    Sample,
}

/// A Metal shader built-in. These name Metal's semantics; the SPIR-V built-in
/// each maps to is a property of emission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Builtin {
    /// `[[position]]`: a vertex output, and the fragment stage's window
    /// coordinate input.
    Position,
    PointSize,
    ClipDistance,
    VertexId,
    InstanceId,
    BaseVertex,
    BaseInstance,
    FrontFacing,
    PointCoord,
    SampleId,
    SampleMask,
    Depth,
    RenderTargetArrayIndex,
    ViewportArrayIndex,
    ThreadPositionInGrid,
    ThreadPositionInThreadgroup,
    ThreadgroupPositionInGrid,
    ThreadgroupsPerGrid,
    ThreadIndexInThreadgroup,
    ThreadsPerThreadgroup,
}

/// Where one stage input or output is delivered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StageBinding {
    /// A user-defined attribute at a Metal location index.
    Location {
        location: u32,
        interpolation: Option<Interpolation>,
    },
    /// A fragment colour attachment.
    RenderTarget {
        index: u32,
    },
    Builtin(Builtin),
}

/// One entry-point parameter delivered by the stage rather than by a resource
/// binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StageInput {
    pub argument: u32,
    pub binding: StageBinding,
    pub name: Option<String>,
    /// A vertex attribute Metal declares as a signed integer. The parameter
    /// stays signless, but Vulkan matches the input's signedness against the
    /// attribute's `SINT` or `UINT` format.
    pub signed: bool,
}

/// One component of an entry point's returned value.
///
/// `field` selects a member of a returned structure; `None` means the whole
/// result is this single output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StageOutput {
    pub field: Option<u32>,
    pub binding: StageBinding,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryPoint {
    pub function: usize,
    pub stage: Stage,
    pub resources: Vec<Resource>,
    pub inputs: Vec<StageInput>,
    pub outputs: Vec<StageOutput>,
}

/// A module-scope variable.
///
/// Metal puts a shader's constant lookup tables and its `threadgroup` arrays
/// outside every function, so they belong to the module rather than to the
/// entry point which reads them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalVariable {
    pub name: String,
    pub space: AddressSpace,
    /// The type of what the variable holds, not the pointer to it.
    pub ty: Type,
    /// `None` for a `threadgroup` variable, which has no initial value.
    pub initializer: Option<Operand>,
    /// Set when the variable holds one of Metal's function constants, whose
    /// value is supplied when a function is made rather than when the library
    /// is compiled.
    pub function_constant: Option<FunctionConstantSlot>,
}

/// A `[[function_constant(index)]]` declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionConstantSlot {
    pub index: u32,
    /// The name Metal looks the constant up by, as well as by index.
    pub name: String,
    /// The Itanium mangling of the constant's Metal type, as AIR spells it:
    /// `b` for `bool`, `j` for `uint`, `Dv4_f` for `float4`.
    pub encoding: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
    pub functions: Vec<Function>,
    pub entries: Vec<EntryPoint>,
    pub globals: Vec<GlobalVariable>,
    /// Functions which compute derived constants into globals before any entry
    /// point runs, in the order they run. Specialization runs them, since
    /// what they compute depends on function constants; emission never sees
    /// one.
    pub static_initializers: Vec<Function>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    pub name: String,
    pub result: Type,
    pub parameters: Vec<Parameter>,
    pub values: Vec<Type>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parameter {
    pub value: ValueId,
    pub name: String,
    pub ty: Type,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    pub id: BlockId,
    pub name: String,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Instruction {
    pub result: Option<ValueId>,
    pub kind: InstructionKind,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Operand {
    Bool(bool),
    Value(ValueId),
    Integer {
        ty: Type,
        value: u64,
    },
    Float {
        ty: Type,
        bits: u64,
    },
    /// A constant vector, array or structure. Metal spells these as values,
    /// and some of its operations, such as a sampling offset, require one.
    Composite {
        ty: Type,
        elements: Vec<Operand>,
    },
    /// A pointer to one of the module's global variables. The type is the
    /// pointer's, carried here so an operand's type never needs the module.
    Global {
        index: u32,
        ty: Type,
    },
    Undef(Type),
}

impl Operand {
    pub fn u32(value: u32) -> Self {
        Self::Integer {
            ty: Type::u32(),
            value: u64::from(value),
        }
    }

    pub fn f32(value: f32) -> Self {
        Self::Float {
            ty: Type::Float { bits: 32 },
            bits: u64::from(value.to_bits()),
        }
    }
}

/// Integer comparison predicates, matching LLVM's `icmp` set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegerPredicate {
    Equal,
    NotEqual,
    UnsignedLessThan,
    UnsignedLessEqual,
    UnsignedGreaterThan,
    UnsignedGreaterEqual,
    SignedLessThan,
    SignedLessEqual,
    SignedGreaterThan,
    SignedGreaterEqual,
}

/// Floating comparison predicates, matching LLVM's `fcmp` set. Ordered
/// comparisons are false when either operand is NaN; unordered are true.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatPredicate {
    OrderedEqual,
    OrderedNotEqual,
    OrderedLessThan,
    OrderedLessEqual,
    OrderedGreaterThan,
    OrderedGreaterEqual,
    Ordered,
    UnorderedEqual,
    UnorderedNotEqual,
    UnorderedLessThan,
    UnorderedLessEqual,
    UnorderedGreaterThan,
    UnorderedGreaterEqual,
    Unordered,
}

/// Elementwise integer arithmetic. Signedness is carried by the operation
/// rather than by the operand type: Metal's `uint`/`int` distinction is
/// already lost in AIR, where LLVM picks `udiv`/`sdiv` instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegerBinaryOp {
    Add,
    Subtract,
    Multiply,
    DivideUnsigned,
    DivideSigned,
    RemainderUnsigned,
    RemainderSigned,
    ShiftLeft,
    ShiftRightLogical,
    ShiftRightArithmetic,
    And,
    Or,
    Xor,
    MinimumSigned,
    MinimumUnsigned,
    MaximumSigned,
    MaximumUnsigned,
}

/// Elementwise integer math library calls with one operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegerUnaryOp {
    AbsoluteSigned,
    SignSigned,
    PopulationCount,
    /// Metal's `clz`, which counts a value's leading zero bits and answers the
    /// full width for zero.
    CountLeadingZeros,
    /// Metal's `ctz`, the same for trailing bits.
    CountTrailingZeros,
    /// Index of the highest set bit, or -1 for zero.
    FindMostSignificantBitUnsigned,
    Not,
}

/// Elementwise floating arithmetic and two-operand math library calls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatBinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Minimum,
    Maximum,
    Power,
    Atan2,
    Step,
}

/// Elementwise floating math library calls with one operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatUnaryOp {
    Negate,
    Absolute,
    Sign,
    Sqrt,
    InverseSqrt,
    Exp,
    Exp2,
    Log,
    Log2,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Floor,
    Ceil,
    Round,
    Truncate,
    Fract,
    /// Screen-space derivatives, valid only in a fragment entry point.
    DerivativeX,
    DerivativeY,
    DerivativeWidth,
}

/// Numeric conversions. Each keeps the source interpretation explicit so the
/// backend never has to infer signedness from a result type alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConvertOp {
    UnsignedToFloat,
    SignedToFloat,
    FloatToUnsigned,
    FloatToSigned,
    /// Widen an integer, filling new bits with zero.
    ZeroExtend,
    /// Widen an integer, replicating its sign bit.
    SignExtend,
    /// Narrow an integer, discarding high bits.
    IntegerTruncate,
    /// Change floating precision in either direction.
    FloatResize,
    /// Reinterpret the bits of an equally sized value.
    Bitcast,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstructionKind {
    GlobalInvocationId,
    /// A function-scope variable: LLVM's `alloca`, which Metal spells as a
    /// `thread` local. The type is what the variable holds.
    LocalVariable {
        ty: Type,
    },
    Load {
        pointer: Operand,
    },
    Store {
        pointer: Operand,
        value: Operand,
    },
    IntegerBinary {
        op: IntegerBinaryOp,
        lhs: Operand,
        rhs: Operand,
    },
    IntegerCompare {
        predicate: IntegerPredicate,
        lhs: Operand,
        rhs: Operand,
    },
    FloatCompare {
        predicate: FloatPredicate,
        lhs: Operand,
        rhs: Operand,
    },
    ElementAddress {
        pointer: Operand,
        index: Operand,
    },
    FieldAddress {
        pointer: Operand,
        indices: Vec<u32>,
    },
    BufferAddress {
        pointer: Operand,
        indices: Vec<Operand>,
    },
    CompositeExtract {
        composite: Operand,
        index: u32,
    },
    /// `v[i]` where `i` is not known until the shader runs. Metal allows it on
    /// a vector, and only on a vector: a structure's members are named.
    VectorExtractDynamic {
        vector: Operand,
        index: Operand,
    },
    /// The same for an assignment to one lane.
    VectorInsertDynamic {
        vector: Operand,
        object: Operand,
        index: Operand,
    },
    All {
        vector: Operand,
    },
    Any {
        vector: Operand,
    },
    Convert {
        op: ConvertOp,
        value: Operand,
    },
    Select {
        condition: Operand,
        on_true: Operand,
        on_false: Operand,
    },
    FloatBinary {
        op: FloatBinaryOp,
        lhs: Operand,
        rhs: Operand,
    },
    FloatUnary {
        op: FloatUnaryOp,
        value: Operand,
    },
    /// Fused multiply-add: `first * second + addend`.
    FloatFma {
        first: Operand,
        second: Operand,
        addend: Operand,
    },
    VectorShuffle {
        first: Operand,
        second: Operand,
        components: Vec<u32>,
    },
    CompositeInsert {
        composite: Operand,
        object: Operand,
        index: u32,
    },
    IntegerUnary {
        op: IntegerUnaryOp,
        value: Operand,
    },
    FloatClamp {
        value: Operand,
        min: Operand,
        max: Operand,
    },
    IntegerClamp {
        signed: bool,
        value: Operand,
        min: Operand,
        max: Operand,
    },
    FloatMix {
        first: Operand,
        second: Operand,
        weight: Operand,
    },
    FloatDot {
        lhs: Operand,
        rhs: Operand,
    },
    VectorSplat {
        value: Operand,
        lanes: u8,
    },
    /// Read one texel through a Metal sampler.
    ///
    /// Metal keeps the array slice, the sampling offset and an explicit level
    /// of detail as separate arguments; combining the coordinate with the
    /// slice is a Vulkan detail and belongs to emission.
    TextureSample {
        texture: Operand,
        sampler: Operand,
        coordinate: Operand,
        array_index: Option<Operand>,
        offset: Option<Operand>,
        /// An explicit level of detail. Metal samples with an implicit level
        /// when this is absent, which is only valid in a fragment stage.
        lod: Option<Operand>,
        /// Explicit derivatives of the coordinate, as Metal's `gradient2d` and
        /// its siblings supply them. They name the level of detail the way an
        /// explicit `lod` does, and never appear alongside one.
        gradient: Option<Box<(Operand, Operand)>>,
        /// `gather` selects one component of the four sampled texels.
        gather: Option<u32>,
    },
    /// Read one texel by integer coordinate, without a sampler.
    TextureRead {
        texture: Operand,
        coordinate: Operand,
        array_index: Option<Operand>,
        lod: Option<Operand>,
        /// The sample index of a multisampled texture.
        sample: Option<Operand>,
    },
    /// Store one texel through a writable Metal texture resource.
    ///
    /// Coordinates and texels remain explicit SSA values: Metal's texture
    /// namespace is represented by the resource, not by a Vulkan descriptor.
    TextureWrite {
        texture: Operand,
        coordinate: Operand,
        array_index: Option<Operand>,
        value: Operand,
    },
    /// One component of a texture's dimensions at a level of detail.
    TextureQuerySize {
        texture: Operand,
        lod: Option<Operand>,
        component: u32,
    },
    /// Metal `discard_fragment()`. It does not terminate the block.
    DiscardFragment,
    /// Metal's `threadgroup_barrier` and `simdgroup_barrier`, which AIR spells
    /// `air.wg.barrier`. The scope is what waits; the memory flags are which
    /// address spaces the barrier orders.
    Barrier {
        scope: BarrierScope,
        memory: BarrierMemory,
    },
    /// A read-modify-write on one memory location, which AIR spells
    /// `air.atomic.<space>.<operation>.<signedness>.<type>`.
    ///
    /// Metal's memory order and scope arguments are carried as Metal names it
    /// them, not as SPIR-V semantics.
    Atomic {
        op: AtomicOp,
        pointer: Operand,
        value: Operand,
        order: MemoryOrder,
        scope: BarrierScope,
    },
    Phi {
        incomings: Vec<(BlockId, Operand)>,
    },
    /// Metal's `is_function_constant_defined`, asked of the global which
    /// holds one function constant. Specialization answers it; emission never
    /// sees one.
    FunctionConstantDefined {
        constant: Operand,
    },
}

/// What a barrier makes wait, and how far an atomic's ordering reaches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BarrierScope {
    /// Metal's SIMD-group: the invocations which execute in lockstep.
    Simdgroup,
    /// Metal's threadgroup, which is a Vulkan workgroup.
    Threadgroup,
    /// The whole device.
    Device,
}

/// The address spaces a barrier orders, which Metal spells as `mem_flags`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BarrierMemory {
    pub device: bool,
    pub threadgroup: bool,
    pub texture: bool,
}

/// Metal's `memory_order`, of which only `relaxed` and `seq_cst` exist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryOrder {
    Relaxed,
    SequentiallyConsistent,
}

/// The read-modify-write an [`InstructionKind::Atomic`] performs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomicOp {
    Add,
    Subtract,
    MinimumSigned,
    MinimumUnsigned,
    MaximumSigned,
    MaximumUnsigned,
    And,
    Or,
    Xor,
    Exchange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Terminator {
    Branch(BlockId),
    CondBranch {
        condition: Operand,
        true_block: BlockId,
        false_block: BlockId,
    },
    Return(Option<Operand>),
    Unreachable,
}

impl Module {
    pub fn verify(&self) -> Result<(), TranslateError> {
        let mut diagnostics = Vec::new();
        for entry in &self.entries {
            let Some(function) = self.functions.get(entry.function) else {
                diagnostics.push(Diagnostic::new(
                    Phase::IrVerification,
                    "entry point refers to a missing function",
                ));
                continue;
            };
            let mut bindings = BTreeSet::new();
            for resource in &entry.resources {
                if resource.count == 0 {
                    diagnostics.push(Diagnostic::new(
                        Phase::IrVerification,
                        format!("{} has an empty resource array", function.name),
                    ));
                }
                if !bindings.insert((resource_namespace(resource.kind), resource.index)) {
                    diagnostics.push(Diagnostic::new(
                        Phase::IrVerification,
                        format!("{} has a duplicate Metal resource index", function.name),
                    ));
                }
                if resource.argument as usize >= function.parameters.len() {
                    diagnostics.push(Diagnostic::new(
                        Phase::IrVerification,
                        format!("{} resource refers to a missing argument", function.name),
                    ));
                }
            }
        }
        for function in self.functions.iter().chain(&self.static_initializers) {
            verify_function(function, &mut diagnostics);
        }
        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(TranslateError { diagnostics })
        }
    }
}

impl Function {
    /// Verify one function on its own, without the entry points which refer
    /// to it.
    pub(crate) fn verify(&self) -> Result<(), TranslateError> {
        let mut diagnostics = Vec::new();
        verify_function(self, &mut diagnostics);
        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(TranslateError { diagnostics })
        }
    }
}
