# Save / Load (`src/io.rs`)

The `.osm` on-disk format. Depends only on `sim` types plus a couple of plain-data GUI geometry
types - never on `OsmilogApp` (see the layering rule in [architecture.md](architecture.md)).

The top-level on-disk unit (v3) is `ProjectFile { version, active: usize, circuits:
Vec<CircuitEntry> }` - the *whole workspace*, every circuit document, not just the active one -
so subcircuits round-trip (see below). Each `CircuitEntry { name, #[serde(flatten)]
snapshot: CircuitSnapshot, subcircuits: Vec<SubcircuitRef> }` names one document and carries its
records as a `CircuitSnapshot { components: Vec<ComponentEntry>, tunnels: Vec<TunnelEntry>, nodes:
Vec<NodeEntry>, segments: Vec<SegEntry> }` - one document's GUI visual state (placed
components/tunnels + the `Wiring` graph), not `Circuit`'s internal maps - every
cross-reference a plain `usize` index into one of the snapshot's own vectors, since the in-memory
keys (slotmap-generational for nets, app-assigned `u64` elsewhere) are ephemeral and not worth
persisting. `CircuitSnapshot` is the reusable payload the clipboard
(`gui::clipboard`) also holds. That indexing convention is exactly how cross-*circuit* links persist too: a
placed subcircuit's `ComponentSpec::Subcircuit::doc` (a runtime-only, serde-skipped `DocId`) is
emitted as a `SubcircuitRef { component, circuit }` (component index within the entry → circuit
index within the project) and re-bound to a freshly-allocated `DocId` on load. `active` records
which document was open.

`version` is bumped whenever the shape changes incompatibly; `ProjectFile::validate()` checks
version, `active`, and every index bound before a load replaces the current app state.
`ProjectFile::from_json` transparently upgrades a legacy **v2** single-circuit file (deserialized
via `io::LegacyV2File`, a versioned `CircuitSnapshot`) into a one-circuit project named "Main". The
App↔file conversion lives in `gui::app` (`to_project_file`/`load_project_file`, plus
`extract_records` / `install_circuit_records` shared with the single-doc `to_snapshot`/`load_snapshot`
helpers). Native and WASM get separate
submodules (`platform::native`, `platform::web`) for the actual file I/O, since blocking `rfd`
dialogs and browser Promise-based APIs are different enough mechanically to not share one
`#[cfg]`-sprinkled function; both stay `OsmilogApp`-agnostic (they take/return `ProjectFile`, not
app state).
