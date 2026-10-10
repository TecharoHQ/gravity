use wit_bindgen_core::wit_parser::{Function, Type};

use crate::go::{GoIdentifier, GoType};

/// An analyzed WIT import.
#[derive(Debug, Clone)]
pub struct AnalyzedImports {
    /// All of the WIT interfaces in the input world.
    pub interfaces: Vec<AnalyzedInterface>,
    /// All standalone types found in the input world.
    pub standalone_types: Vec<AnalyzedType>,
    /// All standalone functions found in the input world.
    pub standalone_functions: Vec<AnalyzedFunction>,

    /// The handle tables for the world's resources. `None` when the world
    /// declares no resource, so worlds without resources generate the same
    /// code as before resources were supported.
    pub resources: Option<AnalyzedResources>,

    /// The name of the factory type to be generated.
    pub factory_name: GoIdentifier,
    /// The name of the instance type to be generated.
    pub instance_name: GoIdentifier,
    /// The name of the constructor for the factory type.
    pub constructor_name: GoIdentifier,
}

/// An analyzed WIT interface with all its metadata.
///
/// A WIT interface looks like this:
///
/// ```wit
/// interface foo {
///     /// Types specific to the interface.
///     type foo-type = u32;
///
///     /// Functions specific to the interface.
///     foo-func: func(input: string) -> string;
/// }
/// ```
///
/// So it has a name, a list of methods, and a list of types.
///
#[derive(Debug, Clone)]
pub struct AnalyzedInterface {
    /// The name of the interface.
    pub name: String,
    /// Every function of the interface, including the constructors, methods
    /// and static functions of its resources.
    pub methods: Vec<InterfaceMethod>,
    pub types: Vec<AnalyzedType>,
    /// The resources the interface declares.
    pub resources: Vec<AnalyzedResource>,

    /// The Go interface type name (e.g., "ITestWorldLogger")
    ///
    /// E.g. the `ILogger` in `type ILogger interface { ... }`.
    pub go_interface_name: GoIdentifier,
    /// The parameter name this instance of the interface will have in the factory constructor.
    ///
    /// E.g. the `logger` in `NewLoggerFactory(ctx context.Context, logger ILogger)`
    pub constructor_param_name: GoIdentifier,
    /// The module name for the wazero host module builder.
    /// Used as the argument to `wazeroRuntime.NewHostModuleBuilder`.
    ///
    /// E.g. the `argjet:basic/logger` in `wazeroRuntime.NewHostModuleBuilder("argjet:basic/logger")`
    pub wazero_module_name: String,
}

/// A resource declared in an imported interface, which the host implements.
///
/// ```wit
/// resource counter {
///     constructor(start: u32);
///     get: func() -> u32;
/// }
/// ```
///
/// The resource becomes a Go interface with one method per WIT method. Its
/// constructor and static functions become methods of the Go interface
/// generated for the WIT interface that declares it.
#[derive(Debug, Clone)]
pub struct AnalyzedResource {
    /// The name of the resource in WIT.
    pub name: String,
    /// The Go interface type the host implements, e.g. `Counter`.
    pub go_type_name: GoIdentifier,
    /// The field holding this resource's handle table in the per-instance
    /// table struct.
    pub table_field: GoIdentifier,
    /// The resource's methods, without constructors and static functions.
    pub methods: Vec<InterfaceMethod>,
}

/// The handle tables of a world, kept once per module instance.
#[derive(Debug, Clone)]
pub struct AnalyzedResources {
    /// The Go struct type holding one table per resource.
    pub tables_name: GoIdentifier,
    /// Resources the host implements: the table field and the Go interface
    /// type its entries hold.
    pub imported: Vec<(GoIdentifier, GoIdentifier)>,
    /// Resources the guest implements: the table field. The entries are the
    /// guest's `u32` representations.
    pub exported: Vec<GoIdentifier>,
    /// The host modules that give the guest its own resources' intrinsics.
    pub guest_modules: Vec<GuestResourceModule>,
}

/// The `[export]<interface>` host module for one exported interface. A guest
/// that implements a resource imports `[resource-new]`, `[resource-rep]` and
/// `[resource-drop]` for it from this module.
#[derive(Debug, Clone)]
pub struct GuestResourceModule {
    /// The module name, e.g. `[export]arcjet:resources/guest`.
    pub wazero_module_name: String,
    /// The name the interface's functions are exported under, e.g.
    /// `arcjet:resources/guest`.
    pub interface_name: String,
    /// The WIT name and table field of each resource.
    pub resources: Vec<(String, GoIdentifier)>,
}

/// Method signature for an interface
#[derive(Debug, Clone)]
pub struct InterfaceMethod {
    /// The name of the interface method.
    pub name: String,
    /// The Go identifier of the interface method.
    ///
    /// E.g. the `Log` in `func (l *Logger) Log(ctx context.Context, message string)`
    pub go_method_name: GoIdentifier,
    /// The parameters of the interface method. A resource method's `self`
    /// is not included: it is the receiver of the Go method.
    pub parameters: Vec<Parameter>,
    /// The return type of the interface method.
    ///
    /// This stores both the WIT type and the Go type.
    pub return_type: Option<WitReturn>,
    /// Raw WIT function, used to generate the body of the interface method.
    pub wit_function: Function,
}

/// A parameter of an interface method.
#[derive(Debug, Clone)]
pub struct Parameter {
    /// The Go identifier of the parameter.
    pub name: GoIdentifier,
    /// The Go type of the parameter.
    pub go_type: GoType,
    /// The WIT type of the parameter.
    pub wit_type: Type,
}

/// The return type of an interface method.
#[derive(Debug, Clone)]
pub struct WitReturn {
    /// The Go type of the return type.
    pub go_type: GoType,
    /// The WIT type of the return type.
    pub wit_type: Type,
}

/// An analyzed WIT type definition.
#[derive(Debug, Clone)]
pub struct AnalyzedType {
    /// The name of the type in the WIT world.
    pub name: String,
    /// The Go identifier of the type.
    pub go_type_name: GoIdentifier,
    /// The definition of the type.
    pub definition: TypeDefinition,
}

/// The definition of a WIT type.
#[derive(Debug, Clone)]
pub enum TypeDefinition {
    /// A struct-like type with named fields
    Record { fields: Vec<(GoIdentifier, GoType)> },
    /// A union-like type with multiple cases, each optionally carrying data.
    Variant { cases: Vec<VariantCase> },
    /// A simple enumeration with named constants
    Enum { cases: Vec<String> },
    /// A type alias that wraps another type
    Alias { target: GoType },
    /// A primitive type that doesn't need special handling
    Primitive,
}

#[derive(Debug, Clone)]
pub struct VariantCase {
    pub name: String,
    /// `None` for unit cases.
    pub payload: Option<GoType>,
    pub dispatch: CaseDispatch,
}

/// How a variant case is represented in Go. See `crate::CaseDispatchKind`
/// for the underlying decision; this enum carries the resolved Go names so
/// they aren't re-derived at emission time.
#[derive(Debug, Clone)]
pub enum CaseDispatch {
    /// WIT shorthand `case(case)`: the payload record itself implements
    /// the variant's marker interface, so callers construct
    /// `MyRecord{...}` directly.
    DirectRecord { record_type: GoType },
    /// Dedicated `{VariantName}{CaseName}` wrapper struct with an optional
    /// `Value` field. Callers construct `Wrapper{Value: payload}` (or
    /// `Wrapper{}` for unit cases) and read the payload via `.Value`.
    Wrapped { wrapper_name: GoIdentifier },
}

/// An analyzed WIT function.
#[derive(Debug, Clone)]
pub struct AnalyzedFunction {
    pub name: String,
    pub go_name: GoIdentifier,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<GoType>,
}
