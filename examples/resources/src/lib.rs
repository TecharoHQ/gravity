// The guest exports an interface, so its export names contain characters
// (`arcjet:resources/guest#[constructor]accumulator`) that a native linker
// rejects in a shared library. The workspace build also compiles every
// example for the host, where this crate is therefore empty.
#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};

use arcjet::resources::host;
use exports::arcjet::resources::guest::{
    Accumulator, AccumulatorBorrow, Guest as GuestInterface, GuestAccumulator, Holder, Tagged,
};

wit_bindgen::generate!({
    world: "resources",
});

struct ResourcesWorld;

export!(ResourcesWorld);

thread_local! {
    static RETAINED: RefCell<Option<Counter>> = const { RefCell::new(None) };
    static LIVE: Cell<u32> = const { Cell::new(0) };
}

struct Acc {
    label: String,
    total: RefCell<u32>,
}

impl Acc {
    fn with(label: String, total: u32) -> Self {
        LIVE.with(|live| live.set(live.get() + 1));
        Self {
            label,
            total: RefCell::new(total),
        }
    }
}

impl Drop for Acc {
    fn drop(&mut self) {
        LIVE.with(|live| live.set(live.get() - 1));
    }
}

impl GuestAccumulator for Acc {
    fn new(label: String) -> Self {
        Acc::with(label, 0)
    }

    fn add(&self, n: u32) {
        *self.total.borrow_mut() += n;
    }

    fn total(&self) -> u32 {
        *self.total.borrow()
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn combine(a: Accumulator, b: AccumulatorBorrow<'_>) -> Accumulator {
        let a = a.get::<Acc>();
        let b = b.get::<Acc>();
        Accumulator::new(Acc::with(
            format!("{}+{}", a.label, b.label),
            a.total() + b.total(),
        ))
    }
}

impl GuestInterface for ResourcesWorld {
    type Accumulator = Acc;

    fn wrap(tag: String, n: u32) -> Tagged {
        Tagged {
            acc: Accumulator::new(Acc::with(tag.clone(), n)),
            tag,
        }
    }

    fn unwrap(t: Tagged) -> u32 {
        t.acc.get::<Acc>().total()
    }

    fn hold(n: u32) -> Holder {
        match n {
            0 => Holder::Empty,
            n => Holder::Full(Accumulator::new(Acc::with("held".into(), n))),
        }
    }

    fn weigh(h: Holder) -> u32 {
        match h {
            Holder::Empty => 0,
            Holder::Full(acc) => acc.get::<Acc>().total(),
        }
    }

    fn sum(a: AccumulatorBorrow<'_>) -> u32 {
        a.get::<Acc>().total()
    }

    fn finish(a: Accumulator) -> u32 {
        a.get::<Acc>().total()
    }

    fn finish_pair(a: Accumulator, b: Accumulator) -> u32 {
        a.get::<Acc>().total() + b.get::<Acc>().total()
    }

    fn finish_text(a: Accumulator, _text: String) -> u32 {
        a.get::<Acc>().total()
    }

    fn make(label: String, n: u32) -> Accumulator {
        Accumulator::new(Acc::with(label, n))
    }

    fn try_make(label: String) -> Result<Accumulator, String> {
        if label.is_empty() {
            return Err("label must not be empty".into());
        }
        Ok(Accumulator::new(Acc::with(label, 0)))
    }

    fn live() -> u32 {
        LIVE.with(|live| live.get())
    }
}

impl Guest for ResourcesWorld {
    fn ticks(start: u32, n: u32) -> u32 {
        let clock = Clock::new(start);
        for _ in 0..n {
            clock.tick();
        }
        read_clock(&clock)
    }

    fn forget(c: &Counter) {
        // Taking the handle out of the binding stops it from dropping the
        // borrow when this function returns.
        let _ = c.take_handle();
    }

    fn trap_borrow(_c: &Counter) -> Result<u32, String> {
        core::arch::wasm32::unreachable()
    }

    fn borrow_text(c: &Counter, _text: String) -> Result<u32, String> {
        Ok(c.get())
    }

    fn retain_and_trap(c: Counter) -> Result<u32, String> {
        RETAINED.with(|retained| *retained.borrow_mut() = Some(c));
        core::arch::wasm32::unreachable()
    }

    fn retained() -> Counter {
        RETAINED.with(|retained| retained.borrow_mut().take().unwrap())
    }

    fn take_text(c: Counter, _text: String) -> Result<u32, String> {
        Ok(c.get())
    }

    fn bump(c: &Counter) -> u32 {
        c.increment();
        c.get()
    }

    fn take(c: Counter) -> u32 {
        c.get()
    }

    fn roundtrip(c: Counter) -> Counter {
        c.increment();
        c
    }

    fn fresh(start: u32) -> Counter {
        Counter::new(start)
    }

    fn exercise(start: u32) -> String {
        let a = Counter::new(start);
        a.increment();
        let b = host::make_counter(10);
        let description = host::describe(&b);
        let merged = Counter::merge(a, &b);
        let name = merged.name();
        let total = host::consume(merged);
        format!("{description};{name};{total}")
    }

    fn maybe(start: u32, some: bool) -> Option<Counter> {
        some.then(|| Counter::new(start))
    }

    fn batch(n: u32) -> Vec<Counter> {
        (0..n).map(Counter::new).collect()
    }
}
