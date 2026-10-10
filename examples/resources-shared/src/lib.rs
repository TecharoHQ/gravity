// The guest exports interfaces, so its export names contain characters that a
// native linker rejects in a shared library. The workspace build also
// compiles every example for the host, where this crate is therefore empty.
#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};

use arcjet::resources_shared::{types_a as host_a, types_b as host_b};
use exports::arcjet::resources_shared::{types_a, types_b};

wit_bindgen::generate!({
    world: "resources-shared",
});

struct ResourcesWorld;

export!(ResourcesWorld);

struct FooA {
    x: Cell<u32>,
}

impl types_a::GuestFoo for FooA {
    fn new(x: u32) -> Self {
        Self { x: Cell::new(x) }
    }

    fn get_x(&self) -> u32 {
        self.x.get()
    }

    fn set_x(&self, n: u32) {
        self.x.set(n);
    }
}

struct Bar {
    value: RefCell<String>,
}

impl types_a::GuestBar for Bar {
    fn new(value: String) -> Self {
        Self {
            value: RefCell::new(value),
        }
    }

    fn get_value(&self) -> String {
        self.value.borrow().clone()
    }

    fn append(&self, s: String) {
        self.value.borrow_mut().push_str(&s);
    }
}

struct FooB {
    y: RefCell<String>,
}

impl types_b::GuestFoo for FooB {
    fn new(y: String) -> Self {
        Self { y: RefCell::new(y) }
    }

    fn get_y(&self) -> String {
        self.y.borrow().clone()
    }

    fn set_y(&self, s: String) {
        self.y.replace(s);
    }
}

struct Baz {
    count: Cell<u32>,
}

impl types_b::GuestBaz for Baz {
    fn new(count: u32) -> Self {
        Self {
            count: Cell::new(count),
        }
    }

    fn increment(&self) {
        self.count.set(self.count.get() + 1);
    }

    fn get_count(&self) -> u32 {
        self.count.get()
    }
}

impl types_a::Guest for ResourcesWorld {
    type Foo = FooA;
    type Bar = Bar;

    fn label_foo(label: String, x: u32) -> types_a::Labeled {
        types_a::Labeled {
            label,
            foo: types_a::Foo::new(FooA { x: Cell::new(x) }),
        }
    }

    fn double_foo_x(f: types_a::FooBorrow<'_>) -> u32 {
        f.get::<FooA>().x.get() * 2
    }

    fn make_bar(value: String) -> types_a::Bar {
        types_a::Bar::new(Bar {
            value: RefCell::new(value),
        })
    }
}

impl types_b::Guest for ResourcesWorld {
    type Foo = FooB;
    type Baz = Baz;

    fn triple_baz_count(b: types_b::BazBorrow<'_>) -> u32 {
        b.get::<Baz>().count.get() * 3
    }

    fn make_foo(y: String) -> types_b::Foo {
        types_b::Foo::new(FooB { y: RefCell::new(y) })
    }
}

impl Guest for ResourcesWorld {
    fn use_host(x: u32, y: String) -> String {
        let foo_a = host_a::Foo::new(x);
        foo_a.set_x(foo_a.get_x() + 1);
        let doubled = host_a::double_foo_x(&foo_a);

        let bar = host_a::make_bar("bar");
        bar.append("!");

        let foo_b = host_b::Foo::new(&y);
        foo_b.set_y(&format!("{}{}", foo_b.get_y(), bar.get_value()));

        let baz = host_b::Baz::new(doubled);
        baz.increment();
        let tripled = host_b::triple_baz_count(&baz);

        let made = host_b::make_foo("made");
        let labeled = host_a::label_foo("label", 7);
        format!(
            "{};{};{};{}={}",
            foo_b.get_y(),
            tripled,
            made.get_y(),
            labeled.label,
            labeled.foo.get_x()
        )
    }
}
