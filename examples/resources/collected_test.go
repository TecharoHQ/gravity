package resources

import (
	"context"
	"errors"
	"slices"
	"testing"
	"time"

	"github.com/tetratelabs/wazero/api"
)

type collectedModule struct {
	api.Module
	destructor api.Function
}

func (m collectedModule) ExportedFunction(string) api.Function { return m.destructor }

type collectedDestructor struct {
	api.Function
	call func(uint32) error
}

func (d collectedDestructor) Call(_ context.Context, params ...uint64) ([]uint64, error) {
	return nil, d.call(uint32(params[0]))
}

func TestCollectedDestructorTrapPreservesRemainingQueue(t *testing.T) {
	var tables resourcesResourceTables
	var calls []uint32
	trap := errors.New("destructor trap")
	ins := &ResourcesInstance{resources: &tables}
	ins.module = collectedModule{destructor: collectedDestructor{call: func(rep uint32) error {
		calls = append(calls, rep)
		if rep == 1 {
			// A cleanup can enqueue another handle while destructors run.
			done := make(chan struct{})
			go func() { tables.collect(collectedResource{rep: 3}); close(done) }()
			<-done
			return trap
		}
		return nil
	}}}
	tables.collect(collectedResource{rep: 1})
	tables.collect(collectedResource{rep: 2})
	wantPanic(t, "failed collected destructor", func() { ins.dropCollected(t.Context()) })
	ins.dropCollected(t.Context())
	if !slices.Equal(calls, []uint32{1, 2, 3}) {
		t.Fatalf("wanted the remaining queue before new cleanups, got calls %v", calls)
	}
	ins.dropCollected(t.Context())
	if !slices.Equal(calls, []uint32{1, 2, 3}) {
		t.Fatalf("destructors ran twice: %v", calls)
	}
}

func TestEmptyCollectedQueueDoesNotLock(t *testing.T) {
	var tables resourcesResourceTables
	ins := &ResourcesInstance{resources: &tables}
	tables.collectedMu.Lock()
	done := make(chan struct{})
	go func() { ins.dropCollected(context.Background()); close(done) }()
	select {
	case <-done:
		tables.collectedMu.Unlock()
	case <-time.After(time.Second):
		tables.collectedMu.Unlock()
		<-done
		t.Fatal("empty collection queue waited for the mutex")
	}
}
