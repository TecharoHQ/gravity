use std::collections::BTreeMap;

use genco::prelude::*;

use crate::{
    codegen::ir::{AnalyzedImports, AnalyzedResources},
    go::{
        GoIdentifier, comment,
        imports::{
            ATOMIC_UINT64, CONTEXT_CONTEXT, ERRORS_NEW, STRCONV_FORMAT_UINT, SYNC_MAP, SYNC_MUTEX,
            WAZERO_API_MEMORY, WAZERO_API_MODULE, WAZERO_COMPILED_MODULE, WAZERO_NEW_MODULE_CONFIG,
            WAZERO_NEW_RUNTIME, WAZERO_RUNTIME,
        },
    },
};

/// Configuration for factory generation
pub struct FactoryConfig<'a> {
    pub analyzed_imports: &'a AnalyzedImports,
    pub import_chains: BTreeMap<String, Tokens<Go>>,
    pub wasm_var_name: &'a GoIdentifier,
}

/// Generator for factory and instance types
pub struct FactoryGenerator<'a> {
    config: FactoryConfig<'a>,
}

impl<'a> FactoryGenerator<'a> {
    /// Create a new factory generator with the given config.
    pub fn new(config: FactoryConfig<'a>) -> Self {
        Self { config }
    }

    /// Get the instance name from the analyzed imports.
    pub fn instance_name(&self) -> &GoIdentifier {
        &self.config.analyzed_imports.instance_name
    }

    /// Generate the `writeString` helper function.
    fn generate_write_string(&self, tokens: &mut Tokens<Go>) {
        // Add writeString helper function for interface string returns
        quote_in! { *tokens =>
            $(comment(&[
                "writeString will put a Go string into the Wasm memory following the Component",
                "Model calling conventions, such as allocating memory with the realloc function",
            ]))
            func writeString(
                ctx $CONTEXT_CONTEXT,
                s string,
                memory $WAZERO_API_MEMORY,
                realloc api.Function,
            ) (uint64, uint64, error) {
                if len(s) == 0 {
                    return 1, 0, nil
                }

                results, err := realloc.Call(ctx, 0, 0, 1, uint64(len(s)))
                if err != nil {
                    return 1, 0, err
                }
                ptr := results[0]
                ok := memory.Write(uint32(ptr), []byte(s))
                if !ok {
                    return 1, 0, $ERRORS_NEW("failed to write string to wasm memory")
                }
                return uint64(ptr), uint64(len(s)), nil
            }
            $['\n']
        };
    }

    /// Generate the factory lifecycle, with resource-specific registration when needed.
    ///
    /// Host modules are shared by all instances. Resource worlds register each
    /// instance's tables under its unique module name before its start function
    /// can call a host function that uses those tables.
    fn generate_factory(&self, tokens: &mut Tokens<Go>) {
        let AnalyzedImports {
            factory_name,
            instance_name,
            constructor_name,
            resources,
            ..
        } = &self.config.analyzed_imports;
        let wasm_var_name = self.config.wasm_var_name;
        let params = self.build_parameters();
        quote_in! { *tokens =>
            $['\n']
            type $factory_name struct {
                runtime $WAZERO_RUNTIME
                module  $WAZERO_COMPILED_MODULE
                $(if resources.is_some() {
                    $(comment(&["instances maps a module instance's name to its resource tables."]))
                    instances *$SYNC_MAP
                    nextInstance $ATOMIC_UINT64
                })
            }
            $['\n']
            func $constructor_name(
                $['\r']
                $params
                $['\r']
            ) (*$factory_name, error) {
                wazeroRuntime := $WAZERO_NEW_RUNTIME(ctx)

                $(if let Some(resources) = resources {
                    $(let tables_name = &resources.tables_name)
                    instances := &$SYNC_MAP{}
                    $(comment(&["resourcesOf returns the resource tables of the instance calling a host function."]))
                    resourcesOf := func(mod $WAZERO_API_MODULE) *$tables_name {
                        tables, ok := instances.Load(mod.Name())
                        if !ok {
                            panic($ERRORS_NEW("module instance has no resource tables"))
                        }
                        return tables.(*$tables_name)
                    }
                })

                $(for chain in self.config.import_chains.values() =>
                    $chain
                    $['\r']
                )

                $(comment(&[
                    "Compiling the module takes a LONG time, so we want to do it once and hold",
                       "onto it with the Runtime",
                ]))
                module, err := wazeroRuntime.CompileModule(ctx, $wasm_var_name)
                if err != nil {
                    return nil, err
                }
                return &$factory_name{
                    runtime: wazeroRuntime,
                    module: module,
                    $(if resources.is_some() { instances: instances, })
                }, nil
            }
            $['\n']
            func (f *$factory_name) Instantiate(ctx $CONTEXT_CONTEXT) (*$instance_name, error) {
                $(if let Some(resources) = resources {
                    $(let tables_name = &resources.tables_name)
                    $(comment(&[
                        "The tables are registered before the module starts, because its start",
                        "function may already call host functions that use them.",
                    ]))
                    name := "gravity-" + $STRCONV_FORMAT_UINT(f.nextInstance.Add(1), 10)
                    resources := &$tables_name{}
                    f.instances.Store(name, resources)
                    module, err := f.runtime.InstantiateModule(ctx, f.module, $WAZERO_NEW_MODULE_CONFIG().WithName(name))
                    if err != nil {
                        f.instances.Delete(name)
                        return nil, err
                    }
                    return &$instance_name{
                        module:    module,
                        resources: resources,
                        release:   func() { f.instances.Delete(name) },
                    }, nil
                } else {
                    if module, err := f.runtime.InstantiateModule(ctx, f.module, $WAZERO_NEW_MODULE_CONFIG()); err != nil {
                        return nil, err
                    } else {
                        return &$instance_name{module}, nil
                    }
                })
            }
            $['\n']
            func (f *$factory_name) Close(ctx $CONTEXT_CONTEXT) {
                f.runtime.Close(ctx)
            }
            $['\n']
        };
    }

    /// Generate the per-instance resource tables and the generic handle
    /// table they are made of.
    fn generate_resource_tables(&self, resources: &AnalyzedResources, tokens: &mut Tokens<Go>) {
        let tables_name = &resources.tables_name;
        quote_in! { *tokens =>
            $(comment(&[
                "resourceHandle is one entry of a resourceTable.",
            ]))
            type resourceHandle[T any] struct {
                value T
                $(comment(&["own is false for a handle that only borrows the value."]))
                own  bool
                live bool
            }
            $['\n']
            $(comment(&[
                "resourceTable is a Component Model handle table. The guest refers to a",
                "resource by a handle, an index into this table. Handles start at 1 and the",
                "index of a removed handle is reused.",
            ]))
            type resourceTable[T any] struct {
                mu      $SYNC_MUTEX
                entries []resourceHandle[T]
                free    []uint32
                $(comment(&["borrows counts the live handles that do not own their value."]))
                borrows int
            }
            $['\n']
            $(comment(&["add stores value and returns its new handle."]))
            func (t *resourceTable[T]) add(value T, own bool) uint32 {
                t.mu.Lock()
                defer t.mu.Unlock()
                entry := resourceHandle[T]{value: value, own: own, live: true}
                if !own {
                    t.borrows++
                }
                if n := len(t.free); n > 0 {
                    handle := t.free[n-1]
                    t.free = t.free[:n-1]
                    t.entries[handle-1] = entry
                    return handle
                }
                t.entries = append(t.entries, entry)
                return uint32(len(t.entries))
            }
            $['\n']
            func (t *resourceTable[T]) lookup(handle uint32) *resourceHandle[T] {
                if handle == 0 || uint64(handle) > uint64(len(t.entries)) || !t.entries[handle-1].live {
                    return nil
                }
                return &t.entries[handle-1]
            }
            $['\n']
            $(comment(&["get returns the value of a handle and leaves the handle in the table."]))
            func (t *resourceTable[T]) get(handle uint32) (value T, ok bool) {
                t.mu.Lock()
                defer t.mu.Unlock()
                entry := t.lookup(handle)
                if entry == nil {
                    return value, false
                }
                return entry.value, true
            }
            $['\n']
            $(comment(&["remove deletes a handle and reports whether it owned its value."]))
            func (t *resourceTable[T]) remove(handle uint32) (value T, own bool, ok bool) {
                t.mu.Lock()
                defer t.mu.Unlock()
                entry := t.lookup(handle)
                if entry == nil {
                    return value, false, false
                }
                value, own = entry.value, entry.own
                if !own {
                    t.borrows--
                }
                *entry = resourceHandle[T]{}
                t.free = append(t.free, handle)
                return value, own, true
            }
            $['\n']
            $(comment(&[
                "dropBorrows removes the borrowed handles that are still in the table and",
                "returns how many there were. A borrow ends with the call that lent it, so",
                "the guest must have dropped every one by the time the call returns.",
            ]))
            func (t *resourceTable[T]) dropBorrows() int {
                t.mu.Lock()
                defer t.mu.Unlock()
                kept := t.borrows
                if kept == 0 {
                    return 0
                }
                for index := range t.entries {
                    if entry := &t.entries[index]; entry.live && !entry.own {
                        *entry = resourceHandle[T]{}
                        t.free = append(t.free, uint32(index+1))
                    }
                }
                t.borrows = 0
                return kept
            }
            $['\n']
            $(comment(&[
                "take deletes a handle that owns its value and returns the value. It fails",
                "for a borrowed handle, which cannot transfer ownership.",
            ]))
            func (t *resourceTable[T]) take(handle uint32) (value T, ok bool) {
                t.mu.Lock()
                defer t.mu.Unlock()
                entry := t.lookup(handle)
                if entry == nil || !entry.own {
                    return value, false
                }
                value = entry.value
                *entry = resourceHandle[T]{}
                t.free = append(t.free, handle)
                return value, true
            }
            $['\n']
            $(comment(&[
                "The resource tables of one module instance: the host's values for the",
                "resources it implements, and the guest's representations for the resources",
                "the guest implements.",
            ]))
            type $tables_name struct {
                $(for (field, typ) in &resources.imported join ($['\r']) => $field resourceTable[$typ])
                $(for field in &resources.exported join ($['\r']) => $field resourceTable[uint32])
                $(if !resources.exported.is_empty() {
                    $(comment(&[
                        "collected lists the guest resources whose Go handles the garbage",
                        "collector freed before anyone dropped them.",
                    ]))
                    collectedMu $SYNC_MUTEX
                    collectedCount $ATOMIC_UINT64
                    collected   []collectedResource
                })
            }
            $['\n']
        };
        if resources.exported.is_empty() {
            return;
        }

        let instance_name = &self.config.analyzed_imports.instance_name;
        quote_in! { *tokens =>
            $(comment(&[
                "collectedResource names a guest resource to destroy: the guest's destructor",
                "export and the resource's representation.",
            ]))
            type collectedResource struct {
                dtor string
                rep  uint32
            }
            $['\n']
            $(comment(&[
                "collect queues a resource for destruction. The garbage collector calls it",
                "from its own goroutine when a handle is freed. A module instance is not safe",
                "for concurrent use, so the destructor cannot run here: the next call into",
                "the instance runs it.",
            ]))
            func (t *$tables_name) collect(resource collectedResource) {
                t.collectedMu.Lock()
                defer t.collectedMu.Unlock()
                t.collected = append(t.collected, resource)
                t.collectedCount.Store(uint64(len(t.collected)))
            }
            $['\n']
            $(comment(&[
                "dropCollected runs the guest's destructor for every resource whose handle",
                "the garbage collector freed since the last call.",
            ]))
            func (i *$instance_name) dropCollected(ctx $CONTEXT_CONTEXT) {
                if i.resources.collectedCount.Load() == 0 {
                    return
                }
                i.resources.collectedMu.Lock()
                collected := i.resources.collected
                i.resources.collected = nil
                i.resources.collectedCount.Store(0)
                i.resources.collectedMu.Unlock()
                for index, resource := range collected {
                    if dtor := i.module.ExportedFunction(resource.dtor); dtor != nil {
                        if _, err := dtor.Call(ctx, uint64(resource.rep)); err != nil {
                            $(comment(&[
                                "Preserve the unprocessed resources before any new cleanups. The failed",
                                "destructor can have side effects, so do not retry that resource.",
                            ]))
                            i.resources.collectedMu.Lock()
                            i.resources.collected = append(collected[index+1:], i.resources.collected...)
                            i.resources.collectedCount.Store(uint64(len(i.resources.collected)))
                            i.resources.collectedMu.Unlock()
                            panic(err)
                        }
                    }
                }
            }
            $['\n']
        };
    }

    /// Generate the instance lifecycle, with resource-specific fields when needed.
    fn generate_instance(&self, tokens: &mut Tokens<Go>) {
        let instance_name = &self.config.analyzed_imports.instance_name;
        let resources = &self.config.analyzed_imports.resources;
        quote_in! { *tokens =>
            type $instance_name struct {
                module $WAZERO_API_MODULE
                $(if let Some(resources) = resources {
                    resources *$(&resources.tables_name)
                    $(comment(&["release removes the instance's resource tables from its factory."]))
                    release func()
                })
            }
            $['\n']
            $(if resources.is_some() {
                $(comment(&[
                    "Close closes the module instance. The guest's resources go away with it,",
                    "so handles to them must not be used afterwards.",
                ]))
            })
            func (i *$instance_name) Close(ctx $CONTEXT_CONTEXT) error {
                $(if resources.is_some() { defer i.release() })
                if err := i.module.Close(ctx); err != nil {
                    return err
                }

                return nil
            }
            $['\n']
        };
        if let Some(resources) = resources {
            self.generate_resource_tables(resources, tokens);
        }
    }

    /// Build parameter list for factory constructor
    fn build_parameters(&self) -> Tokens<Go> {
        let interfaces = &self.config.analyzed_imports.interfaces;

        quote! {
            ctx $CONTEXT_CONTEXT,
            $(for interface in interfaces.iter() join ($['\r']) =>
            $(&interface.constructor_param_name) $(&interface.go_interface_name),
            )
        }
    }
}

impl<'a> FormatInto<Go> for &FactoryGenerator<'a> {
    fn format_into(self, tokens: &mut Tokens<Go>) {
        self.generate_factory(tokens);
        tokens.push();
        self.generate_instance(tokens);
        tokens.push();
        self.generate_write_string(tokens);
        tokens.push();
    }
}

#[cfg(test)]
mod tests {
    use genco::lang::go::Tokens;

    use crate::{
        codegen::{FactoryGenerator, factory::FactoryConfig, ir::AnalyzedImports},
        go::GoIdentifier,
    };

    #[test]
    fn test_generate_write_string() {
        let analyzed_imports = &AnalyzedImports {
            interfaces: vec![],
            standalone_types: vec![],
            standalone_functions: vec![],
            resources: None,
            factory_name: GoIdentifier::public("test-factory"),
            instance_name: GoIdentifier::public("test-instance"),
            constructor_name: GoIdentifier::public("test-constructor"),
        };
        let config = FactoryConfig {
            analyzed_imports,
            import_chains: Default::default(),
            wasm_var_name: &GoIdentifier::public("test-wasm"),
        };
        let generator = FactoryGenerator::new(config);
        let mut tokens = Tokens::new();
        generator.generate_write_string(&mut tokens);

        assert!(tokens.to_string().unwrap().contains("func writeString"));
    }
}
