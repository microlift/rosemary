use super::*;

/// `-[MTLDevice newDefaultLibrary]`: the main bundle's `default.metallib`.
pub(super) unsafe extern "C" fn new_default_library(receiver: Id, _sel: Sel) -> Id {
    let Some(path) =
        foundation::bundle::main().and_then(|bundle| bundle.resource("default", "metallib"))
    else {
        report_library_error("newDefaultLibrary found no default.metallib in the main bundle");
        return std::ptr::null_mut();
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            report_library_error(format!(
                "newDefaultLibrary could not read {}: {error}",
                path.display()
            ));
            return std::ptr::null_mut();
        }
    };
    // SAFETY: an `RosemaryMTLDevice`.
    let target = unsafe { object::body::<Device>(receiver) }.target;
    // SAFETY: the profile belongs to the process-wide adapter list.
    let Some(target) = (unsafe { target.as_ref() }) else {
        report_library_error("Metal device has no Vulkan target profile");
        return std::ptr::null_mut();
    };
    let (translated, input) = match translate_metallib(&bytes, target) {
        Ok(result) => result,
        Err(error) => {
            report_library_error(error);
            return std::ptr::null_mut();
        }
    };
    let library = retain_library(receiver, translated, input, Some(target));
    label_metallib(library, &bytes);
    library
}

pub(super) unsafe extern "C" fn new_library_with_data(
    receiver: Id,
    _sel: Sel,
    data: u64,
    error: *mut Id,
) -> Id {
    let Some((bytes, length)) = darwin::dispatch::data::bytes(data) else {
        return fail_library(
            error,
            "newLibraryWithData:error: received non-dispatch data",
        );
    };
    // SAFETY: dispatch reported this range as the data's bytes.
    let bytes = unsafe { std::slice::from_raw_parts(bytes, length) };
    library_from_metallib(receiver, bytes, error)
}

/// A library of the precompiled `bytes`, or nil with `error` filled in.
fn library_from_metallib(receiver: Id, bytes: &[u8], error: *mut Id) -> Id {
    // SAFETY: an `RosemaryMTLDevice`.
    let target = unsafe { object::body::<Device>(receiver) }.target;
    // SAFETY: the profile belongs to the process-wide adapter list.
    let Some(target) = (unsafe { target.as_ref() }) else {
        return fail_library(error, "Metal device has no Vulkan target profile");
    };
    let (translated, input) = match translate_metallib(bytes, target) {
        Ok(result) => result,
        Err(failure) => return fail_library(error, &failure.to_string()),
    };
    let library = retain_library(receiver, translated, input, Some(target));
    label_metallib(library, bytes);
    library
}

/// A library of the `.metallib` the guest names at `path`.
fn library_from_file(receiver: Id, path: Option<String>, error: *mut Id) -> Id {
    let Some(path) = path else {
        return fail_library(error, "no path to a metallib");
    };
    match std::fs::read(prefix::ns::existing_host_path(&path)) {
        Ok(bytes) => library_from_metallib(receiver, &bytes, error),
        Err(cause) => fail_library(error, &format!("could not read {path}: {cause}")),
    }
}

/// `-[MTLDevice newLibraryWithFile:error:]`
pub(super) unsafe extern "C" fn new_library_with_file(
    receiver: Id,
    _sel: Sel,
    path: Id,
    error: *mut Id,
) -> Id {
    // SAFETY: an `NSString`, or nil.
    let path = String::from_utf8(unsafe { foundation::string::bytes(path) }).ok();
    library_from_file(receiver, path.filter(|p| !p.is_empty()), error)
}

/// `-[MTLDevice newLibraryWithURL:error:]`
pub(super) unsafe extern "C" fn new_library_with_url(
    receiver: Id,
    _sel: Sel,
    url: Id,
    error: *mut Id,
) -> Id {
    // SAFETY: an `NSURL`, or nil.
    let path =
        unsafe { corefoundation::url::as_url(url as CFTypeRef) }.and_then(|url| url.file_path());
    library_from_file(receiver, path, error)
}

/// Name a precompiled library's entries for `ROSEMARY_METAL_PROFILE` by the
/// hash `ROSEMARY_MSL_OUT` files the library under, since precompiled
/// shaders tend to share one entry name.
fn label_metallib(library: Id, bytes: &[u8]) {
    if library.is_null() || !crate::profile::enabled() {
        return;
    }
    let hash = fnv1a64(bytes);
    if let Some(directory) = std::env::var_os("ROSEMARY_MSL_OUT") {
        let directory = std::path::PathBuf::from(directory);
        if std::fs::create_dir_all(&directory).is_ok() {
            let _ = std::fs::write(directory.join(format!("air-{hash:016x}.metallib")), bytes);
        }
    }
    // SAFETY: a library just made by `retain_library`.
    let library = unsafe { object::body::<Library>(library) };
    for (index, entry) in library.translated.entries.iter().enumerate() {
        crate::profile::label_entry(entry, format!("air-{hash:016x}#{index}:{}", entry.name));
    }
}

fn translate_metallib(
    bytes: &[u8],
    target: &metal_translate::TargetProfile,
) -> Result<
    (metal_translate::TranslatedLibrary, Option<LibraryInput>),
    metal_translate::TranslateError,
> {
    let modules = metal_translate::metallib_airs(bytes)?;
    // A module this translator cannot import only takes its own functions
    // out of the library, when the function list says which they are.
    let module_functions = metal_translate::metallib_module_functions(bytes).ok();
    let mut entries = Vec::new();
    let mut function_constants = Vec::new();
    let mut function_reflection = Vec::new();
    let mut rejected = Vec::new();
    let mut entry_modules = Vec::new();
    for (module, air) in modules.iter().enumerate() {
        let input = metal_translate::AirInput::Bitcode(air);
        let translated = match metal_translate::translate_cached(input, target) {
            Ok(translated) => translated,
            Err(error) => match module_functions.as_ref().map(|names| &names[module]) {
                Some(names) if !names.is_empty() => {
                    rejected.extend(names.iter().map(|name| metal_translate::RejectedEntry {
                        name: name.clone(),
                        reason: error.to_string(),
                    }));
                    continue;
                }
                _ => return Err(error),
            },
        };
        let offset = entries.len();
        function_reflection.extend(translated.function_reflection.into_iter().map(
            |mut function| {
                function.entry = function.entry.map(|entry| offset + entry);
                function
            },
        ));
        entry_modules.extend(std::iter::repeat_n(module, translated.entries.len()));
        entries.extend(translated.entries);
        function_constants.extend(translated.function_constants);
        rejected.extend(translated.rejected);
    }
    // Only a library with function constants is ever translated again.
    let input = (!function_constants.is_empty()).then(|| LibraryInput::Air {
        modules: modules.into_iter().map(<[u8]>::to_vec).collect(),
        entry_modules,
    });
    Ok((
        metal_translate::TranslatedLibrary {
            entries,
            function_constants,
            function_reflection,
            rejected,
        },
        input,
    ))
}

/// Wrap a translated library in the `MTLLibrary` object the guest holds.
pub(super) fn retain_library(
    device: Id,
    translated: metal_translate::TranslatedLibrary,
    input: Option<LibraryInput>,
    target: Option<&'static metal_translate::TargetProfile>,
) -> Id {
    if let Some(target) = target
        && let Some((entry, requirement)) = translated.entries.iter().find_map(|entry| {
            target
                .unsupported_requirement(&entry.required)
                .map(|requirement| (entry.name.as_str(), requirement))
        })
    {
        report_library_error(format!(
            "entry point {entry} requires {requirement}, which the Vulkan device did not enable"
        ));
        return std::ptr::null_mut();
    }
    let library = object::make(
        c"RosemaryMTLLibrary",
        Library {
            translated,
            input,
            target,
            device,
        },
    );
    if library.is_null() {
        report_library_error("could not allocate MTL library");
        return std::ptr::null_mut();
    }
    library
}

pub(super) unsafe extern "C" fn new_library_with_source(
    receiver: Id,
    _sel: Sel,
    source: Id,
    options: Id,
    error: *mut Id,
) -> Id {
    // SAFETY: a guest `NSString`, and an options object or nil.
    let (source, macros) = unsafe { source_inputs(source, options) };
    // SAFETY: the caller's error slot.
    unsafe { library_from_source(receiver, source.as_deref(), &macros, error) }
}

/// What `newLibraryWithSource:options:` reads of its arguments: the source's
/// bytes, `None` for nil, and the preprocessor macros.
///
/// # Safety
///
/// `source` must be null or an `NSString`, `options` null or an object.
pub(super) unsafe fn source_inputs(
    source: Id,
    options: Id,
) -> (Option<Vec<u8>>, Vec<(String, String)>) {
    // SAFETY: the caller's contract.
    let source = (!source.is_null()).then(|| unsafe { foundation::string::bytes(source) });
    let macros = if object::is_kind(options, c"MTLCompileOptions") {
        // SAFETY: an options object of this crate's.
        unsafe { object::body::<crate::pipeline::CompileOptions>(options) }
            .macros
            .clone()
    } else {
        Vec::new()
    };
    (source, macros)
}

/// `newLibraryWithSource:options:error:` over the inputs it read.
///
/// # Safety
///
/// `receiver` must be an `RosemaryMTLDevice`, and `error` null or writable.
pub(super) unsafe fn library_from_source(
    receiver: Id,
    source: Option<&[u8]>,
    macros: &[(String, String)],
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        // SAFETY: the caller's contract.
        unsafe { *error = std::ptr::null_mut() };
    }
    let Some(source) = source else {
        return fail_library(error, "newLibraryWithSource: requires an NSString source");
    };
    // SAFETY: an `RosemaryMTLDevice`.
    let target = unsafe { object::body::<Device>(receiver) }.target;
    // SAFETY: the profile belongs to the process-wide adapter list.
    let Some(target) = (unsafe { target.as_ref() }) else {
        return fail_library(error, "Metal device has no Vulkan target profile");
    };

    if source.is_empty() {
        return fail_library(
            error,
            "newLibraryWithSource: received an empty or invalid NSString",
        );
    }
    let Ok(source) = std::str::from_utf8(source) else {
        return fail_library(error, "Metal source is not valid UTF-8");
    };
    report_source_library(source);
    dump_source_library(source);
    let (translated, source_input) = match metal_translate::MslSource::load_cached(
        source,
        macros,
        target,
        metal_translate::PassPipeline::from_env(),
    ) {
        Ok(result) => result,
        Err(translation_error) => return fail_library(error, &translation_error.to_string()),
    };
    let library = retain_library(
        receiver,
        translated,
        source_input.map(LibraryInput::Msl),
        Some(target),
    );
    if library.is_null() {
        return fail_library(error, "could not allocate MTL source library");
    }
    if crate::profile::enabled() {
        let hash = fnv1a64(source.as_bytes());
        // SAFETY: the library just made.
        for entry in &unsafe { object::body::<Library>(library) }
            .translated
            .entries
        {
            crate::profile::label_entry(entry, format!("msl-{hash:016x}:{}", entry.name));
        }
    }
    library
}

fn function_definition(
    library: &Library,
    name: &str,
) -> Option<metal_translate::FunctionReflection> {
    if matches!(&library.input, Some(LibraryInput::Msl(_))) {
        return library
            .translated
            .function_reflection
            .iter()
            .find(|function| function.name == name)
            .cloned();
    }
    let (entry, artifact) = library
        .translated
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| entry.name == name)?;
    if let Some(reflection) = library
        .translated
        .function_reflection
        .iter()
        .find(|function| function.entry == Some(entry))
    {
        return Some(reflection.clone());
    }
    Some(metal_translate::FunctionReflection {
        name: artifact.name.clone(),
        stage: artifact.stage,
        entry: Some(entry),
        constants: Vec::new(),
    })
}

pub(super) unsafe extern "C" fn new_function_with_name(receiver: Id, _sel: Sel, name: Id) -> Id {
    // SAFETY: a guest `NSString`.
    let name = unsafe { foundation::string::bytes(name) };
    let Ok(name) = std::str::from_utf8(&name) else {
        return std::ptr::null_mut();
    };
    // SAFETY: an `RosemaryMTLLibrary`.
    let library = unsafe { object::body::<Library>(receiver) };
    let Some(definition) = function_definition(library, name) else {
        report_rejected(library, name);
        return std::ptr::null_mut();
    };
    object::make(
        c"RosemaryMTLFunction",
        Function {
            library: objc::arc::retain(receiver),
            name: definition.name,
            stage: definition.stage,
            base_entry: definition.entry,
            constants: definition.constants,
            specialized: None,
        },
    )
}

/// `-[MTLLibrary newFunctionWithName:constantValues:error:]`.
///
/// The library is translated again with the values, through the shader cache,
/// which keys an entry by them; specialization comes before the IR passes, so
/// the code a constant switches off is folded away before emission.
pub(super) unsafe extern "C" fn new_function_with_constants(
    receiver: Id,
    _sel: Sel,
    name: Id,
    constants: Id,
    error: *mut Id,
) -> Id {
    // SAFETY: a guest `NSString`, and a constant values object or nil.
    let (name, values) = unsafe { constant_inputs(name, constants) };
    // SAFETY: the caller's error slot.
    unsafe { function_with_constants(receiver, &name, values, error) }
}

/// What `newFunctionWithName:constantValues:` reads of its arguments.
///
/// # Safety
///
/// `name` must be null or an `NSString`, `constants` null or an object.
pub(super) unsafe fn constant_inputs(
    name: Id,
    constants: Id,
) -> (Vec<u8>, metal_translate::FunctionConstantValues) {
    // SAFETY: the caller's contract.
    let name = unsafe { foundation::string::bytes(name) };
    let values = if object::is_kind(constants, c"MTLFunctionConstantValues") {
        // SAFETY: checked above.
        unsafe { object::body::<ConstantValues>(constants) }
            .values
            .clone()
    } else {
        metal_translate::FunctionConstantValues::new()
    };
    (name, values)
}

/// `newFunctionWithName:constantValues:error:` over the inputs it read.
///
/// # Safety
///
/// `receiver` must be an `RosemaryMTLLibrary`, and `error` null or writable.
pub(super) unsafe fn function_with_constants(
    receiver: Id,
    name: &[u8],
    values: metal_translate::FunctionConstantValues,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        // SAFETY: the caller's contract.
        unsafe { *error = std::ptr::null_mut() };
    }
    let Ok(name) = std::str::from_utf8(name) else {
        return fail_library(
            error,
            "newFunctionWithName: names a function in invalid UTF-8",
        );
    };
    // SAFETY: an `RosemaryMTLLibrary`.
    let library = unsafe { object::body::<Library>(receiver) };
    let Some(definition) = function_definition(library, name) else {
        return fail_library(error, &missing_function(&library.translated, name));
    };
    // A library which declares no function constants has nothing to
    // specialize: Metal ignores the values, and so does this.
    let specialized = match (
        library.translated.function_constants.is_empty(),
        &library.input,
        library.target,
    ) {
        (true, _, _) => None,
        (false, Some(input), Some(target)) => {
            let translated = match input {
                LibraryInput::Air {
                    modules,
                    entry_modules,
                } => {
                    let Some(air) = definition
                        .entry
                        .and_then(|entry| entry_modules.get(entry))
                        .and_then(|module| modules.get(*module))
                    else {
                        return fail_library(error, "the function has no owning AIR module");
                    };
                    metal_translate::translate_specialized_cached(
                        metal_translate::AirInput::Bitcode(air),
                        target,
                        metal_translate::PassPipeline::from_env(),
                        &values,
                    )
                }
                LibraryInput::Msl(source) => source.specialize_cached(
                    target,
                    metal_translate::PassPipeline::from_env(),
                    &values,
                ),
            };
            match translated {
                Ok(mut translated) => {
                    let Some(position) = translated
                        .entries
                        .iter()
                        .position(|entry| entry.name == name)
                    else {
                        return fail_library(error, &missing_function(&translated, name));
                    };
                    let entry = translated.entries.swap_remove(position);
                    if let Some(requirement) = target.unsupported_requirement(&entry.required) {
                        return fail_library(
                            error,
                            &format!(
                                "entry point {name} requires {requirement}, which the Vulkan device did not enable"
                            ),
                        );
                    }
                    Some(entry)
                }
                Err(translation) => return fail_library(error, &translation.to_string()),
            }
        }
        _ => None,
    };
    let base_entry = definition.entry;
    let function = object::make(
        c"RosemaryMTLFunction",
        Function {
            library: objc::arc::retain(receiver),
            name: definition.name,
            stage: definition.stage,
            base_entry,
            constants: Vec::new(),
            specialized,
        },
    );
    if crate::profile::enabled() && !function.is_null() {
        // SAFETY: the function just made, whose library is alive.
        let function = unsafe { object::body::<Function>(function) };
        if let (Some(specialized), Some(base)) = (
            &function.specialized,
            base_entry.and_then(|entry| library.translated.entries.get(entry)),
        ) {
            crate::profile::label_specialized(specialized, base);
        }
    }
    function
}

/// Why `translated` has no entry point `name`.
fn missing_function(translated: &metal_translate::TranslatedLibrary, name: &str) -> String {
    translated
        .rejected
        .iter()
        .find(|rejected| rejected.name == name)
        .map_or_else(
            || format!("the library has no function named {name}"),
            |rejected| {
                format!(
                    "function {name} could not be translated: {}",
                    rejected.reason
                )
            },
        )
}

/// `-[MTLLibrary functionNames]`: the entry points in declaration order.
pub(super) unsafe extern "C" fn library_function_names(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `RosemaryMTLLibrary`.
    let library = unsafe { object::body::<Library>(receiver) };
    let names: Vec<CFTypeRef> = match &library.input {
        Some(LibraryInput::Msl(_)) => library
            .translated
            .function_reflection
            .iter()
            .map(|function| foundation::string::from_str(&function.name) as CFTypeRef)
            .collect(),
        _ => library
            .translated
            .entries
            .iter()
            .map(|entry| foundation::string::from_str(&entry.name) as CFTypeRef)
            .collect(),
    };
    // SAFETY: a fresh array over freshly created strings.
    unsafe {
        corefoundation::api::CFAutorelease(corefoundation::collections::new_array(
            &names,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

pub(super) unsafe extern "C" fn library_device(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `RosemaryMTLLibrary`.
    unsafe { object::body::<Library>(receiver) }.device
}

pub(super) unsafe extern "C" fn function_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `RosemaryMTLFunction`.
    let function = unsafe { object::body::<Function>(receiver) };
    foundation::string::from_str(&function.name)
}

/// `-[MTLFunction functionType]`: `MTLFunctionTypeVertex`, `Fragment` or
/// `Kernel`.
pub(super) unsafe extern "C" fn function_type(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: an `RosemaryMTLFunction`.
    let function = unsafe { object::body::<Function>(receiver) };
    match function.stage {
        metal_translate::ir::Stage::Vertex => 1,
        metal_translate::ir::Stage::Fragment => 2,
        _ => 3,
    }
}

/// `-[MTLFunction vertexAttributes]`: one `MTLVertexAttribute` per stage
/// input of a vertex function, from the translation's reflection.
pub(super) unsafe extern "C" fn function_vertex_attributes(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `RosemaryMTLFunction`.
    let function = unsafe { object::body::<Function>(receiver) };
    // SAFETY: the function's library is alive while the function holds it.
    let inputs = match unsafe { function.artifact() } {
        Some(artifact) if artifact.stage == metal_translate::ir::Stage::Vertex => {
            artifact.vertex_inputs.clone()
        }
        _ => Vec::new(),
    };
    let attributes: Vec<corefoundation::base::CFTypeRef> = inputs
        .iter()
        .map(|input| {
            object::make(
                c"RosemaryMTLVertexAttribute",
                VertexAttributeReflection {
                    name: foundation::string::from_str(&input.name),
                    index: u64::from(input.location),
                    data_type: input.data_type,
                },
            ) as corefoundation::base::CFTypeRef
        })
        .collect();
    let array = corefoundation::collections::new_array(
        &attributes,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for attribute in attributes {
        objc::arc::release(attribute as Id);
    }
    objc::arc::autorelease(array as Id)
}

pub(super) unsafe extern "C" fn function_device(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `RosemaryMTLFunction`.
    let function = unsafe { object::body::<Function>(receiver) };
    // SAFETY: the library which vended the function.
    unsafe { object::body::<Library>(function.library) }.device
}

/// `MTLVertexAttribute`: one stage input a vertex function reads.
pub(super) struct VertexAttributeReflection {
    name: Id,
    index: u64,
    data_type: u64,
}

pub(super) unsafe extern "C" fn attribute_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a vertex attribute of this crate's.
    unsafe { object::body::<VertexAttributeReflection>(receiver) }.name
}

pub(super) unsafe extern "C" fn attribute_index(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a vertex attribute of this crate's.
    unsafe { object::body::<VertexAttributeReflection>(receiver) }.index
}

pub(super) unsafe extern "C" fn attribute_type(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a vertex attribute of this crate's.
    unsafe { object::body::<VertexAttributeReflection>(receiver) }.data_type
}

pub(super) unsafe extern "C" fn attribute_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a vertex attribute of this crate's.
    objc::arc::release(unsafe { object::body::<VertexAttributeReflection>(receiver) }.name);
    objc::arc::finalize(receiver);
}

pub(super) fn fail_library(error: *mut Id, message: &str) -> Id {
    report_library_error(message);
    if !error.is_null() {
        // SAFETY: the guest passed a writable `NSError **`.
        unsafe {
            *error = foundation::error::make("MTLLibraryErrorDomain", 3, message);
        }
    }
    std::ptr::null_mut()
}

pub(super) fn report_library_error(message: impl std::fmt::Display) {
    if std::env::var_os("ROSEMARY_TRACE_METAL").is_none() {
        return;
    }
    let message = message.to_string();
    let mut seen = REPORTED_LIBRARY_ERRORS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if seen.insert(message.clone()) {
        eprintln!("rosemary: Metal: shader library translation stopped: {message}");
    }
}

/// Report just enough information to identify the first guest source library
/// without placing proprietary game shaders in a log file.
pub(super) fn report_source_library(source: &str) {
    if std::env::var_os("ROSEMARY_TRACE_METAL").is_some()
        && !REPORTED_SOURCE_LIBRARY.swap(true, Ordering::Relaxed)
    {
        eprintln!(
            "rosemary: Metal: source library: {} bytes, fnv1a64={:016x}",
            source.len(),
            fnv1a64(source.as_bytes()),
        );
    }
}

pub(super) fn dump_source_library(source: &str) {
    let Some(directory) = std::env::var_os("ROSEMARY_MSL_OUT") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    let name = format!("msl-{:016x}.metal", fnv1a64(source.as_bytes()));
    let _ = std::fs::write(directory.join(name), source);
}

pub(super) fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// `-[MTLLibrary dealloc]`.
///
/// The translated modules and the AIR kept for function constants are Rust
/// allocations of this body's; a function made from the library retains it,
/// so a library only reaches here once every function it vended has gone.
pub(super) unsafe extern "C" fn library_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a library of this crate's.
    let library = unsafe { object::body::<Library>(receiver) };
    // SAFETY: the body was established by `object::make` and is not read again.
    unsafe { std::ptr::drop_in_place(library as *mut Library) };
    objc::arc::finalize(receiver);
}

/// `-[MTLFunction dealloc]`.
pub(super) unsafe extern "C" fn function_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a function of this crate's.
    let function = unsafe { object::body::<Function>(receiver) };
    objc::arc::release(std::mem::replace(
        &mut function.library,
        std::ptr::null_mut(),
    ));
    // SAFETY: as above. The specialized artifact is this body's own.
    unsafe { std::ptr::drop_in_place(function as *mut Function) };
    objc::arc::finalize(receiver);
}

/// `MTLFunctionConstant`: one constant a function can be specialized with.
pub(super) struct FunctionConstant {
    name: Id,
    data_type: u64,
    index: u64,
}

/// `-[MTLFunction functionConstantsDictionary]`: name to `MTLFunctionConstant`.
pub(super) unsafe extern "C" fn function_constants_dictionary(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `RosemaryMTLFunction`.
    let function = unsafe { object::body::<Function>(receiver) };
    let mut pairs = Vec::new();
    for constant in &function.constants {
        let Some(data_type) = metal_translate::metal_data_type_of_encoding(&constant.encoding)
        else {
            report_library_error(format!(
                "function constant {} has a type, {}, MTLDataType has no value for",
                constant.name, constant.encoding
            ));
            continue;
        };
        let name = foundation::string::from_str(&constant.name);
        let object = object::make(
            c"RosemaryMTLFunctionConstant",
            FunctionConstant {
                name: objc::arc::retain(name),
                data_type,
                index: u64::from(constant.index),
            },
        );
        pairs.push((name as CFTypeRef, object as CFTypeRef));
    }
    let policy = corefoundation::callbacks::Policy::from_type_callbacks();
    let dictionary = corefoundation::collections::new_dictionary(&pairs, policy, policy, false);
    for (name, object) in pairs {
        // The dictionary holds its own references.
        objc::arc::release(name as Id);
        objc::arc::release(object as Id);
    }
    // SAFETY: a fresh dictionary, handed back autoreleased as a getter's is.
    unsafe { corefoundation::api::CFAutorelease(dictionary) as Id }
}

unsafe extern "C" fn function_constant_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `MTLFunctionConstant` of this crate's.
    unsafe { object::body::<FunctionConstant>(receiver) }.name
}

unsafe extern "C" fn function_constant_type(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: as above.
    unsafe { object::body::<FunctionConstant>(receiver) }.data_type
}

unsafe extern "C" fn function_constant_index(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: as above.
    unsafe { object::body::<FunctionConstant>(receiver) }.index
}

unsafe extern "C" fn function_constant_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: as above; the name was retained when the constant was made.
    objc::arc::release(unsafe { object::body::<FunctionConstant>(receiver) }.name);
    objc::arc::finalize(receiver);
}

pub(super) fn install_function_constants() {
    let class = object::register::<FunctionConstant>(c"RosemaryMTLFunctionConstant");
    methods!(class, {
        "name" => function_constant_name, c"@16@0:8";
        "type" => function_constant_type, c"Q16@0:8";
        "index" => function_constant_index, c"Q16@0:8";
        "dealloc" => function_constant_dealloc, c"v16@0:8";
    });
}

/// Say why a function the library declares has no translation, if it does.
fn report_rejected(library: &Library, name: &str) {
    if let Some(rejected) = library
        .translated
        .rejected
        .iter()
        .find(|rejected| rejected.name == name)
    {
        report_library_error(format!(
            "function {name} could not be translated: {}",
            rejected.reason
        ));
    }
}
