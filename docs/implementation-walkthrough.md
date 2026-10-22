# Implementation Walkthrough

This document explains what is currently built, why each decision was made, and how the code works. It is a companion to the longer [platform specification](private-library-platform-spec.md).

## Current milestone: prove local streaming

The full platform will eventually handle identities, encrypted objects, member-run replicas, and discovery. None of those exist yet. The current code is an intentionally small **local streaming spike**:

```text
media file on disk
        |
        v
    plpd Rust process
        |                     
        +-- GET /       -> browser test client
        +-- GET /media  -> media bytes
        +-- HEAD /media -> media metadata only
```

The goal is to prove the most basic retrieval path before layering distributed-system concerns on top of it.

## Decisions made so far

### Rust standard library only

**Decision:** The first slice uses `std::net::TcpListener` and `std::fs::File`, with no web framework or third-party crates.

**Reason:** It keeps the protocol behavior visible while the project is still learning what its durable interface should be. A framework such as Axum may be a better choice later, but adding it before we need routing, middleware, async work, or structured errors would obscure the lesson of this slice.

**Trade-off:** The HTTP parser is deliberately small, so this is a development prototype rather than a production-ready Internet-facing server.

### A media file is supplied by the operator

**Decision:** Start the daemon with `--media <path>`.

**Reason:** There is no object store or catalogue yet. Asking the operating system for one explicit file is the smallest honest version of “retrieve media from local storage.”

**Trade-off:** The daemon can serve exactly one file and does not yet have uploads, object IDs, metadata, or permissions.

### The browser client is a separate static file

**Decision:** The client lives in `client/index.html`; Rust embeds that file into the executable at compile time with `include_str!` and serves it at `/`.

**Reason:** You can see and edit the client directly, but deployment remains one executable. This lets us test a real browser player without introducing a Node.js build chain or a separate static-file server.

**Trade-off:** This is a single-page diagnostic client, not a standalone frontend application with its own build, routing, or API layer.

### HTTP byte ranges are supported now

**Decision:** `GET /media` accepts one `Range: bytes=start-end` request and replies with `206 Partial Content`.

**Reason:** Browser players seek by requesting portions of a file. Supporting ranges early proves that a later peer-backed object reader must also support efficient random access.

**Trade-off:** Multiple ranges in one request are rejected. They are not needed for the first player and would add parsing and response complexity.

## File-by-file guide

| File | Responsibility | Read it when you want to understand |
| --- | --- | --- |
| `Cargo.toml` | Declares the `plpd` Rust binary. | How the project builds. |
| `src/main.rs` | Parses command-line options, listens for HTTP requests, serves the client, and streams a file. | The server behavior. |
| `client/index.html` | Browser UI with a native `<video>` element pointing at `/media`. | The test client. |
| `tests/media_streaming.rs` | Starts the real binary and sends real TCP/HTTP requests. | What behavior is proven. |
| `docs/private-library-platform-spec.md` | The intended distributed platform design. | Where this prototype is heading. |

## Server code, in order

### 1. Configuration

`Configuration::from_arguments` reads the command line:

```text
plpd --media C:\videos\example.mp4 --bind 127.0.0.1:8080
```

It rejects a missing media argument, a non-existent file, unknown flags, and an invalid network address. The default bind address is `127.0.0.1:8080`, which keeps this prototype local to the machine by default.

### 2. Listening for clients

`run` creates a `TcpListener` on the configured address. Every incoming TCP connection is handed to a short-lived thread, and each thread calls `handle_connection`.

This is simple and easy to follow. It is not the final concurrency model: a production node will use bounded concurrency, timeouts, request limits, and probably an async runtime.

### 3. Reading an HTTP request

`read_request` reads the request line and headers. It extracts only the values currently needed:

- the HTTP method (`GET` or `HEAD`);
- the target path (`/` or `/media`);
- the optional `Range` header.

It limits total request headers to 16 KiB so one client cannot make the prototype keep reading an unbounded header.

### 4. Routing

`handle_connection` is the current routing table:

| Request | Result |
| --- | --- |
| `GET /` | Return the browser client HTML. |
| `HEAD /` | Return the client headers without its HTML body. |
| `GET /media` | Return the full file or a requested byte range. |
| `HEAD /media` | Return media headers without file bytes. |
| anything else | Return `404 Not Found`. |

Keeping this table small is intentional. A later milestone will replace direct routes such as `/media` with library, object, and capability-aware routes.

### 5. Streaming a file

`serve_media` opens the selected file and obtains its size. It then chooses one of two responses:

- No `Range` header: `200 OK`, followed by the entire file.
- A valid range: seek to the requested starting byte, return `206 Partial Content`, and copy only the requested number of bytes.

`io::copy` moves file bytes to the TCP socket without loading the entire video into application memory. `media.take(content_length)` ensures the range path cannot accidentally send bytes beyond the requested interval.

`content_type_for` maps common extensions to media MIME types. Browsers use this header to determine how to interpret the response.

### 6. Understanding `ByteRange`

`ByteRange::parse` turns strings such as `bytes=6-10` into numeric offsets:

```text
bytes=6-10  -> start = 6, end = 10, length = 5
bytes=6-    -> start = 6, end = final byte
bytes=-500  -> final 500 bytes
```

If the request is impossible, such as a starting byte beyond the file’s end, the server returns `416 Range Not Satisfiable` and tells the client the total size.

## Browser client code

The important line in `client/index.html` is:

```html
<video controls preload="metadata" src="/media">
```

- `src="/media"` tells the browser to request the file from this same node.
- `controls` asks the browser to render its familiar play, pause, volume, and seek UI.
- `preload="metadata"` asks it to load enough information to describe the media before downloading more than necessary.

When a user drags the seek bar, a browser normally requests a byte range. That request reaches `serve_media`, which is why the range behavior and the client belong in the same first milestone.

## Tests and what they mean

`tests/media_streaming.rs` is an integration test suite. It starts the compiled `plpd` executable, gives it a temporary media file, makes real TCP connections, and asserts on its HTTP responses.

| Test | Behavior it proves |
| --- | --- |
| `serves_a_browser_player_at_the_root_url` | A browser can retrieve an HTML page with a video control that points to `/media`. |
| `streams_the_entire_media_file` | A normal `GET /media` returns `200 OK`, correct headers, and exact file bytes. |
| `streams_a_requested_byte_range_for_seeking` | A range request returns `206 Partial Content`, correct range metadata, and only the requested bytes. |
| `head_describes_media_without_sending_its_bytes` | A `HEAD` request describes media without sending its body. |

Run them with:

```powershell
cargo test
```

## How to try the current code

1. Choose a browser-compatible video file, such as an H.264/AAC MP4 or WebM.
2. From the repository root, run:

   ```powershell
   cargo run -- --media "C:\path\to\video.mp4"
   ```

3. Open `http://127.0.0.1:8080/`.
4. Play the video and drag the seek control. A successful seek exercises `Range` support.
5. Stop the process with `Ctrl+C`.

## What is intentionally not implemented

- Login, memberships, encryption, or access control.
- Uploads or a media library catalogue.
- Content-addressed object IDs or integrity hashes.
- Peer discovery, replication, caching, NAT traversal, or relays.
- Adaptive bitrate streaming or transcoding.
- A hardened HTTP implementation suitable for exposing directly to the Internet.

These omissions are deliberate. The next change should add one clearly explained capability at a time, with a matching behavioral test.

## Documentation protocol going forward

For each meaningful change, the implementation notes and the chat update will state:

1. **Goal:** the user-visible behavior we are adding.
2. **Decision:** the design selected and the main alternative not chosen.
3. **Files changed:** what moved or changed, and why.
4. **Code walkthrough:** how the new code works, including unfamiliar Rust or protocol behavior.
5. **Verification:** the exact test or manual check performed and its result.
6. **Limits / next step:** what the change does not solve yet.

