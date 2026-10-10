package resources

import (
	"context"
	"fmt"
	"runtime"
	"testing"
	"time"
)

// counter is the host's implementation of the `counter` resource.
type counter struct {
	n     uint32
	label string
	host  *host
}

func (c *counter) Increment(ctx context.Context)   { c.n++ }
func (c *counter) Get(ctx context.Context) uint32  { return c.n }
func (c *counter) Name(ctx context.Context) string { return c.label }

// ResourceDrop is the optional hook gravity calls when the guest drops a
// handle that owns the counter.
func (c *counter) ResourceDrop(ctx context.Context) { c.host.dropped = append(c.host.dropped, c.label) }

// host implements the imported `host` interface.
type host struct {
	// dropped lists the labels of the counters the guest dropped, in order.
	dropped []string
}

func (h *host) NewCounter(ctx context.Context, start uint32) Counter {
	return &counter{n: start, label: "new", host: h}
}

func (h *host) CounterMerge(ctx context.Context, a Counter, b Counter) Counter {
	return &counter{n: a.Get(ctx) + b.Get(ctx), label: "merged", host: h}
}

func (h *host) Describe(ctx context.Context, c Counter) string {
	return fmt.Sprintf("counter(%d)", c.Get(ctx))
}

func (h *host) MakeCounter(ctx context.Context, start uint32) Counter {
	return &counter{n: start, label: "made", host: h}
}

func (h *host) Consume(ctx context.Context, c Counter) uint32 {
	return c.Get(ctx)
}

// clock is the host's implementation of the `clock` resource, which the
// world declares itself.
type clock struct{ now uint32 }

func (c *clock) Tick(ctx context.Context) uint32 {
	c.now++
	return c.now
}

// world implements the world's own imports.
type world struct{}

func (world) NewClock(ctx context.Context, start uint32) Clock { return &clock{now: start} }

func (world) ReadClock(ctx context.Context, c Clock) uint32 { return c.(*clock).now }

// live returns the number of handles in a table.
func live[T any](t *resourceTable[T]) int {
	return len(t.entries) - len(t.free)
}

func setup(t *testing.T) (*host, *ResourcesInstance) {
	t.Helper()

	h := &host{}
	fac, err := NewResourcesFactory(t.Context(), h, world{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { fac.Close(context.Background()) })

	ins, err := fac.Instantiate(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ins.Close(context.Background()) })

	return h, ins
}

// wantPanic runs f and fails the test unless it panics.
func wantPanic(t *testing.T, what string, f func()) {
	t.Helper()
	defer func() {
		if recover() == nil {
			t.Errorf("%s: wanted a panic, but got none", what)
		}
	}()
	f()
}

// The guest borrows a counter the host owns. The handle it receives is gone
// when the call returns, and the host's value is not dropped.
func TestBorrowHostResource(t *testing.T) {
	h, ins := setup(t)
	c := &counter{n: 1, label: "mine", host: h}

	if got := ins.Bump(t.Context(), c); got != 2 {
		t.Errorf("wanted: 2, but got: %d", got)
	}
	if c.n != 2 {
		t.Errorf("wanted the host's counter to be 2, but got: %d", c.n)
	}
	if len(h.dropped) != 0 {
		t.Errorf("wanted no drops for a borrow, but got: %v", h.dropped)
	}
	if n := live(&ins.resources.importedCounter); n != 0 {
		t.Errorf("wanted an empty handle table, but %d handles are live", n)
	}
}

// The guest takes ownership of a counter and drops it, which the host hears
// about through ResourceDrop.
func TestGiveHostResource(t *testing.T) {
	h, ins := setup(t)
	c := &counter{n: 5, label: "mine", host: h}

	if got := ins.Take(t.Context(), c); got != 5 {
		t.Errorf("wanted: 5, but got: %d", got)
	}
	if len(h.dropped) != 1 || h.dropped[0] != "mine" {
		t.Errorf("wanted the guest to drop the counter, but got: %v", h.dropped)
	}
	if n := live(&ins.resources.importedCounter); n != 0 {
		t.Errorf("wanted an empty handle table, but %d handles are live", n)
	}
}

// The guest hands an owned counter back: the host gets its own value again
// and nothing is dropped.
func TestRoundtripHostResource(t *testing.T) {
	h, ins := setup(t)
	c := &counter{n: 5, label: "mine", host: h}

	got := ins.Roundtrip(t.Context(), c)
	if got != Counter(c) {
		t.Errorf("wanted the same counter back, but got: %#v", got)
	}
	if c.n != 6 {
		t.Errorf("wanted: 6, but got: %d", c.n)
	}
	if len(h.dropped) != 0 {
		t.Errorf("wanted no drops, but got: %v", h.dropped)
	}
	if n := live(&ins.resources.importedCounter); n != 0 {
		t.Errorf("wanted an empty handle table, but %d handles are live", n)
	}
}

// The guest calls the host's constructor and returns the result.
func TestGuestConstructsHostResource(t *testing.T) {
	_, ins := setup(t)

	c := ins.Fresh(t.Context(), 7).(*counter)
	if c.n != 7 || c.label != "new" {
		t.Errorf("wanted a new counter at 7, but got: %+v", c)
	}
}

// The guest uses every import: constructor, methods, a static function with
// owned and borrowed parameters, and free functions that give and take
// ownership.
func TestGuestUsesHostResources(t *testing.T) {
	h, ins := setup(t)

	const want = "counter(10);merged;12"
	if got := ins.Exercise(t.Context(), 1); got != want {
		t.Errorf("wanted: %s, but got: %s", want, got)
	}
	// The guest gave `a` to merge and `merged` to consume, so the host owns
	// those. It only dropped `b`, the counter it kept.
	if len(h.dropped) != 1 || h.dropped[0] != "made" {
		t.Errorf("wanted the guest to drop only the made counter, but got: %v", h.dropped)
	}
	if n := live(&ins.resources.importedCounter); n != 0 {
		t.Errorf("wanted an empty handle table, but %d handles are live", n)
	}
}

func TestOptionalHostResource(t *testing.T) {
	_, ins := setup(t)

	if got := ins.Maybe(t.Context(), 3, false); got != nil {
		t.Errorf("wanted: nil, but got: %v", got)
	}
	got := ins.Maybe(t.Context(), 3, true)
	if got == nil {
		t.Fatal("wanted a counter, but got: nil")
	}
	if n := (*got).Get(t.Context()); n != 3 {
		t.Errorf("wanted: 3, but got: %d", n)
	}
}

func TestListOfHostResources(t *testing.T) {
	_, ins := setup(t)

	counters := ins.Batch(t.Context(), 3)
	if len(counters) != 3 {
		t.Fatalf("wanted 3 counters, but got: %d", len(counters))
	}
	for i, c := range counters {
		if n := c.Get(t.Context()); n != uint32(i) {
			t.Errorf("counter %d: wanted: %d, but got: %d", i, i, n)
		}
	}
	if n := live(&ins.resources.importedCounter); n != 0 {
		t.Errorf("wanted an empty handle table, but %d handles are live", n)
	}
}

// The host constructs a resource the guest implements, calls its methods and
// drops it, which runs the guest's destructor.
func TestGuestResource(t *testing.T) {
	_, ins := setup(t)
	ctx := t.Context()

	acc := ins.NewAccumulator(ctx, "a")
	acc.Add(ctx, 2)
	acc.Add(ctx, 3)
	if got := acc.Total(ctx); got != 5 {
		t.Errorf("wanted: 5, but got: %d", got)
	}
	if got := acc.Label(ctx); got != "a" {
		t.Errorf("wanted: a, but got: %s", got)
	}
	if got := ins.Sum(ctx, acc); got != 5 {
		t.Errorf("wanted: 5, but got: %d", got)
	}
	if got := ins.Live(ctx); got != 1 {
		t.Errorf("wanted 1 live accumulator, but got: %d", got)
	}

	if err := acc.Drop(ctx); err != nil {
		t.Fatal(err)
	}
	if got := ins.Live(ctx); got != 0 {
		t.Errorf("wanted the destructor to run, but %d accumulators are live", got)
	}
	if err := acc.Drop(ctx); err != nil {
		t.Errorf("wanted a second drop to do nothing, but got: %v", err)
	}
	if got := ins.Live(ctx); got != 0 {
		t.Errorf("wanted a second drop to do nothing, but %d accumulators are live", got)
	}
	wantPanic(t, "method on a dropped handle", func() { acc.Total(ctx) })
}

// The host gives a guest resource back to the guest. The guest drops it, and
// the Go handle is spent.
func TestGiveGuestResource(t *testing.T) {
	_, ins := setup(t)
	ctx := t.Context()

	a := ins.Make(ctx, "a", 5)
	b := ins.Make(ctx, "b", 10)

	// combine takes `a` by value and borrows `b`.
	c := ins.AccumulatorCombine(ctx, a, b)
	if got := c.Label(ctx); got != "a+b" {
		t.Errorf("wanted: a+b, but got: %s", got)
	}
	if got := c.Total(ctx); got != 15 {
		t.Errorf("wanted: 15, but got: %d", got)
	}
	if got := ins.Live(ctx); got != 2 {
		t.Errorf("wanted b and c to be live, but got: %d", got)
	}
	wantPanic(t, "method on a given handle", func() { a.Total(ctx) })
	if got := b.Total(ctx); got != 10 {
		t.Errorf("wanted the borrowed accumulator to stay usable at 10, but got: %d", got)
	}

	if got := ins.Finish(ctx, c); got != 15 {
		t.Errorf("wanted: 15, but got: %d", got)
	}
	if got := ins.Live(ctx); got != 1 {
		t.Errorf("wanted only b to be live, but got: %d", got)
	}
	if err := b.Drop(ctx); err != nil {
		t.Fatal(err)
	}
	if got := ins.Live(ctx); got != 0 {
		t.Errorf("wanted no live accumulators, but got: %d", got)
	}
	if n := live(&ins.resources.exportedAccumulator); n != 0 {
		t.Errorf("wanted an empty handle table, but %d handles are live", n)
	}
}

func TestFallibleGuestResource(t *testing.T) {
	_, ins := setup(t)
	ctx := t.Context()

	if _, err := ins.TryMake(ctx, ""); err == nil {
		t.Error("wanted an error for an empty label, but got none")
	}
	acc, err := ins.TryMake(ctx, "ok")
	if err != nil {
		t.Fatal(err)
	}
	if got := acc.Label(ctx); got != "ok" {
		t.Errorf("wanted: ok, but got: %s", got)
	}
}

// Every instance has its own handle tables and its own guest resources.
func TestInstancesAreIsolated(t *testing.T) {
	h := &host{}
	fac, err := NewResourcesFactory(t.Context(), h, world{})
	if err != nil {
		t.Fatal(err)
	}
	defer fac.Close(t.Context())
	ctx := t.Context()

	first, err := fac.Instantiate(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close(ctx)
	second, err := fac.Instantiate(ctx)
	if err != nil {
		t.Fatal(err)
	}

	acc := first.Make(ctx, "a", 1)
	if got := first.Live(ctx); got != 1 {
		t.Errorf("first instance: wanted 1 live accumulator, but got: %d", got)
	}
	if got := second.Live(ctx); got != 0 {
		t.Errorf("second instance: wanted 0 live accumulators, but got: %d", got)
	}
	wantPanic(t, "handle used with another instance", func() { second.Sum(ctx, acc) })

	// Closing an instance forgets its tables and leaves the other usable.
	if err := second.Close(ctx); err != nil {
		t.Fatal(err)
	}
	count := 0
	fac.instances.Range(func(_, _ any) bool { count++; return true })
	if count != 1 {
		t.Errorf("wanted 1 registered instance after a close, but got: %d", count)
	}
	if got := first.Sum(ctx, acc); got != 1 {
		t.Errorf("wanted: 1, but got: %d", got)
	}
}

// The world declares a resource itself. The guest reaches it and the
// world-level functions that use it through the `$root` module.
func TestWorldResource(t *testing.T) {
	_, ins := setup(t)

	if got := ins.Ticks(t.Context(), 10, 3); got != 13 {
		t.Errorf("wanted: 13, but got: %d", got)
	}
	if n := live(&ins.resources.importedClock); n != 0 {
		t.Errorf("wanted an empty handle table, but %d handles are live", n)
	}
}

// A borrow ends with the call that lent it. A guest that returns while it
// still holds the handle is trapped, and the handle is taken away.
func TestKeptBorrowTraps(t *testing.T) {
	h, ins := setup(t)
	c := &counter{n: 1, label: "mine", host: h}

	wantPanic(t, "guest keeps a borrowed handle", func() { ins.Forget(t.Context(), c) })
	if n := live(&ins.resources.importedCounter); n != 0 {
		t.Errorf("wanted the kept handle to be removed, but %d handles are live", n)
	}
	if len(h.dropped) != 0 {
		t.Errorf("wanted no drops for a borrow, but got: %v", h.dropped)
	}

	// The instance stays usable.
	if got := ins.Bump(t.Context(), c); got != 2 {
		t.Errorf("wanted: 2, but got: %d", got)
	}
}

func TestDropBorrows(t *testing.T) {
	var table resourceTable[string]
	owned := table.add("owned", true)
	table.add("borrowed", false)
	table.add("borrowed too", false)

	if got := table.dropBorrows(); got != 2 {
		t.Errorf("wanted 2 kept borrows, but got: %d", got)
	}
	if got := table.dropBorrows(); got != 0 {
		t.Errorf("wanted no borrows left, but got: %d", got)
	}
	if value, ok := table.get(owned); !ok || value != "owned" {
		t.Errorf("wanted the owned handle to stay, but got: %q, %v", value, ok)
	}
	if n := live(&table); n != 1 {
		t.Errorf("wanted 1 live handle, but got: %d", n)
	}
}

// A handle that is garbage collected without a Drop is destroyed by a later
// call into the instance.
func TestCollectedGuestResource(t *testing.T) {
	_, ins := setup(t)
	ctx := t.Context()

	// Made in a separate function so that no handle stays on this stack.
	func() {
		ins.Make(ctx, "forgotten", 1)
		ins.NewAccumulator(ctx, "also forgotten")
	}()
	kept := ins.Make(ctx, "kept", 2)

	deadline := time.Now().Add(10 * time.Second)
	for ins.Live(ctx) != 1 {
		if time.Now().After(deadline) {
			t.Fatalf("wanted the collected accumulators to be destroyed, but %d are live", ins.Live(ctx))
		}
		runtime.GC()
		time.Sleep(10 * time.Millisecond)
	}

	// A handle that is still referenced is not collected, and a handle that
	// was dropped by hand is not destroyed a second time.
	if got := kept.Total(ctx); got != 2 {
		t.Errorf("wanted: 2, but got: %d", got)
	}
	if err := kept.Drop(ctx); err != nil {
		t.Fatal(err)
	}
	kept = nil
	for range 5 {
		runtime.GC()
		time.Sleep(10 * time.Millisecond)
	}
	if got := ins.Live(ctx); got != 0 {
		t.Errorf("wanted no live accumulators, but got: %d", got)
	}
}

// A record and a variant carry handles to a resource the guest implements.
func TestGuestResourceInRecordAndVariant(t *testing.T) {
	_, ins := setup(t)
	ctx := t.Context()

	var tagged Tagged = ins.Wrap(ctx, "tag", 4)
	if tagged.Tag != "tag" {
		t.Errorf("wanted: tag, but got: %s", tagged.Tag)
	}
	var acc *Accumulator = tagged.Acc
	acc.Add(ctx, 1)
	if got := ins.Unwrap(ctx, tagged); got != 5 {
		t.Errorf("wanted: 5, but got: %d", got)
	}
	wantPanic(t, "method on a handle given away in a record", func() { acc.Total(ctx) })

	if _, ok := ins.Hold(ctx, 0).(HolderEmpty); !ok {
		t.Error("wanted an empty holder")
	}
	full, ok := ins.Hold(ctx, 9).(HolderFull)
	if !ok {
		t.Fatal("wanted a full holder")
	}
	if got := full.Value.Total(ctx); got != 9 {
		t.Errorf("wanted: 9, but got: %d", got)
	}
	if got := ins.Weigh(ctx, full); got != 9 {
		t.Errorf("wanted: 9, but got: %d", got)
	}
	if got := ins.Weigh(ctx, HolderEmpty{}); got != 0 {
		t.Errorf("wanted: 0, but got: %d", got)
	}
	if got := ins.Live(ctx); got != 0 {
		t.Errorf("wanted no live accumulators, but got: %d", got)
	}
}
