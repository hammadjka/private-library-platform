# Resource budget

**Status:** Accepted for milestone 0.1.8  
**Date:** October 28, 2026

This budget protects one home-hosted node running direct-play media. It is a
safety boundary, not a throughput promise. The values favor a small, predictable
implementation and leave headroom above milestone 0.1's five-stream verification
target. They should be revisited with measurements rather than raised on guesswork.

## Budget

| Resource | V0.1 limit | Reason | Behavior at the limit |
| --- | ---: | --- | --- |
| Complete HTTP/1 request head | 16 KiB, including request line and headers | Preserves the prototype's ceiling and leaves ample room for normal browser, cookie, and future authorization headers. | Return `431 Request Header Fields Too Large` when possible, then close the connection. |
| Header fields | 64 | Normal browser requests use far fewer; a count limit also bounds many tiny headers. | Return `431` and close the connection. |
| Header-read deadline | 10 seconds total | A legitimate header block is small. An absolute deadline prevents a client from keeping a socket by sending one byte at a time. | Close the connection; an HTTP response is not guaranteed because the request might be incomplete. |
| Accepted TCP connections | 64 | Bounds sockets, connection tasks, and request-head buffers while allowing several connections per viewer. | Stop accepting until a permit is released. The operating-system backlog may queue clients; excess clients can time out or be refused, so no HTTP response is guaranteed. |
| Active media response bodies | 16 | Bounds open media files and long-lived streaming work while leaving headroom above the five-stream milestone check. | Do not queue. Return `503 Service Unavailable`, `Retry-After: 1`, and an empty body. |
| Request body | 0 bytes | V0.1 has only bodyless `GET` and `HEAD` routes; accepting uploads would add an unbounded resource with no feature using it. | Return `413 Content Too Large` with an empty body and close the connection. |
| Transport inactivity after headers | 30 seconds without successful read or write progress | Releases stalled uploads, idle keep-alive connections, and streams whose clients have stopped reading without limiting legitimate stream duration. | Close the connection. If response headers were already sent, the body is truncated rather than replaced with another status. |
| Shutdown drain | 30 seconds | Gives short requests and buffered writes time to finish, but a movie-length wait would make shutdown unbounded. | Stop accepting immediately, drain for 30 seconds, then cancel remaining connections and bodies. |

## Lifetime rules

The connection permit starts before `accept` and remains owned until the TCP
connection closes. Releasing it when a request finishes would allow unlimited
idle keep-alive connections.

The media permit starts before the file is opened and remains owned by the
streaming response body. It is released only when the body reaches EOF, fails,
or is dropped after a client disconnect. Tower's ordinary concurrency-limit
future releases its permit when the handler produces a `Response`; that is too
early for a body that can stream for hours.

Header parsing limits and the header-read deadline belong at the Hyper
connection layer, before Axum has a request. Application middleware cannot
protect resources consumed while an incomplete request is still being parsed.

## Timeout rule

There is deliberately no total request or response deadline for media. Stream
duration and inactivity are different: a two-hour response that continually
makes progress is healthy, while a connection with no transport progress for
30 seconds is idle and can be closed. A conventional handler timeout can also
stop measuring once response headers exist, so 0.1.9 must verify the lifetime of
every timeout it installs.

## Capacity is not bandwidth

Sixteen streams do not promise that a node can deliver sixteen videos smoothly.
Players buffer in bursts, so peak outbound traffic can be much higher than the
sum of average media bitrates. The stream limit bounds server resources; the
browser and performance task in 0.1.13 will measure actual throughput. Operators
must lower the stream limit when their disk or uplink cannot sustain it.

## Configuration policy

All values remain named code constants during milestone 0.1. This keeps the
first implementation and its tests deterministic.

The following deployment-dependent values can become configuration after V0.1:

- maximum accepted connections;
- maximum active media streams;
- transport inactivity timeout; and
- shutdown drain period.

The request-head size, header count, header deadline, and zero-byte request-body
ceiling remain fixed security defaults until authentication, uploads, or a
measured compatibility problem gives us a reason to change them.

## Implementation consequences for 0.1.9

- Configure Hyper's HTTP/1 connection builder explicitly for header count,
  request-head size, and header-read timeout. Hyper 1.12 exposes `max_headers`,
  `max_header_size`, and `header_read_timeout`; the timeout requires a timer.
  Hyper was upgraded from 1.3.1 because `max_buf_size` controls buffer growth,
  not an exact request-head ceiling.
- An explicit accept loop may replace the `axum::serve` convenience function so
  the connection permit covers the full connection and Hyper parsing limits can
  be configured. The Axum router remains the request handler.
- Use non-waiting semaphore acquisition for media bodies so overload returns
  `503` instead of creating an unbounded queue.
- Carry the owned stream permit inside the response body, not merely inside the
  handler future.
- Test every overload and timeout at its actual lifetime boundary.

## Primary references

- [Hyper 1.12.0 HTTP/1 connection builder](https://docs.rs/hyper/1.12.0/hyper/server/conn/http1/struct.Builder.html)
- [Tokio semaphore](https://docs.rs/tokio/1.38.1/tokio/sync/struct.Semaphore.html)
- [Tower concurrency limit](https://docs.rs/tower/0.5.1/tower/limit/concurrency/struct.ConcurrencyLimit.html)
- [Axum 0.7.9 server and graceful shutdown](https://docs.rs/axum/0.7.9/axum/serve/struct.Serve.html)
