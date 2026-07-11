# Server-Side Rendering

A single Kāra program often compiles to more than one target. The classic
case is **server-side rendering** (SSR): the server renders a page to HTML,
the browser hydrates it and handles interaction — and you want *one*
component to drive both, not two copies that drift apart.

Kāra does this without `#[cfg]` chains in your component. The component
stays an ordinary, target-agnostic function. What differs between server
and client is which **provider** you bind for its resources.

The full, runnable code for this chapter is
[`examples/ssr_counter`](https://github.com/karalang/kara/tree/main/examples/ssr_counter).

## The shared component

The component renders against an abstract resource, `Sink`, rather than
talking to a concrete HTML buffer or DOM. It has no idea which target it is
running on — and no `#[target(...)]` attribute:

```kara
effect resource Sink;

// Target-agnostic: compiles unchanged for the server and the client.
pub fn render_counter(count: i64) with writes(Sink) {
    Sink.heading("Kāra SSR Counter");
    Sink.count(count);
    Sink.parity(count % 2);
}
```

`render_counter` issues *semantic* render calls. Turning those into bytes
or DOM mutations is somebody else's job — the provider's.

## Two providers, one resource

A provider is just a type whose methods realize the resource. On the
server, `Sink` becomes HTML:

```kara
struct StringSink {}
impl StringSink {
    fn heading(mut ref self, title: String) { print(f"<h1>{title}</h1>"); }
    fn count(mut ref self, n: i64) { print(f"<output id=\"count\">{n}</output>"); }
    fn parity(mut ref self, p: i64) {
        if p == 0 { println("<span id=\"parity\">even</span>"); }
        else { println("<span id=\"parity\">odd</span>"); }
    }
}
```

On the client, `Sink` becomes DOM mutation. The static heading is already
present in the page the server rendered, so hydration leaves it alone —
only the dynamic values cross to the host:

```kara
effect resource Dom;
host fn dom_set_count(value: i64) with writes(Dom);
host fn dom_set_parity(value: i64) with writes(Dom);

struct DomSink {}
impl DomSink {
    fn heading(mut ref self, title: String) {}  // already in the SSR'd DOM
    fn count(mut ref self, n: i64) with writes(Dom) { dom_set_count(n); }
    fn parity(mut ref self, p: i64) with writes(Dom) { dom_set_parity(p); }
}
```

## The entry points — the *only* place `#[target]` belongs

Each target binds its provider with `with_provider`, then calls the same
component. The entry points are the one genuinely per-target part of the
program, so they — and only they — carry `#[target(...)]`:

```kara,ignore
// Server: render to HTML on stdout.
#[target(native)]
fn main() {
    with_provider[Sink](StringSink {}, || {
        render_counter(42);
    });
}

// Client: hydrate the live DOM. `pub` + a matching target tag exports it
// to JavaScript.
#[target(wasm_browser)]
pub fn hydrate(count: i64) -> i64 with writes(Dom) {
    with_provider[Sink](DomSink {}, || {
        render_counter(count);
    });
    count
}
```

Build each target from the same file:

```sh
karac build ssr_counter.kara                       # ./ssr_counter (server)
karac build ssr_counter.kara --target=wasm_browser # .wasm + .js (client)
```

The server prints the page body; the browser loads the wasm, supplies the
DOM host fns, and calls `hydrate`. One component, rendered two ways.

## The rule

> Keep `#[target(...)]` out of component bodies. The attribute is for
> entry points and irreducible forks — code that genuinely cannot exist on
> every target. Everything else is target-agnostic, and per-target
> behavior comes from the providers you bind.

This is not just style. Because the component is target-agnostic, the
compiler type-checks and effect-checks it **once per target** (see
[the design notes on cross-target compilation](https://github.com/karalang/kara/blob/main/docs/design.md#cross-target-compilation)).
A user-defined resource like `Sink` has no target affinity — it lives
wherever a provider for it does — so the same component is provably correct
on the server and the client without a single conditional.

The effect system also catches target mistakes for you. A function that
reaches a browser-only capability (say `writes(Display)`) cannot be
compiled for `native`; the compiler rejects it at the target gate and
points to the call chain — no silent misbuild, no runtime surprise.
