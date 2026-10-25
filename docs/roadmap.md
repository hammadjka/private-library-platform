# Product Roadmap

**Status:** Working roadmap

This roadmap translates the product vision into ordered, testable delivery
milestones. The product vision is the authority for sequencing: first make one
single-host family library genuinely useful, then connect multiple households,
and only then invest in replication or other distributed-system machinery.

The broader platform specification remains the long-term architecture reference.

## Submilestones

### 0.1 — Finish the transport foundation

Replace the handwritten HTTP layer with a mature Rust HTTP stack while
preserving range streaming. Add timeouts, resource limits, graceful shutdown,
structured errors, and the missing transport tests.

The review-sized implementation and learning tasks for this milestone are in
[Milestone 0.1 — Transport Foundation](milestones/0.1-transport-foundation.md).

**Exit gate:** Full-file, open-ended, suffix, invalid, and unsatisfiable ranges
pass; concurrent clients and disconnects are tested; browser playback and
seeking still work.

### 1.1 — Persistent local library

Introduce application configuration and SQLite. Allow one or more approved
folders to be scanned safely. Define stable media IDs and track available,
moved, changed, and missing files across restarts.

**Exit gate:** A folder containing multiple files can be scanned; IDs remain
stable under the documented rules; files outside configured roots cannot be
accessed.

### 1.2 — Multi-file catalog and playback API

Add a versioned local API for listing titles, reading details, rescanning
folders, and streaming a selected media ID.

**Exit gate:** A client can browse multiple files and play or seek any available
compatible file without supplying filesystem paths.

### 1.3 — Single-host browser product

Build first-run folder selection, catalog browsing, title details, playback,
rescan controls, and clear unavailable or unsupported states.

**Exit gate:** On a fresh installation, a host can select a folder and play a
compatible video through the UI without editing configuration files.

### 1.4 — Private access and invitations

Add the owner account, authenticated sessions, invitation acceptance, viewer
permissions, revocation, and short-lived media-scoped playback grants.

**Exit gate:** An invited viewer can browse and play; an unauthenticated user
cannot enumerate titles or retrieve bytes; revocation blocks new sessions and
playback grants.

### 1.5 — Installable and operable single-host release

Package the daemon and embedded web UI. Add migrations, restart behavior, logs,
health reporting, metadata backup and restore, and one documented remote-access
method using an established secure overlay.

**Exit gate:** A clean supported machine can go from installation to invited
playback, survive restart or upgrade, and recover its catalog metadata. This
completes the first compelling product.

### 2.1 — Coordinator and node enrollment

Formalize the coordinator and media-node boundary. Add node identities,
explicit enrollment, renewable heartbeats, and media availability
advertisements. Initially, the coordinator and node may still run in one
binary.

**Exit gate:** A second machine can enroll, advertise its catalog, go offline,
return, and be represented accurately.

### 2.2 — Two hosts, one catalog

Aggregate titles from both nodes, track which nodes advertise each title, and
issue playback grants that media nodes validate locally.

**Exit gate:** One viewer sees one catalog containing media from two hosts and
can play a title from either host; video bytes bypass the coordinator.

### 2.3 — Cross-household playback

Add reachability checks, simple eligible-host selection, a documented direct or
overlay network path, and understandable connectivity diagnostics.

**Exit gate:** Two households can contribute folders and stream from both under
the supported networking setup, without manual application configuration.

### 3.1 — Predictable failure and capacity behavior

Add offline-state handling, alternate-host retry where another copy exists,
quotas, concurrency limits, token renewal, metrics, and actionable operator
errors.

**Exit gate:** Offline nodes, interrupted streams, capacity exhaustion, and
coordinator interruption have tested, understandable outcomes.

### 3.2 — Relay decision and hardening

Validate whether users actually need an integrated relay. If so, add it with
bandwidth limits, abuse controls, cost visibility, and a threat-model review.

**Exit gate:** Common NAT scenarios either work through the supported direct
approach or fail over through an explicitly configured relay with measurable
cost.

## Stable subprojects

The milestones cut vertically across these ongoing areas of ownership:

1. Rust media node and HTTP transport
2. Catalog, scanner, and persistence
3. Browser application
4. Identity, invitations, and authorization
5. Coordinator and node discovery
6. Networking and reliability
7. Packaging, upgrades, backup, and observability

Milestones are user-visible product gates. Finishing one component in isolation
does not complete a milestone unless its exit gate passes end to end.

## Immediate investment sequence

1. **0.1 — Finish the transport foundation**
2. **1.1 — Persistent local library**
3. **1.2 — Multi-file catalog and playback API**
4. **1.3 — Single-host browser product**

Identity and invitation design should begin during milestone 1.2 so the API does
not require a major redesign, but implementation should not delay proving that
the multi-file local library is useful.

## Explicitly deferred

Until user evidence or a milestone exit gate demonstrates the need, defer:

- automatic replication and placement;
- a chunked content-addressed object store;
- blind replicas;
- custom NAT traversal or a custom relay protocol;
- public DHT or gossip discovery;
- transcoding and adaptive streaming;
- multiple coordinators or consensus.

Before accepting a major feature, ask whether it directly improves the path
from install to invite to browse to play, or materially improves the security
and reliability of that path.
