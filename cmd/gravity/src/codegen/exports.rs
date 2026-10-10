use std::collections::BTreeSet;

use genco::prelude::*;
use wit_bindgen_core::wit_parser::{
    Function, FunctionKind, InterfaceId, Param, Resolve, SizeAlign, World, WorldItem, WorldKey,
};

use crate::{
    ExportedResources,
    codegen::imports::{ImportAnalyzer, ImportCodeGenerator},
    go::{
        GoIdentifier, GoResult, GoType, comment,
        imports::{CONTEXT_CONTEXT, ERRORS_NEW},
    },
};

pub struct ExportConfig<'a> {
    pub instance: &'a GoIdentifier,
    pub world: &'a World,
    pub resolve: &'a Resolve,
    pub sizes: &'a SizeAlign,
}

pub struct ExportGenerator<'a> {
    config: ExportConfig<'a>,
}

/// What a generated Go function is a method of.
enum Receiver<'a> {
    /// The instance type: world-level functions, and an exported interface's
    /// free functions, constructors and static functions.
    Instance,
    /// The Go handle type of a resource the guest implements: its methods.
    Resource(&'a GoIdentifier),
}

impl<'a> ExportGenerator<'a> {
    pub fn new(config: ExportConfig<'a>) -> Self {
        Self { config }
    }

    /// Generate the Go function code for the given world-level function.
    fn generate_function(&self, func: &Function, tokens: &mut Tokens<Go>) {
        self.generate_export(
            func,
            &func.name,
            &GoIdentifier::public(&func.name),
            Receiver::Instance,
            &ExportedResources::new(),
            tokens,
        )
    }

    /// Generate the Go function code for the given function.
    ///
    /// `export_name` is the name the guest exports the function under,
    /// `go_name` the name of the Go method and `exported` the resources the
    /// function sees as implemented by the guest.
    ///
    /// The signature is obtained by:
    /// - getting the function parameters from the `wit_parser::Function`, converting
    ///   names to to Go identifiers and types to Go types.
    /// - similar for the result
    ///
    /// To implement the body, we:
    /// - creating a `Func` struct which implements `Bindgen` and passing it to the
    ///   `wit_bindgen_core::abi::call` function. This will call `Func::emit` lots of
    ///   times, one for each instruction in the function, and `Func::emit` will generate
    ///   Go code for each instruction
    fn generate_export(
        &self,
        func: &Function,
        export_name: &str,
        go_name: &GoIdentifier,
        receiver: Receiver<'_>,
        exported: &ExportedResources,
        tokens: &mut Tokens<Go>,
    ) {
        let params = func
            .params
            .iter()
            .map(|Param { name, ty, .. }| {
                match crate::resolve_param_type_in(ty, self.config.resolve, exported) {
                    GoType::ValueOrOk(t) => (GoIdentifier::local(name), *t),
                    t => (GoIdentifier::local(name), t),
                }
            })
            .collect::<Vec<_>>();

        let result = if let Some(wit_type) = &func.result {
            GoResult::Anon(crate::resolve_type_in(
                wit_type,
                self.config.resolve,
                exported,
            ))
        } else {
            GoResult::Empty
        };

        let mut f = crate::Func::export(result, self.config.sizes)
            .with_exported_resources(exported.clone())
            .with_export_name(export_name.to_string());
        wit_bindgen_core::abi::call(
            self.config.resolve,
            wit_bindgen_core::abi::AbiVariant::GuestExport,
            wit_bindgen_core::abi::LiftLower::LowerArgsLiftResults,
            func,
            &mut f,
            // async is not currently supported
            false,
        );

        let arg_assignments = f
            .args()
            .iter()
            .zip(&params)
            .map(|(arg, (param, _))| (arg, param))
            .collect::<Vec<_>>();
        // A resource method's `self` is the receiver, so it is not repeated
        // in the parameter list. The body still reads it as its first
        // argument, and reaches the module through `i` like every export.
        let (receiver, listed_params, prelude) = match receiver {
            Receiver::Instance => (
                quote!(i *$(self.config.instance)),
                &params[..],
                Tokens::<Go>::new(),
            ),
            Receiver::Resource(resource) => (
                quote!(self *$resource),
                &params[1..],
                quote!(i := self.instance),
            ),
        };
        quote_in! { *tokens =>
            $['\n']
            func ($receiver) $go_name(
                $['\r']
                ctx $CONTEXT_CONTEXT,
                $(for (name, typ) in listed_params join ($['\r']) => $name $typ,)
            ) $(f.result()) {
                $prelude
                $(for (arg, param) in arg_assignments join ($['\r']) => $arg := $param)
                $(f.body())
            }
        }
    }

    /// Generates the Go handle type for a resource the guest implements.
    ///
    /// The guest names each value with a `u32` of its choice, the
    /// representation. The Go handle owns one value: it holds the
    /// representation and the instance the value lives in.
    fn generate_resource_type(
        &self,
        name: &str,
        go_name: &GoIdentifier,
        interface_name: &str,
        tokens: &mut Tokens<Go>,
    ) {
        let instance = self.config.instance;
        let go_name_str = String::from(go_name);
        let dtor = format!("{interface_name}#[dtor]{name}");
        let dropped = format!("{name} handle used after it was dropped or given away");
        let foreign = format!("{name} handle belongs to another instance");
        quote_in! { *tokens =>
            $['\n']
            $(comment(&[
                format!("{go_name_str} owns a `{name}` resource, which the guest implements."),
                "Call Drop when the value is no longer needed, or the guest keeps it until the".to_string(),
                "instance is closed. A handle belongs to the instance that returned it.".to_string(),
            ]))
            type $go_name struct {
                instance *$instance
                rep uint32
                dropped bool
            }
            $['\n']
            $(comment(&[
                "Drop runs the guest's destructor for the resource. The handle must not be",
                "used afterwards. Dropping a handle twice does nothing.",
            ]))
            func (self *$go_name) Drop(ctx $CONTEXT_CONTEXT) error {
                if self.dropped {
                    return nil
                }
                self.dropped = true
                if dtor := self.instance.module.ExportedFunction($(quoted(dtor))); dtor != nil {
                    if _, err := dtor.Call(ctx, uint64(self.rep)); err != nil {
                        return err
                    }
                }
                return nil
            }
            $['\n']
            $(comment(&[
                "lend returns the representation for a call that borrows the resource.",
            ]))
            func (self *$go_name) lend(i *$instance) uint32 {
                if self == nil || self.dropped {
                    panic($ERRORS_NEW($(quoted(&dropped))))
                }
                if self.instance != i {
                    panic($ERRORS_NEW($(quoted(&foreign))))
                }
                return self.rep
            }
            $['\n']
            $(comment(&[
                "give returns the representation for a call that takes ownership of the",
                "resource, and spends the handle.",
            ]))
            func (self *$go_name) give(i *$instance) uint32 {
                rep := self.lend(i)
                self.dropped = true
                return rep
            }
        }
    }

    /// Generates everything an exported interface declares: its data types,
    /// a Go handle type per resource, and a Go method per function.
    fn generate_interface(
        &self,
        key: &WorldKey,
        id: InterfaceId,
        exported: &ExportedResources,
        collisions: &BTreeSet<String>,
        tokens: &mut Tokens<Go>,
    ) {
        let resolve = self.config.resolve;
        let interface = &resolve.interfaces[id];
        let interface_name = resolve.name_world_key(key);
        let imported = self.config.world.imports.values().any(
            |item| matches!(item, WorldItem::Interface { id: imported, .. } if *imported == id),
        );

        // The import side already generated the data types of an interface
        // that is also imported.
        if !imported {
            let analyzer = ImportAnalyzer::new(resolve, self.config.world);
            for &type_id in interface.types.values() {
                if let Some(typ) = analyzer.analyze_type(type_id) {
                    ImportCodeGenerator::generate_type_definition(&typ, tokens);
                }
            }
        }

        for (name, &type_id) in &interface.types {
            if let Some(go_name) = exported.get(&type_id) {
                self.generate_resource_type(
                    name,
                    &GoIdentifier::public(go_name),
                    &interface_name,
                    tokens,
                );
            }
        }

        for func in interface.functions.values() {
            let export_name = format!("{interface_name}#{}", func.name);
            let go_name = Self::go_function_name(func, resolve, exported);
            match func.kind {
                FunctionKind::Method(resource) => {
                    let resource = GoIdentifier::public(&exported[&resource]);
                    self.generate_export(
                        func,
                        &export_name,
                        &GoIdentifier::public(go_name),
                        Receiver::Resource(&resource),
                        exported,
                        tokens,
                    );
                }
                _ => {
                    let go_name = if collisions.contains(&go_name) {
                        let interface = interface.name.as_deref().unwrap_or(&interface_name);
                        format!("{interface}-{go_name}")
                    } else {
                        go_name
                    };
                    self.generate_export(
                        func,
                        &export_name,
                        &GoIdentifier::public(go_name),
                        Receiver::Instance,
                        exported,
                        tokens,
                    );
                }
            }
        }
    }

    /// Kebab-case Go name for a function of an exported interface.
    ///
    /// A constructor and a static function are named for the resource's Go
    /// handle type (`new-accumulator`, `accumulator-combine`), which is
    /// already unique in the package, where the bare resource name is not when
    /// two interfaces declare the same one.
    fn go_function_name(
        func: &Function,
        resolve: &Resolve,
        exported: &ExportedResources,
    ) -> String {
        let item = || func.name.split_once('.').map_or("", |(_, item)| item);
        match func.kind {
            FunctionKind::Constructor(id) => format!("new-{}", exported[&id]),
            FunctionKind::Static(id) => format!("{}-{}", exported[&id], item()),
            _ => crate::function_go_name(func, resolve),
        }
    }

    /// The Go names that more than one of the world's exports would give a
    /// method of the instance type. An exported interface's functions share
    /// the instance type with the world's own functions and with every other
    /// exported interface, so the colliding ones are qualified with their
    /// interface name.
    fn instance_method_collisions(&self, exported: &ExportedResources) -> BTreeSet<String> {
        let resolve = self.config.resolve;
        let mut seen = BTreeSet::new();
        let mut collisions = BTreeSet::new();
        let mut record = |name: String| {
            if !seen.insert(name.clone()) {
                collisions.insert(name);
            }
        };
        for item in self.config.world.exports.values() {
            match item {
                WorldItem::Function(func) => record(func.name.clone()),
                WorldItem::Interface { id, .. } => {
                    for func in resolve.interfaces[*id].functions.values() {
                        if !matches!(func.kind, FunctionKind::Method(_)) {
                            record(Self::go_function_name(func, resolve, exported));
                        }
                    }
                }
                WorldItem::Type { .. } => (),
            }
        }
        collisions
    }
}

impl FormatInto<Go> for ExportGenerator<'_> {
    fn format_into(self, tokens: &mut Tokens<Go>) {
        let exported = crate::exported_resources(self.config.resolve, self.config.world);
        let collisions = self.instance_method_collisions(&exported);
        for (key, item) in self.config.world.exports.iter() {
            match item {
                WorldItem::Function(func) => self.generate_function(func, tokens),
                WorldItem::Interface { id, .. } => {
                    self.generate_interface(key, *id, &exported, &collisions, tokens)
                }
                WorldItem::Type { .. } => todo!("generate type exports"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use genco::prelude::*;

    use crate::{codegen::test_wit::Fixture, go::GoIdentifier};

    use super::{ExportConfig, ExportGenerator};

    /// Generates the Go method for the export `name` in `fixture`'s world.
    fn generate(fixture: &Fixture, name: &str) -> String {
        let instance = GoIdentifier::public("TestInstance");
        let config = ExportConfig {
            instance: &instance,
            world: fixture.world(),
            resolve: &fixture.resolve,
            sizes: &fixture.sizes,
        };
        let generator = ExportGenerator::new(config);
        let mut tokens = Tokens::new();
        generator.generate_function(fixture.export(name), &mut tokens);
        tokens.to_string().unwrap()
    }

    #[test]
    fn test_generate_function_simple_u32_param() {
        let fixture = Fixture::parse(
            "package test:fixture;
            world test-world {
                export add-number: func(value: u32) -> u32;
            }",
        );
        let generated = generate(&fixture, "add-number");
        println!("Generated: {}", generated);

        // Verify basic function structure
        assert!(generated.contains("func (i *TestInstance) AddNumber("));
        assert!(generated.contains("value uint32"));
        assert!(generated.contains("ctx context.Context"));
        assert!(generated.contains(") uint32 {"));

        // Verify function body
        assert!(generated.contains("arg0 := value"));
        assert!(
            generated
                .contains("i.module.ExportedFunction(\"add-number\").Call(ctx, uint64(result0))")
        );
        assert!(generated.contains("if err1 != nil {"));
        assert!(generated.contains("panic(err1)"));
        assert!(generated.contains("results1 := raw1[0]"));
        assert!(generated.contains("result2 := uint32(results1)"));
        assert!(generated.contains("return result2"));

        // I32FromU32 / U32FromI32 are no-op reinterpretations — they must not
        // use api.EncodeU32 or api.DecodeU32 (which round-trip through uint64,
        // causing type mismatches in VariantLower and needless widening elsewhere).
        assert!(
            !generated.contains("api.EncodeU32"),
            "Export must not use api.EncodeU32 (returns uint64 but downstream expects uint32), got:\n{generated}"
        );
        assert!(
            !generated.contains("api.DecodeU32"),
            "Export must not use api.DecodeU32 (needless uint32→uint64→uint32 round-trip), got:\n{generated}"
        );
    }

    /// Regression test: export function with a variant parameter containing
    /// a u32 payload must generate Go code where I32FromU32 produces a
    /// uint32 value matching the VariantLower variable declaration.
    /// Previously I32FromU32 used api.EncodeU32() which returns uint64,
    /// causing a Go compile error: cannot use uint64 as uint32.
    #[test]
    fn test_export_variant_u32_no_encode_u32() {
        let fixture = Fixture::parse(
            "package test:fixture;
            world test-world {
                variant u32-option { some-val(u32), none-val }
                export process-u32-option: func(opt: u32-option) -> u32;
            }",
        );
        let generated = generate(&fixture, "process-u32-option");
        println!("Generated u32-option function:\n{}", generated);

        // VariantLower declares `var variant_1 uint32` for the I32 payload slot.
        // I32FromU32 must NOT use api.EncodeU32 (returns uint64 → type mismatch)
        assert!(
            !generated.contains("api.EncodeU32"),
            "I32FromU32 must not use api.EncodeU32 in exports (returns uint64, \
             but VariantLower variable is uint32), got:\n{generated}"
        );
    }

    /// Regression test: export function with a variant parameter containing
    /// a u64 payload must generate Go code where I64FromU64 produces a
    /// uint64 value matching the VariantLower variable declaration.
    /// Previously I64FromU64 used int64() which returns int64, causing a
    /// Go compile error: cannot use int64 as uint64.
    #[test]
    fn test_export_variant_u64_no_int64_cast() {
        let fixture = Fixture::parse(
            "package test:fixture;
            world test-world {
                variant u64-option { some-val(u64), none-val }
                export process-u64-option: func(opt: u64-option) -> u64;
            }",
        );
        let generated = generate(&fixture, "process-u64-option");
        println!("Generated u64-option function:\n{}", generated);

        // VariantLower declares `var variant_1 uint64` for the I64 payload slot.
        // I64FromU64 must NOT use int64() (returns int64 → type mismatch)
        assert!(
            !generated.contains(":= int64("),
            "I64FromU64 must not use int64() cast in exports (returns int64, \
             but VariantLower variable is uint64), got:\n{generated}"
        );
    }

    /// Generates everything `fixture`'s world exports.
    fn generate_world(fixture: &Fixture) -> String {
        let instance = GoIdentifier::public("TestInstance");
        let config = ExportConfig {
            instance: &instance,
            world: fixture.world(),
            resolve: &fixture.resolve,
            sizes: &fixture.sizes,
        };
        let mut tokens = Tokens::new();
        ExportGenerator::new(config).format_into(&mut tokens);
        tokens.to_string().unwrap()
    }

    /// A resource the guest implements becomes a Go handle type. The guest
    /// exports an interface's functions as `<interface>#<function>`, receives
    /// a borrow of its own resource as the bare representation, and returns
    /// an owned resource as a handle that leaves its table.
    #[test]
    fn test_exported_resource() {
        let fixture = Fixture::parse(
            "package test:fixture;
            interface things {
                resource thing {
                    constructor(n: u32);
                    get: func() -> u32;
                    join: static func(a: thing, b: borrow<thing>) -> thing;
                }
                peek: func(t: borrow<thing>) -> u32;
            }
            world test-world {
                export things;
            }",
        );
        let generated = generate_world(&fixture);
        println!("Generated:\n{generated}");

        assert!(generated.contains("type Thing struct {"));
        assert!(generated.contains("func (self *Thing) Drop(ctx context.Context) error {"));
        assert!(generated.contains("ExportedFunction(\"test:fixture/things#[dtor]thing\")"));

        // The constructor and the static function are methods of the instance.
        assert!(generated.contains("func (i *TestInstance) NewThing("));
        assert!(generated.contains("func (i *TestInstance) ThingJoin("));
        assert!(generated.contains("ExportedFunction(\"test:fixture/things#[constructor]thing\")"));
        assert!(generated.contains("i.resources.exportedThing.take(uint32("));
        assert!(generated.contains("&Thing{instance: i, rep: "));

        // A method is a Go method of the handle type and lends `self`.
        assert!(generated.contains("func (self *Thing) Get("));
        assert!(generated.contains("i := self.instance"));
        assert!(generated.contains("arg0 := self"));
        assert!(generated.contains("arg0.lend(i)"));

        // An owned parameter becomes a new handle and spends the Go handle.
        assert!(generated.contains("i.resources.exportedThing.add(arg0.give(i), true)"));
        assert!(generated.contains("arg1.lend(i)"));

        assert!(generated.contains("func (i *TestInstance) Peek("));
        assert!(generated.contains("t *Thing,"));
    }

    /// A world-level export can only name the imported side of a resource:
    /// the host implements it, so the guest receives handles to Go values.
    #[test]
    fn test_export_with_imported_resource() {
        let fixture = Fixture::parse(
            "package test:fixture;
            interface things {
                resource thing {
                    get: func() -> u32;
                }
            }
            world test-world {
                import things;
                use things.{thing};
                export lend: func(t: borrow<thing>);
                export give: func(t: thing) -> thing;
            }",
        );
        let generated = generate_world(&fixture);
        println!("Generated:\n{generated}");

        assert!(generated.contains("t Thing,"));
        assert!(generated.contains("i.resources.importedThing.add(arg0, false)"));
        assert!(generated.contains("i.resources.importedThing.add(arg0, true)"));
        assert!(generated.contains("i.resources.importedThing.take(uint32("));
    }

    /// An interface that is both imported and exported declares each resource
    /// twice. The guest's side is prefixed, and two exported interfaces that
    /// declare the same function name get qualified Go names.
    #[test]
    fn test_resource_imported_and_exported() {
        let fixture = Fixture::parse(
            "package test:fixture;
            interface a {
                resource thing {
                    constructor();
                }
                make: func() -> thing;
            }
            interface b {
                resource thing {
                    constructor();
                }
                make: func() -> thing;
            }
            world test-world {
                import a;
                export a;
                export b;
            }",
        );
        let generated = generate_world(&fixture);
        println!("Generated:\n{generated}");

        assert!(generated.contains("type ExportedAThing struct {"));
        assert!(generated.contains("type BThing struct {"));
        assert!(generated.contains("func (i *TestInstance) NewExportedAThing("));
        assert!(generated.contains("func (i *TestInstance) NewBThing("));
        assert!(generated.contains("func (i *TestInstance) AMake("));
        assert!(generated.contains("func (i *TestInstance) BMake("));
        assert!(generated.contains("i.resources.exportedAThing.take(uint32("));
    }

    /// Regression test: an export whose parameters flatten to more than the
    /// canonical ABI's 16 flat params is lowered *indirectly* - the host must
    /// `cabi_realloc` an area, store each field into it and pass a single
    /// pointer.
    #[test]
    fn test_export_indirect_params_allocates_with_realloc() {
        // A record with 17 u32 fields flattens to 17 core params, one over the
        // limit of 16, which forces indirect parameter lowering.
        let fields: String = (0..17).map(|i| format!("field{i}: u32, ")).collect();
        let fixture = Fixture::parse(&format!(
            "package test:fixture;
            world test-world {{
                record wide {{ {fields} }}
                export take-wide: func(wide: wide) -> u32;
            }}"
        ));
        let generated = generate(&fixture, "take-wide");
        println!("Generated wide-record function:\n{}", generated);

        // The param area is allocated through the guest's `cabi_realloc` with
        // the record's alignment (4) and size (17 * 4 = 68 bytes).
        assert!(
            generated.contains("ExportedFunction(\"cabi_realloc\").Call(ctx, 0, 0, 4, 68)"),
            "indirect params must allocate the param area via cabi_realloc, got:\n{generated}"
        );
        // Each field is stored into that area, and the pointer is the only
        // argument passed to the exported wasm function.
        assert!(
            generated.contains("Memory().WriteUint32Le"),
            "indirect params must be stored into the allocated area, got:\n{generated}"
        );
        assert!(
            generated.contains("ExportedFunction(\"take-wide\").Call(ctx, uint64(ptr"),
            "the wasm export must be called with the param area pointer, got:\n{generated}"
        );
    }
}
