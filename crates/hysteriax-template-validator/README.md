# Fixed subscription validators

This internal crate compiles the fixed Mihomo and sing-box schemas with
jsonschema 0.59.0's validator macro. The server passes borrowed JSON values and
receives field paths; no generated business model drops unknown fields or changes
missing/null values. Platform rules, references, authorization, warnings and
rendering remain server responsibilities.

The existing field catalogs and `schema_transform.rs` under the server's
subscription-template module remain the source of truth. `build.rs` tracks only
those explicit inputs, validates raw/prepared schemas, writes checked artifacts
and emits macro declarations. The macro tracks its JSON inputs as well.

Keeping this crate independent of server code allows Cargo and the Docker target
cache to reuse the generated machine code when ordinary backend files change.
Schema, transformation, compiler, feature or validator-version changes can
correctly require rebuilding it.

The long-lived local Docker builder keeps Cargo's architecture-specific target
cache. CI explicitly preserves workspace artifacts in rust-cache and keys the
additional cache by validator source and Schema inputs; Cargo still verifies
artifact freshness after restore. See the action's
[workspace-cache option](https://github.com/Swatinem/rust-cache#example-usage).

`scripts/verify-subscription-schema-build-gate.py` checks application-only cache
reuse by observing Cargo's fresh artifact and unchanged rlib hash/mtime. It also
checks that valid schema changes rebuild and change verdicts, and invalid raw or
prepared schemas fail the build. The compatibility script compares the actual
crate with the frozen 1,046-case assessment corpus. The production offline
runtime diagnostic covers five platforms, repeated work and concurrency.
