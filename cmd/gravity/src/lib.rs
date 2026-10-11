pub mod codegen;
pub mod go;

use std::collections::{BTreeMap, BTreeSet};

use crate::go::GoType;
use wit_bindgen_core::{
    abi::WasmType,
    dealias,
    wit_parser::{
        Case, Function, FunctionKind, Handle, Resolve, Result_, Type, TypeDef, TypeDefKind, TypeId,
        TypeOwner, World, WorldItem,
    },
};

// Temporary re-export while we migrate.
pub use codegen::Func;

/// How a single variant case is represented in Go.
pub enum CaseDispatchKind {
    /// The case payload's named record IS the dispatch type — the record
    /// implements the variant's marker interface directly. Constructed as
    /// `MyRecord{...}`.
    DirectRecord,
    /// A dedicated `{variant_name}-{case_name}` wrapper struct holds the
    /// optional payload in a `Value` field. Constructed as
    /// `Wrapper{Value: payload}` or `Wrapper{}` for unit cases.
    Wrapped,
}

/// Detect the WIT shorthand `case-name(case-name)` where the payload is a
/// named record sharing the case's name — the historical arcjet shape
/// (`allow-email-validation-config(allow-email-validation-config)`). We let
/// the record implement the marker interface directly so existing call
/// sites that construct the record value as the variant keep working.
pub fn case_dispatch_kind(case: &Case, resolve: &Resolve) -> CaseDispatchKind {
    if let Some(Type::Id(payload_id)) = &case.ty {
        let payload_def = &resolve.types[*payload_id];
        if matches!(payload_def.kind, TypeDefKind::Record(_))
            && payload_def.name.as_deref() == Some(case.name.as_str())
        {
            return CaseDispatchKind::DirectRecord;
        }
    }
    CaseDispatchKind::Wrapped
}

/// Kebab-case Go name for the type a variant case dispatches against in a
/// type-switch.
pub fn case_dispatch_name(variant_name: &str, case: &Case, resolve: &Resolve) -> String {
    case_dispatch_name_in(variant_name, case, resolve, &ExportedResources::new())
}

/// Like [`case_dispatch_name`], naming the payload record as `exported` sees
/// it.
pub fn case_dispatch_name_in(
    variant_name: &str,
    case: &Case,
    resolve: &Resolve,
    exported: &ExportedResources,
) -> String {
    match case_dispatch_kind(case, resolve) {
        CaseDispatchKind::DirectRecord => match case.ty {
            Some(Type::Id(payload_id)) => exported.type_name(payload_id, resolve),
            _ => unreachable!("DirectRecord requires a Type::Id payload"),
        },
        CaseDispatchKind::Wrapped => format!("{variant_name}-{}", case.name),
    }
}

/// Returns a globally-unique kebab-case name suitable for deriving a Go
/// identifier from a WIT type. WIT lets two interfaces declare types of
/// the same name (e.g. both `email-validator-overrides` and `verify-bot`
/// declare an `enum validator-response`); we qualify only the colliding
/// names with their owning interface so stable single-instance names like
/// `algorithm-result` stay flat. The result is fed to
/// `GoIdentifier::public`, so it must remain in kebab-case.
pub fn qualified_type_name(type_id: TypeId, resolve: &Resolve) -> String {
    let canonical = dealias(resolve, type_id);
    let type_def = &resolve.types[canonical];
    let name = type_def
        .name
        .as_ref()
        .expect("expected named type for qualified_type_name");

    // Skip `Type` aliases when looking for collisions: they re-export an
    // existing type rather than introducing a new one.
    let collides = resolve.types.iter().any(|(other_id, other_def)| {
        other_id != canonical
            && other_def.name.as_deref() == Some(name.as_str())
            && !matches!(other_def.kind, TypeDefKind::Type(_))
    });

    qualify_type_name(type_def, resolve, collides)
}

fn qualify_type_name(type_def: &TypeDef, resolve: &Resolve, collides: bool) -> String {
    let name = type_def.name.as_ref().expect("expected named type");
    if !collides {
        return name.clone();
    }

    match type_def.owner {
        TypeOwner::Interface(id) => {
            let interface_name = resolve.interfaces[id]
                .name
                .as_ref()
                .expect("interface missing name");
            format!("{interface_name}-{name}")
        }
        TypeOwner::World(_) | TypeOwner::None => name.clone(),
    }
}

/// The types that look different from the guest's side of the world.
///
/// wit-parser gives a resource one `TypeId` even when its interface is both
/// imported and exported, so the type alone does not say who implements it.
/// This is the answer for one function: functions of an exported interface
/// use [`exported_resources`], every other function uses an empty value,
/// because a world-level function and an import can only name the imported
/// side of an interface.
#[derive(Debug, Clone, Default)]
pub struct ExportedResources {
    /// The resources the guest implements, with the kebab-case name of the Go
    /// handle type gravity generates for each.
    pub resources: BTreeMap<TypeId, String>,
    /// The records and variants that hold a handle to one of `resources` and
    /// whose interface is also imported, with the kebab-case name of their
    /// guest-side Go type. Such a type exists twice in Go: the imported side
    /// holds the host's Go value, the exported side the guest's handle.
    pub types: BTreeMap<TypeId, String>,
}

impl ExportedResources {
    pub fn new() -> Self {
        Self::default()
    }

    /// Kebab-case Go name of the named type `id` on this side of the world.
    pub fn type_name(&self, id: TypeId, resolve: &Resolve) -> String {
        match self.types.get(&dealias(resolve, id)) {
            Some(name) => name.clone(),
            None => qualified_type_name(id, resolve),
        }
    }
}

/// Whether `typ` holds, at any depth, a handle to one of `resources`.
fn holds_handle_to(
    typ: &Type,
    resolve: &Resolve,
    resources: &BTreeMap<TypeId, String>,
    memo: &mut BTreeMap<TypeId, bool>,
) -> bool {
    let Type::Id(id) = typ else {
        return false;
    };
    if let Some(&holds) = memo.get(id) {
        return holds;
    }
    let mut holds = |typ: &Type| holds_handle_to(typ, resolve, resources, memo);
    let result = match &resolve.types[*id].kind {
        TypeDefKind::Handle(handle) => resources.contains_key(&handle_resource(handle, resolve).0),
        TypeDefKind::Record(record) => record.fields.iter().any(|field| holds(&field.ty)),
        TypeDefKind::Variant(variant) => variant
            .cases
            .iter()
            .any(|case| case.ty.iter().any(&mut holds)),
        TypeDefKind::Tuple(tuple) => tuple.types.iter().any(&mut holds),
        TypeDefKind::Result(result) => result.ok.iter().chain(result.err.iter()).any(&mut holds),
        TypeDefKind::Option(inner)
        | TypeDefKind::List(inner)
        | TypeDefKind::FixedLengthList(inner, _)
        | TypeDefKind::Type(inner) => holds(inner),
        TypeDefKind::Map(key, value) => holds(key) || holds(value),
        TypeDefKind::Future(inner) | TypeDefKind::Stream(inner) => inner.iter().any(&mut holds),
        TypeDefKind::Resource
        | TypeDefKind::Flags(_)
        | TypeDefKind::Enum(_)
        | TypeDefKind::Unknown => false,
    };
    memo.insert(*id, result);
    result
}

/// The resources declared in the interfaces `world` exports.
///
/// A resource whose interface is also imported exists twice, once implemented
/// by the host and once by the guest. The guest's side gets an `exported-`
/// prefix so the two Go types do not collide.
pub fn exported_resources(resolve: &Resolve, world: &World) -> ExportedResources {
    let mut exported = ExportedResources::new();
    let imported_interfaces: BTreeSet<_> = world
        .imports
        .values()
        .filter_map(|item| match item {
            WorldItem::Interface { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    let is_imported = |owner: TypeOwner| match owner {
        TypeOwner::Interface(id) => imported_interfaces.contains(&id),
        _ => false,
    };
    let mut memo = BTreeMap::new();
    let mut name_counts = BTreeMap::new();
    for (_, typ) in resolve.types.iter() {
        if !matches!(typ.kind, TypeDefKind::Type(_))
            && let Some(name) = &typ.name
        {
            *name_counts.entry(name.as_str()).or_insert(0usize) += 1;
        }
    }
    let type_name = |id| {
        let typ = &resolve.types[id];
        let name = typ.name.as_ref().expect("expected named type");
        qualify_type_name(
            typ,
            resolve,
            name_counts.get(name.as_str()).copied().unwrap_or(0) > 1,
        )
    };
    for item in world.exports.values() {
        let WorldItem::Interface { id, .. } = item else {
            continue;
        };
        for &type_id in resolve.interfaces[*id].types.values() {
            let type_def = &resolve.types[type_id];
            if !matches!(type_def.kind, TypeDefKind::Resource) {
                continue;
            }
            let name = type_name(type_id);
            let name = if is_imported(type_def.owner) {
                format!("exported-{name}")
            } else {
                name
            };
            exported.resources.insert(type_id, name);
        }
    }

    // A record or variant that holds one of those handles needs a second Go
    // type when the import side already generated one under the plain name.
    for item in world.exports.values() {
        let WorldItem::Interface { id, .. } = item else {
            continue;
        };
        for &type_id in resolve.interfaces[*id].types.values() {
            let type_def = &resolve.types[type_id];
            if matches!(
                type_def.kind,
                TypeDefKind::Record(_) | TypeDefKind::Variant(_)
            ) && is_imported(type_def.owner)
                && holds_handle_to(&Type::Id(type_id), resolve, &exported.resources, &mut memo)
            {
                let name = type_name(type_id);
                exported.types.insert(type_id, format!("exported-{name}"));
            }
        }
    }
    exported
}

/// The resource a handle refers to, with aliases such as `use iface.{res}`
/// followed to the resource's declaration, and whether the handle owns it.
pub fn handle_resource(handle: &Handle, resolve: &Resolve) -> (TypeId, bool) {
    match handle {
        Handle::Own(id) => (dealias(resolve, *id), true),
        Handle::Borrow(id) => (dealias(resolve, *id), false),
    }
}

/// Kebab-case name of the handle table field for `resource` in the generated
/// per-instance table struct. The prefix keeps the two sides of a resource
/// that is both imported and exported apart, and keeps a resource named for a
/// Go keyword (`type`, `map`) a valid identifier.
pub fn resource_table_field(resource: TypeId, resolve: &Resolve, exported: bool) -> String {
    let name = qualified_type_name(resource, resolve);
    if exported {
        format!("exported-{name}")
    } else {
        format!("imported-{name}")
    }
}

/// Kebab-case Go name for a WIT function.
///
/// A resource's functions carry their kind in the WIT name
/// (`[method]counter.get`, `[static]counter.merge`, `[constructor]counter`).
/// A method becomes a Go method on the resource, so it keeps only its own name
/// (`get`). A static function and a constructor become Go methods beside the
/// interface's free functions, so they keep the resource name
/// (`counter-merge`, `new-counter`).
pub fn function_go_name(func: &Function, resolve: &Resolve) -> String {
    let item = || {
        func.name
            .split_once('.')
            .map(|(_, item)| item)
            .unwrap_or(&func.name)
    };
    let resource = |id: &TypeId| {
        resolve.types[*id]
            .name
            .clone()
            .expect("resource should have a name")
    };
    match &func.kind {
        FunctionKind::Freestanding => func.name.clone(),
        FunctionKind::Method(_) => item().to_string(),
        FunctionKind::Static(id) => format!("{}-{}", resource(id), item()),
        FunctionKind::Constructor(id) => format!("new-{}", resource(id)),
        kind => todo!("TODO(#4): implement async function kind {kind:?}"),
    }
}

/// Resolves a Wasm type to a Go type.
pub fn resolve_wasm_type(typ: &WasmType) -> GoType {
    match typ {
        WasmType::I32 => GoType::Uint32,
        WasmType::I64 => GoType::Uint64,
        WasmType::F32 => GoType::Float32,
        WasmType::F64 => GoType::Float64,
        WasmType::Pointer => GoType::Uint64,
        WasmType::PointerOrI64 => GoType::Uint64,
        WasmType::Length => GoType::Uint64,
    }
}

/// Resolves a WIT type to a Go type.
///
/// # Panics
///
/// This function panics if:
///
/// - The type definition cannot be found in the resolve context.
/// - The type is still unimplemented.
/// - The type does not have a name when it is expected to have one (enums, records, type aliases).
pub fn resolve_type(typ: &Type, resolve: &Resolve) -> GoType {
    resolve_type_in(typ, resolve, &ExportedResources::new())
}

/// Like [`resolve_type`], for a function whose resources in `exported` are
/// implemented by the guest.
///
/// A handle to a resource the host implements is the Go interface generated
/// for that resource. A handle to a resource the guest implements is a pointer
/// to the generated handle struct. `own<T>` and `borrow<T>` share one Go type.
pub fn resolve_type_in(typ: &Type, resolve: &Resolve, exported: &ExportedResources) -> GoType {
    let nested =
        |typ: &Type, position: &str| resolve_nested_type_in(typ, resolve, exported, position);
    match typ {
        // Basic types.
        Type::Bool => GoType::Bool,
        Type::U8 => GoType::Uint8,
        Type::U16 => GoType::Uint16,
        Type::U32 => GoType::Uint32,
        Type::U64 => GoType::Uint64,
        Type::S8 => GoType::Int8,
        Type::S16 => GoType::Int16,
        Type::S32 => GoType::Int32,
        Type::S64 => GoType::Int64,
        Type::F32 => GoType::Float32,
        Type::F64 => GoType::Float64,
        Type::Char => GoType::Rune,
        Type::String => GoType::String,
        Type::ErrorContext => todo!("TODO(#4): implement error context conversion"),

        // Complex types.
        Type::Id(id) => {
            let TypeDef { kind, .. } = resolve
                .types
                .get(*id)
                .expect("failed to find type definition");
            match kind {
                TypeDefKind::Record(_) => GoType::UserDefined(exported.type_name(*id, resolve)),
                TypeDefKind::Resource => {
                    panic!("a resource is only usable through an own or borrow handle")
                }
                TypeDefKind::Handle(handle) => {
                    let (resource, _) = handle_resource(handle, resolve);
                    match exported.resources.get(&resource) {
                        Some(name) => GoType::Pointer(Box::new(GoType::UserDefined(name.clone()))),
                        None => GoType::UserDefined(qualified_type_name(resource, resolve)),
                    }
                }
                TypeDefKind::Flags(_) => todo!("TODO(#4): implement flag conversion"),
                TypeDefKind::Tuple(_) => todo!("TODO(#4): implement tuple conversion"),
                TypeDefKind::Variant(_) => GoType::UserDefined(exported.type_name(*id, resolve)),
                TypeDefKind::Enum(_) => GoType::UserDefined(exported.type_name(*id, resolve)),
                // `option<T>` is `*T`: `nil` is `none`, `&v` is `some`. A
                // single pointer composes in every position (param, return,
                // record field, list element); the prior `(T, bool)`
                // comma-ok shape didn't.
                TypeDefKind::Option(value) => {
                    GoType::Pointer(Box::new(nested(value, "an option payload")))
                }

                // Various results, including specialised ones.
                TypeDefKind::Result(Result_ {
                    ok: Some(ok),
                    err: Some(Type::String),
                }) => GoType::ValueOrError(Box::new(nested(ok, "a result's ok type"))),
                TypeDefKind::Result(Result_ {
                    ok: Some(_),
                    err: Some(_),
                }) => {
                    todo!("TODO(#4): implement remaining result conversion")
                }
                TypeDefKind::Result(Result_ {
                    ok: Some(ok),
                    err: None,
                }) => nested(ok, "a result's ok type"),
                TypeDefKind::Result(Result_ {
                    ok: None,
                    err: Some(Type::String),
                }) => GoType::Error,
                TypeDefKind::Result(Result_ {
                    ok: None,
                    err: Some(_),
                }) => todo!("TODO(#4): implement remaining result conversion"),
                TypeDefKind::Result(Result_ {
                    ok: None,
                    err: None,
                }) => GoType::Nothing,

                TypeDefKind::List(inner) => {
                    GoType::Slice(Box::new(nested(inner, "a list element")))
                }
                TypeDefKind::Future(_) => todo!("TODO(#4): implement future conversion"),
                TypeDefKind::Stream(_) => todo!("TODO(#4): implement stream conversion"),
                TypeDefKind::Type(_) => GoType::UserDefined(exported.type_name(*id, resolve)),
                TypeDefKind::FixedLengthList(_, _) => {
                    todo!("TODO(#4): implement fixed length list conversion")
                }
                TypeDefKind::Map(_, _) => todo!("TODO(#4): implement map conversion"),
                TypeDefKind::Unknown => todo!("TODO(#4): implement unknown conversion"),
            }
        }
    }
}

/// Resolves a WIT type that sits inside another type, such as a record field
/// or a list element. `position` names where it sits, for the error message.
///
/// gravity represents a `result` with Go's multiple return values (`T,
/// error`, or `error` alone), which only a function's results can carry, so a
/// nested `result` is refused here rather than written out as Go that does not
/// compile.
pub fn resolve_nested_type(typ: &Type, resolve: &Resolve, position: &str) -> GoType {
    resolve_nested_type_in(typ, resolve, &ExportedResources::new(), position)
}

/// Like [`resolve_nested_type`], with the resources in `exported` implemented
/// by the guest.
pub fn resolve_nested_type_in(
    typ: &Type,
    resolve: &Resolve,
    exported: &ExportedResources,
    position: &str,
) -> GoType {
    if let Type::Id(id) = typ
        && matches!(
            resolve.types[dealias(resolve, *id)].kind,
            TypeDefKind::Result(_)
        )
    {
        todo!("TODO(#4): implement a nested result in {position}");
    }
    resolve_type_in(typ, resolve, exported)
}

/// Like [`resolve_type`], but downgrades a top-level Variant to
/// `interface{}` so existing call sites can keep passing the variant
/// payload through `any`-typed plumbing (rule config returns, generic
/// dispatch layers). The marker interface and per-case structs are still
/// generated, and the type-switch in `VariantLower` still dispatches on
/// the concrete case types — callers who want compile-time exhaustiveness
/// just declare their value as the marker interface explicitly.
///
/// Variants nested inside records, lists, or returns stay typed so
/// generated record fields remain strongly typed.
pub fn resolve_param_type(typ: &Type, resolve: &Resolve) -> GoType {
    resolve_param_type_in(typ, resolve, &ExportedResources::new())
}

/// Like [`resolve_param_type`], with the resources in `exported` implemented
/// by the guest.
pub fn resolve_param_type_in(
    typ: &Type,
    resolve: &Resolve,
    exported: &ExportedResources,
) -> GoType {
    if let Type::Id(id) = typ {
        let def = &resolve.types[dealias(resolve, *id)];
        if matches!(def.kind, TypeDefKind::Variant(_)) {
            return GoType::Interface;
        }
    }
    resolve_type_in(typ, resolve, exported)
}

#[cfg(test)]
mod resource_analysis_tests {
    use super::*;
    use crate::codegen::test_wit::Fixture;

    #[test]
    fn shared_type_graph_caches_positive_and_negative_results() {
        let fixture = Fixture::parse(
            "package test:fixture;
            interface things {
                resource thing;
                record leaf { value: u32 }
                record branch { a: leaf, b: leaf }
                record diamond { a: branch, b: branch }
                record held { value: thing }
                record nested { a: diamond, b: held }
            }
            world test-world { import things; export things; }",
        );
        let exported = exported_resources(&fixture.resolve, fixture.world());
        let mut memo = BTreeMap::new();
        let diamond = Type::Id(fixture.type_id("diamond"));
        assert!(!holds_handle_to(
            &diamond,
            &fixture.resolve,
            &exported.resources,
            &mut memo
        ));
        let cached = memo.clone();
        assert!(!holds_handle_to(
            &diamond,
            &fixture.resolve,
            &exported.resources,
            &mut memo
        ));
        assert_eq!(memo, cached);
        assert_eq!(memo.len(), 3, "each shared record is visited once");
        let nested = fixture.type_id("nested");
        assert!(holds_handle_to(
            &Type::Id(nested),
            &fixture.resolve,
            &exported.resources,
            &mut memo
        ));
        assert_eq!(exported.types.get(&nested).unwrap(), "exported-nested");
        assert!(!exported.types.contains_key(&fixture.type_id("diamond")));
    }
}
