# Private Library Platform

**Status:** Draft v0.1  
**Audience:** initial maintainers and application developers  
**Implementation language:** Rust for the node and protocol libraries

## 1. Summary

Private Library Platform (PLP) is an open-source, self-hosted foundation for private groups to store, replicate, and retrieve shared media and files without relying on a permanent central server.

A group operates one or more nodes. Members choose which nodes store which data. Any authorized node may serve content to an authorized viewer, so a library can remain available when another member’s home server is offline. PLP supplies the peer, storage, identity, encryption, replication, and retrieval layers. Applications supply the user experience and domain model.

The first reference application is a private video library. It demonstrates upload, catalogue metadata, access control, multi-node replication, and byte-range streaming. PLP must also support non-video applications, such as a shared document archive, without video concepts in its core model.

## 2. Problem

Today, a private group that wants to share a media library usually chooses one owner-operated media server. That server is a single availability, bandwidth, and administration point. File synchronization tools can copy the media between members, but do not provide one shared library model, streaming-aware retrieval, permissions, or durable application metadata.

PLP addresses this gap: a group should be able to jointly operate a library while retaining control of its data, membership, storage allocation, and infrastructure.

## 3. Goals

- A group can create an invite-only library and add member-operated nodes.
- A member can upload immutable binary content and attach mutable application-defined metadata.
- A group can select which nodes replicate which collections and set storage/bandwidth limits.
- An authorized client can retrieve a complete object or byte ranges from an authorized available node.
- Replicas verify content integrity before serving it.
- A library remains usable if some nodes are offline.
- Nodes can connect across typical home networks, using an optional relay when direct connectivity fails.
- Applications can use a stable local API and SDK without implementing peer discovery, encryption, or replication.
- A minimum deployment works with one node; additional nodes increase availability rather than being mandatory.

## 4. Non-goals for v0.1

- A global public catalogue, global search, recommendations, or anonymous public publishing.
- Financial incentives, tokens, proof-of-storage, or a marketplace for capacity.
- Censorship-resistant distribution or an attempt to evade lawful removal processes.
- Hiding all network metadata from other members or from a network observer.
- Perfect revocation of bytes a former member has already downloaded.
- A distributed SQL database or arbitrary cross-node transaction engine.
- Transcoding, recommendation ranking, comments, subscriptions, or social features in the core platform.
- Automatic replication of every object to every node.

## 5. Terms

| Term | Meaning |
| --- | --- |
| **Library** | A private namespace governed by a membership and policy record. |
| **Member** | A person or service identity permitted by the library policy. |
| **Node** | A process operated by a member that stores, replicates, and/or serves library data. |
| **Object** | Immutable bytes addressed by their content identifier (CID). |
| **Record** | A signed mutable, application-defined document that refers to objects or other records. |
| **Collection** | An application-visible grouping of records and objects with a replication policy. |
| **Capability** | A narrowly scoped, revocable authorization presented to a node. |
| **Replica** | A node that has a verified local copy of an object or encrypted object chunks. |
| **Relay** | An optional network service that forwards encrypted transport traffic when direct peer connections are unavailable. It is not a library authority. |

## 6. Product model

Each library owns:

1. A public library identifier and a root signing key.
2. A membership policy, signed by a library administrator or a defined quorum.
3. One or more collections.
4. Object manifests and application records.
5. A membership-scoped peer directory.

The platform has two data planes:

```text
Control plane
  membership, invitations, policies, records, replica advertisements

Data plane
  encrypted object chunks, range reads, verification, cache and replication traffic
```

The control plane is deliberately small. It identifies content and permissions; it does not carry video or file bytes.

## 7. Data model

### 7.1 Immutable objects

An object is chunked before storage. The object CID is calculated from a canonical manifest that includes the ordered list of chunk hashes, total size, content type, and format version. A node must verify each received chunk hash and the final manifest before marking an object complete.

```text
ObjectManifest
  version
  object_cid
  content_type
  total_size
  chunk_size
  chunk_hashes[]
  encryption_descriptor
```

`object_cid` is the durable technical identifier. A file name, title, or video slug is never an object identifier.

### 7.2 Records

Records hold mutable meaning. The core validates the record envelope but treats the payload as application data.

```text
RecordEnvelope
  record_id                 // stable random identifier
  library_id
  collection_id
  schema_uri                // e.g. plp.video/v1
  revision                  // monotonic per record author
  author_identity
  previous_revision_hash
  payload
  signature
```

Applications define schemas. A video application might create a `Video` record with `title`, `description`, `published_at`, `object_cid`, `thumbnail_cid`, captions, and visibility rules. A title alias such as `videos/my-holiday-film` resolves to a record ID, not directly to bytes.

### 7.3 Conflict behavior

v0.1 permits one writable authority for each record or alias. Concurrent writes from the same authority resolve by revision and signed hash chain. Concurrent writes from different authorities are rejected unless an application schema explicitly defines a merge rule.

This is intentionally conservative. Collaborative CRDT-style editing is deferred until a reference application needs it.

## 8. Identity, authorization, and encryption

### 8.1 Identities

Every person and node has a long-lived public/private signing identity. Nodes also maintain transport keys that may rotate without changing their member identity.

Libraries define these roles:

- **Owner:** creates the library; may delegate or recover administration.
- **Administrator:** manages membership, policies, and collection configuration.
- **Publisher:** creates application records and uploads objects in designated collections.
- **Viewer:** reads authorized records and objects.
- **Replica operator:** runs a node approved to store particular collections; this can be combined with other roles.

### 8.2 Access enforcement

Nodes authenticate a client or peer before disclosing records, provider information, or object bytes. Access is granted through short-lived signed capabilities containing at least:

```text
library_id, subject, allowed_actions, collection scope,
object/record scope when applicable, expiry, nonce, issuer signature
```

Capabilities are verified at every serving node. Nodes cache revocation data for a bounded period and revalidate it before renewing a long-lived stream.

### 8.3 Encryption model

All network transport is encrypted and mutually authenticated. Library content is encrypted before it is replicated to a node that is not authorized to read plaintext. Keys are scoped at least to a library and may later be scoped to a collection or object.

v0.1 supports two replica modes:

- **Trusted replica:** may decrypt and stream its assigned collection.
- **Blind replica:** stores and transfers ciphertext but cannot decode it.

An application client obtains decryption material only after authorization. The exact cryptographic suite is an implementation decision, selected from well-reviewed Rust cryptography libraries; it must be documented in a security design before a production release.

### 8.4 Revocation limits

Removing a member blocks new authorization and rotates affected keys for future content. It cannot make a previously viewed, downloaded, or decrypted object disappear from that member’s devices. The UI and documentation must state this plainly.

## 9. Node architecture

`plpd` is the Rust daemon. It exposes a loopback-first local API and optionally an authenticated remote administrative API.

```text
Applications / clients
          │ local HTTP or gRPC
          ▼
        plpd
  ├─ API and authorization service
  ├─ record store and policy engine
  ├─ local content-addressed object store
  ├─ replication scheduler
  ├─ peer directory and connection manager
  ├─ encrypted transport and optional relay client
  ├─ range-read / streaming service
  └─ observability and administration
```

The node stores local indexes and queues in SQLite. SQLite is an implementation detail, not a shared database: nodes exchange signed records and verified objects over the protocol.

Storage must be abstracted behind an interface so v0.1 supports a local filesystem first and can later support NAS or object-store backends.

## 10. Replication

### 10.1 Policies

Collections contain a replication policy. Initial policy forms:

- **Owner only:** retain data only on upload nodes.
- **Named replicas:** retain complete copies on explicitly selected nodes.
- **Replication factor:** seek `N` eligible replicas from a selected member group.
- **Viewer cache:** retain recently streamed chunks subject to a local quota and eviction policy.

Policies are goals, not guarantees. A policy is healthy only if enough approved nodes are online, have capacity, and have verified the intended data.

### 10.2 Scheduling

The scheduler considers policy priority, object size, node free space, observed bandwidth, online status, and replica count. It should prefer a complete object replica before creating partial caches. A node may decline new assignments based on local quotas.

### 10.3 Provider discovery

Peers advertise availability only within their library’s authorized peer directory. The platform must not expose object CIDs or provider locations to a public DHT in v0.1. A client ranks eligible providers using reachability, recent success, measured throughput, and latency; geographic proximity is only a weak hint.

## 11. Retrieval and video streaming

The object service supports:

- Object manifest retrieval.
- Verified chunk retrieval.
- Byte-range requests.
- Resuming interrupted transfers.
- Selecting a new authorized provider when a provider disconnects.

For the video reference application, the first implementation may serve original files with HTTP range support. The client seeks by requesting relevant ranges. Transcoding and adaptive bitrate HLS/DASH packaging are application-level follow-on work. Media manifests, thumbnails, captions, and transcoded derivatives are ordinary PLP objects.

## 12. Application API

The first API surface is local and versioned. It may use HTTP/JSON for ease of adoption; a typed Rust client crate is provided alongside it.

Minimum operations:

```text
POST   /v1/libraries
POST   /v1/libraries/{library_id}/invites
POST   /v1/libraries/{library_id}/members
POST   /v1/libraries/{library_id}/collections

POST   /v1/objects                 // resumable upload; returns CID when finalized
GET    /v1/objects/{cid}
GET    /v1/objects/{cid}/ranges

POST   /v1/records
GET    /v1/records/{record_id}
GET    /v1/collections/{id}/records

PUT    /v1/collections/{id}/replication-policy
GET    /v1/replication/status
GET    /v1/nodes/status
```

The API returns capabilities or short-lived stream URLs rather than a globally reusable object URL. An API specification, error taxonomy, pagination behavior, and idempotency rules are required before external application developers depend on it.

## 13. Operational requirements

- Single-binary deployment for Linux, macOS, and Windows; ARM Linux is a release target.
- Safe upgrades with a database migration backup and a documented rollback path.
- A web or CLI administration flow for first-node setup, invite creation, member removal, storage limits, and health inspection.
- Structured logs, health endpoint, and metrics for available storage, replication backlog, transfer rates, provider failures, and key/policy expiry.
- No telemetry by default. Optional telemetry must be explicit and must not include library names, object identifiers, or member identities.
- Clear export and recovery procedures for library administration keys and membership state.

## 14. Security and abuse posture

PLP is infrastructure for private groups, not a hosted content service. Operators remain responsible for what they choose to store and serve. The project will provide a code of conduct, security-reporting process, documentation on safe deployment, and facilities for group administrators to remove content and members from their own libraries.

Before a public beta, conduct a focused threat-model review covering compromised members, compromised nodes, malicious replicas, replayed records, leaked capabilities, denial of service, invitation theft, metadata leakage, and key loss.

## 15. Milestones

### M0 — local foundation

- Rust workspace with `plpd`, protocol crate, storage crate, and client crate.
- One-node library creation, identities, filesystem object store, content verification, and local API.
- Upload and retrieve arbitrary objects by CID.

**Exit criteria:** a user can restart the daemon and retrieve an uploaded object with its hash verified.

### M1 — private group and records

- Invitation-based member enrollment.
- Signed library policies and record envelopes.
- Publisher/viewer roles and collection permissions.
- Video reference app can add title metadata pointing to uploaded objects.

**Exit criteria:** an unauthorized local client cannot enumerate or retrieve library data.

### M2 — two-node replication

- Authenticated node pairing and private peer directory.
- Named-replica policy, resumable encrypted transfer, integrity checks, and replica status.
- Node removal and revoked access for future requests.

**Exit criteria:** after the uploader is offline, an authorized member can retrieve a verified replica from the second node.

### M3 — watchable media

- HTTP byte-range streaming from a selected replica.
- Provider fallback during playback or retry.
- Storage quotas and viewer cache eviction.
- Minimal web reference application for upload, catalogue browsing, and playback.

**Exit criteria:** a supported browser can seek through a video held only by a remote authorized replica.

### M4 — real-world connectivity and hardening

- NAT traversal and configurable encrypted relay support.
- Key backup/recovery workflow, audit log, observability, failure tests, and security review.
- Packaging and installation documentation for supported self-hosting targets.

**Exit criteria:** a small geographically distributed test group can operate a library without manual port forwarding in the common case.

## 16. Decisions deferred

- Exact peer transport and discovery implementation.
- Exact cryptographic algorithms and key hierarchy.
- Governance and recovery model: single owner, threshold administrators, or both.
- Whether blind replicas ship in v0.1 or immediately after the trusted-replica path.
- Record schema registry and schema evolution policy.
- Relay operation model and whether the project ships a hosted relay directory.
- Licensing choice and contributor governance.

## 17. Success measures for the first beta

- A non-technical group can create a library and add two nodes without editing configuration files.
- A 5 GB test video replicates from one home node to another and verifies correctly after a restart or interrupted transfer.
- Removing a viewer prevents new metadata and stream access within the documented propagation window.
- Playback continues from a replica after the original publisher node goes offline.
- A separate sample file-library application can use objects, records, and replication without importing video-specific types.

