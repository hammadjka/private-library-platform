# Private Library Platform

An open-source, self-hosted foundation for private groups to share and replicate media and files.

## Current slice: stream one local media file

The initial Rust daemon serves one local media file over HTTP with full-file playback, `HEAD`, and single byte-range requests for seeking.

```powershell
cargo run -- --media "C:\path\to\video.mp4"
```

Open `http://127.0.0.1:8080/` in a browser to use the built-in test player. The raw media stream remains available at `http://127.0.0.1:8080/media`, which is useful for testing another media player. To select another local address or port:

```powershell
cargo run -- --media "C:\path\to\video.mp4" --bind 127.0.0.1:9000
```

The browser client source is [`client/index.html`](client/index.html). The daemon embeds and serves it at `/`.

For architecture diagrams, the complete request lifecycle, and a function-by-function guide to the current implementation, read [docs/implementation-walkthrough.md](docs/implementation-walkthrough.md).

## Verify

```powershell
cargo test
```

Generate browsable documentation for the production modules and their private
implementation details with:

```powershell
cargo doc --no-deps --document-private-items --open
```

The platform design is in [docs/private-library-platform-spec.md](docs/private-library-platform-spec.md).
The ordered product milestones and active investment sequence are in [docs/roadmap.md](docs/roadmap.md).

## Status

This is an early local-streaming foundation. Identity, private groups, peer replication, authorization, and encryption are planned work and are not implemented yet.

