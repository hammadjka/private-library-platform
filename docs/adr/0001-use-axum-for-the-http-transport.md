---
status: accepted
date: 2026-10-25
---

# Use Axum for the HTTP transport

The prototype proved that one local file can be streamed and sought, but its handwritten HTTP/1.1 parser and thread-per-connection server are not a suitable foundation for a private media product. We will use Axum over Tokio and Hyper, with Tower middleware and Tracing diagnostics, because this removes protocol implementation work while keeping the application interface small, testable, and compatible with the broader Tokio ecosystem.

## Decision drivers

- Prefer development speed and simple application code over custom transport machinery.
- Delegate HTTP parsing, message framing, connection handling, and protocol correctness to a mature implementation.
- Stream large files with backpressure instead of buffering them in memory.
- Add timeouts, limits, graceful shutdown, and request diagnostics through established abstractions.
- Test routes in process while retaining end-to-end executable tests.
- Preserve an inexpensive reversal path by keeping media and range rules free of framework types.

## Decision

Use the following responsibilities:

- **Axum** owns routing, request extraction, application state, handlers, and conversion of application results into HTTP responses.
- **Tokio** owns the asynchronous runtime, network I/O, scheduling, timers, signals, and asynchronous I/O adapters used by the server.
- **Hyper**, reached through Axum, owns HTTP parsing, connection state, and HTTP body transport.
- **Tower and Tower HTTP** own composable transport middleware such as tracing, concurrency limits, request-body limits, and carefully selected timeouts.
- **Tracing** owns structured events and spans for operational diagnostics.

The executable will have one Tokio runtime rather than one operating-system thread per connection. Network waits and response streaming will suspend async tasks so runtime threads can serve other work. Blocking or CPU-heavy work must not run directly on async runtime threads. Media bodies will be streamed with backpressure and bounded to the selected byte range; they will not be loaded entirely into memory.

Keep the byte-range selection rules in a small framework-independent module. It should accept a range value and representation length and return a decision; it should not know about Axum requests, Hyper bodies, sockets, or filesystem paths. `main.rs` remains responsible for CLI configuration and process lifecycle, while `server.rs` becomes the HTTP adapter.

Use only the dependency features the application needs rather than enabling every optional feature by default.

## Considered options

| Option | Strengths | Costs for this project | Decision |
| --- | --- | --- | --- |
| Raw `std::net` | No third-party dependencies; makes the mechanics visible; already proves the initial slice. | We own HTTP parsing, framing, connection behavior, limits, and many security-sensitive edge cases. Blocking sockets and one thread per connection make bounded concurrency and graceful shutdown harder. | Retire after the Axum path has equivalent behavior. Keep it only as learning history in Git. |
| Hyper directly | Mature HTTP implementation with low-level control and no application-framework policy. | We would still assemble the runtime adapter, accept loop, routing, extraction, response conversion, middleware integration, and test conveniences ourselves. This is more plumbing than the product needs. | Use indirectly through Axum; drop down to Hyper types only for a streaming detail Axum cannot express cleanly. |
| Axum + Tokio + Hyper | Small routing and handler interface; typed extractors; predictable response conversion; direct Tower compatibility; in-process router testing; access to Hyper when needed. | Introduces the async ecosystem, additional dependencies, longer builds, `Send`/lifetime constraints, and cancellation semantics that contributors must learn. | Selected. |
| Actix Web | Mature, pragmatic framework with routing, application state, middleware, HTTP serving, and production use; the actor framework is not required for ordinary HTTP. | It introduces a separate application and middleware model from the Tower-based stack already suited to the planned limits and observability work. Switching would not improve a demonstrated requirement enough to justify that divergence. | Credible fallback, not selected. |

## Consequences

Positive consequences:

- The project stops maintaining an HTTP parser before exposing the server to untrusted or slow clients.
- Later API routes can share typed state, errors, middleware, and tests.
- Network concurrency no longer requires an operating-system thread for every connection.
- Tower provides common composition points for the resource-budget work in milestone 0.1.

Costs and cautions:

- Async Rust adds futures, task cancellation, ownership across `.await`, and runtime-aware testing to the contributor learning path.
- Middleware names are not guarantees: each timeout or limit must be verified to determine whether it covers headers, handlers, or streaming bodies.
- Ordinary filesystem access is not made magically non-blocking by an async function; file streaming must use the runtime's supported adapter without blocking network executor threads.
- The dependency graph and compile time will grow.
- Performance claims are not a selection criterion. The browser and concurrency checks later in milestone 0.1 will measure the actual application.

## Reversal path

Keep CLI/configuration, byte-range selection, media selection, and future storage interfaces independent of Axum. Preserve the executable-level HTTP tests as the transport contract. If Axum later blocks a demonstrated requirement, replace `server.rs` with a Hyper-direct or Actix Web adapter while reusing those modules and tests. This makes the framework replaceable without pretending that a second adapter is needed today.

## References

- [Axum crate documentation](https://docs.rs/axum/latest/axum/)
- [Tokio tutorial](https://tokio.rs/tokio/tutorial)
- [Hyper server guide](https://hyper.rs/guides/1/server/hello-world/)
- [Tower overview](https://docs.rs/crate/tower/latest)
- [Tower HTTP middleware](https://docs.rs/tower-http/latest/tower_http/)
- [Actix Web overview](https://actix.rs/docs/whatis/)
- [`std::net::TcpListener`](https://doc.rust-lang.org/std/net/struct.TcpListener.html)
