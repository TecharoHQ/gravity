package resources_shared

import (
	"context"
	"testing"
)

// The host's implementations of the resources in the imported `types-a` and
// `types-b` interfaces. Both interfaces declare a `foo`, so the Go names
// carry the interface: TypesAFoo and TypesBFoo.

type fooA struct {
	x uint32
	h *host
}

func (f *fooA) GetX(ctx context.Context) uint32    { return f.x }
func (f *fooA) SetX(ctx context.Context, n uint32) { f.x = n }
func (f *fooA) ResourceDrop(ctx context.Context)   { f.h.dropped++ }

type bar struct {
	value string
	h     *host
}

func (b *bar) GetValue(ctx context.Context) string  { return b.value }
func (b *bar) Append(ctx context.Context, s string) { b.value += s }
func (b *bar) ResourceDrop(ctx context.Context)     { b.h.dropped++ }

type fooB struct {
	y string
	h *host
}

func (f *fooB) GetY(ctx context.Context) string    { return f.y }
func (f *fooB) SetY(ctx context.Context, s string) { f.y = s }
func (f *fooB) ResourceDrop(ctx context.Context)   { f.h.dropped++ }

type baz struct {
	count uint32
	h     *host
}

func (b *baz) Increment(ctx context.Context)       { b.count++ }
func (b *baz) GetCount(ctx context.Context) uint32 { return b.count }
func (b *baz) ResourceDrop(ctx context.Context)    { b.h.dropped++ }

// host counts the resources the guest dropped.
type host struct {
	dropped int
}

type hostA struct{ h *host }

func (a hostA) NewFoo(ctx context.Context, x uint32) TypesAFoo { return &fooA{x: x, h: a.h} }
func (a hostA) NewBar(ctx context.Context, value string) Bar   { return &bar{value: value, h: a.h} }
func (a hostA) DoubleFooX(ctx context.Context, f TypesAFoo) uint32 {
	return f.GetX(ctx) * 2
}
func (a hostA) MakeBar(ctx context.Context, value string) Bar { return &bar{value: value, h: a.h} }

type hostB struct{ h *host }

func (b hostB) NewFoo(ctx context.Context, y string) TypesBFoo { return &fooB{y: y, h: b.h} }
func (b hostB) NewBaz(ctx context.Context, count uint32) Baz   { return &baz{count: count, h: b.h} }
func (b hostB) TripleBazCount(ctx context.Context, z Baz) uint32 {
	return z.GetCount(ctx) * 3
}
func (b hostB) MakeFoo(ctx context.Context, y string) TypesBFoo { return &fooB{y: y, h: b.h} }

func setup(t *testing.T) (*host, *ResourcesSharedInstance) {
	t.Helper()

	h := &host{}
	fac, err := NewResourcesSharedFactory(t.Context(), hostA{h}, hostB{h})
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

// The guest uses the host's side of both interfaces.
func TestHostSide(t *testing.T) {
	h, ins := setup(t)

	const want = "ybar!;33;made"
	if got := ins.UseHost(t.Context(), 4, "y"); got != want {
		t.Errorf("wanted: %s, but got: %s", want, got)
	}
	// The guest made five resources and dropped them all when it returned.
	if h.dropped != 5 {
		t.Errorf("wanted 5 dropped resources, but got: %d", h.dropped)
	}
}

// The host uses the guest's side of both interfaces. Its handle types carry
// an Exported prefix, because the host's side already uses the plain names.
func TestGuestSide(t *testing.T) {
	_, ins := setup(t)
	ctx := t.Context()

	foo := ins.NewExportedTypesAFoo(ctx, 21)
	if got := ins.DoubleFooX(ctx, foo); got != 42 {
		t.Errorf("wanted: 42, but got: %d", got)
	}
	foo.SetX(ctx, 4)
	if got := foo.GetX(ctx); got != 4 {
		t.Errorf("wanted: 4, but got: %d", got)
	}

	var made *ExportedBar = ins.MakeBar(ctx, "hello")
	made.Append(ctx, " world")
	if got := made.GetValue(ctx); got != "hello world" {
		t.Errorf("wanted: hello world, but got: %s", got)
	}

	var fooB *ExportedTypesBFoo = ins.MakeFoo(ctx, "y")
	fooB.SetY(ctx, fooB.GetY(ctx)+"!")
	if got := fooB.GetY(ctx); got != "y!" {
		t.Errorf("wanted: y!, but got: %s", got)
	}

	baz := ins.NewExportedBaz(ctx, 1)
	baz.Increment(ctx)
	if got := ins.TripleBazCount(ctx, baz); got != 6 {
		t.Errorf("wanted: 6, but got: %d", got)
	}

	for _, drop := range []func(context.Context) error{foo.Drop, made.Drop, fooB.Drop, baz.Drop} {
		if err := drop(ctx); err != nil {
			t.Error(err)
		}
	}
}
