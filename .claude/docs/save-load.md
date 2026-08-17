# Save / Load (`src/io.rs`)

The `.osm` on-disk format. Depends only on `sim` types plus plain-data GUI geometry
types, never on `OsmilogApp` (see the layering rule in [architecture.md](architecture.md)).

The top-level on-disk unit (v3) is `ProjectFile { version, active: usize, circuits:
Vec<CircuitEntry> }` — the whole workspace, every circuit document, not just the active
one, so subcircuits round-trip. Each `CircuitEntry { name, #[serde(flatten)] snapshot:
CircuitSnapshot, subcircuits: Vec<SubcircuitRef> }` names one document and carries its
records as a `CircuitSnapshot { components: Vec<ComponentEntry>, tunnels:
Vec<TunnelEntry>, nodes: Vec<NodeEntry>, segments: Vec<SegEntry> }` — one document's
placed components/tunnels and `Wiring` graph, not `Circuit`'s internal maps.

Every cross-reference is a plain `usize` index into one of the snapshot's own vectors;
the in-memory keys (slotmap-generational for nets, app-assigned `u64` elsewhere) are not
persisted. `CircuitSnapshot` is also the payload the clipboard (`gui::clipboard`)
holds.

A placed subcircuit's `ComponentSpec::Subcircuit::doc` (a runtime-only,
`#[serde(skip)]` `DocId`) is emitted as a `SubcircuitRef { component, circuit }`
(component index within the entry, circuit index within the project) and re-bound to a
freshly-allocated `DocId` on load. `active` records which document was open.

`version` bumps on every incompatible shape change. `ProjectFile::validate()` checks
version, `active`, and every index bound before a load replaces the current app state.
`ProjectFile::from_json` upgrades a legacy v2 single-circuit file (deserialized via
`io::LegacyV2File`, a versioned `CircuitSnapshot`) into a one-circuit project named
"Main".

The App↔file conversion lives in `gui::app`
(`to_project_file`/`load_project_file`, plus `extract_records`/`install_circuit_records`
shared with the single-doc `to_snapshot`/`load_snapshot` helpers). Native and WASM have
separate submodules (`platform::native`, `platform::web`) for file I/O; both take and
return `ProjectFile` and stay `OsmilogApp`-agnostic.
