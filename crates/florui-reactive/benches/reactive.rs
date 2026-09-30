//! Baseline for the reactive core: signal writes with and without a host
//! listening, batching, steady-state re-render cost of hook-heavy
//! components, and keyed child scopes. Same convention as the other
//! crates' baselines: state built outside the timed closure, "100, 1,000"
//! scale, no pass/fail budget.

use std::cell::Cell;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use florui_reactive::{
    ComponentScope, Signal, batch, use_child_scope_keyed, use_effect, use_memo, use_signal,
};

fn signal_in_scope(scope: &ComponentScope) -> Signal<u64> {
    scope.render(|| use_signal(|| 0u64))
}

fn bench_signal_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("reactive/signal_write");
    for &n in &[100usize, 1000] {
        let (scope, dirty) = ComponentScope::new();
        let wakes = Rc::new(Cell::new(0usize));
        let counter = Rc::clone(&wakes);
        dirty.on_mark(move || counter.set(counter.get() + 1));
        let signal = signal_in_scope(&scope);

        group.bench_function(format!("{n}_unbatched"), |b| {
            b.iter(|| {
                for i in 0..n as u64 {
                    signal.set(i);
                }
                black_box(signal.get())
            });
        });
        group.bench_function(format!("{n}_batched"), |b| {
            b.iter(|| {
                batch(|| {
                    for i in 0..n as u64 {
                        signal.set(i);
                    }
                });
                black_box(signal.get())
            });
        });
    }
    group.finish();
}

fn bench_rerender_hooks(c: &mut Criterion) {
    let mut group = c.benchmark_group("reactive/rerender_hooks");
    for &n in &[100usize, 1000] {
        let (scope, _dirty) = ComponentScope::new();
        let render = |scope: &ComponentScope, dep: u64| {
            scope.render(|| {
                let mut total = 0u64;
                for _ in 0..n {
                    let signal = use_signal(|| 1u64);
                    total += use_memo(dep, |d| *d + signal.get());
                }
                total
            })
        };
        render(&scope, 0);

        group.bench_function(format!("{n}_memos_unchanged"), |b| {
            b.iter(|| black_box(render(&scope, 0)));
        });
        let mut dep = 0u64;
        group.bench_function(format!("{n}_memos_recomputed"), |b| {
            b.iter(|| {
                dep += 1;
                black_box(render(&scope, dep))
            });
        });
    }
    group.finish();
}

fn bench_effects(c: &mut Criterion) {
    let mut group = c.benchmark_group("reactive/effects");
    for &n in &[100usize, 1000] {
        let (scope, _dirty) = ComponentScope::new();
        let sink = Rc::new(Cell::new(0u64));
        let mut generation = 0u64;
        group.bench_function(format!("{n}_rerun"), |b| {
            b.iter(|| {
                generation += 1;
                scope.render(|| {
                    for _ in 0..n {
                        let sink = Rc::clone(&sink);
                        use_effect(generation, move || {
                            sink.set(sink.get() + 1);
                            None
                        });
                    }
                });
            });
        });
        black_box(sink.get());
    }
    group.finish();
}

fn bench_keyed_children(c: &mut Criterion) {
    let mut group = c.benchmark_group("reactive/keyed_children");
    for &n in &[100usize, 1000] {
        let (scope, _dirty) = ComponentScope::new();
        let render = |scope: &ComponentScope, reversed: bool| {
            scope.render(|| {
                for i in 0..n {
                    let key = if reversed { n - 1 - i } else { i };
                    use_child_scope_keyed(key as u64, || use_signal(|| key).get());
                }
            });
        };
        render(&scope, false);

        group.bench_function(format!("{n}_steady"), |b| {
            b.iter(|| render(&scope, false));
        });
        let mut reversed = false;
        group.bench_function(format!("{n}_reordered"), |b| {
            b.iter(|| {
                reversed = !reversed;
                render(&scope, reversed);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_signal_writes,
    bench_rerender_hooks,
    bench_effects,
    bench_keyed_children,
);
criterion_main!(benches);
