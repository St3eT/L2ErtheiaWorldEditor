# unreal-rs

`unreal-rs` is a Rust CLI that extracts Unreal Engine 2 `.unr` map and `.ukx`
animation packages to
readable, indented XML or JSON. It writes the package header, complete name/import/export
tables, tagged export properties, and hex-encoded raw or unread export data so
unsupported native structures are not silently discarded.

Lineage II `Lineage2Ver111` and `Lineage2Ver121` XOR encryption is handled
automatically. The tool does not need Java or `L2.ini`.

```sh
cargo build --release
target/release/unreal-rs unr export --input /path/to/map.unr --output map.xml
target/release/unreal-rs unr export --input /path/to/LineageII/MAPS --output all-maps.json
target/release/unreal-rs unr export --input /path/to/map.unr --output decoded-map --format json
target/release/unreal-rs unr import --input decoded-map --output rebuilt.unr
target/release/unreal-rs ukx export --input /path/to/animations.ukx --output animations.xml
target/release/unreal-rs ukx export --input /path/to/LineageII/ANIMATIONS --output all-animations.json
target/release/unreal-rs ukx import --input /path/to/decoded-folder --output rebuilt.ukx
target/release/unreal-rs ukx object export --input source.ukx --object Grim_reaper_m00 --output mesh.json
target/release/unreal-rs ukx object add-export --input target.ukx --bundle mesh.json --output modified.ukx
target/release/unreal-rs ukx object add-import --input target.ukx --path MonSound6.extra.hit --class Engine.Sound --output modified.ukx
target/release/unreal-rs ukx object remove-export --input target.ukx --object Grim_reaper_m00 --output modified.ukx
target/release/unreal-rs ukx object remove-import --input target.ukx --object MonSound6.extra.hit --output modified.ukx
target/release/unreal-rs decrypt --input encrypted.package --output decrypted.package
target/release/unreal-rs encrypt --input decrypted.package --output encrypted.package
```

## 3D map viewer

The optional Bevy viewer loads terrain and locally embedded static meshes from
an `.unr` package:

```sh
cargo run --release --features viewer -- unr view --input /path/to/map.unr
```

When the map is inside a Lineage II `Maps` directory, the viewer automatically
loads referenced `.usx` packages from the sibling `StaticMeshes` directory. For
a standalone map, add one or more explicit search paths:

```sh
cargo run --release --features viewer -- unr view \
  --input /path/to/map.unr --assets /path/to/LineageII/StaticMeshes
```

Use `W`, `A`, `S`, and `D` to move, `Q` and `E` to descend and ascend, hold the
right mouse button to look around, and hold left Shift to move faster.

The viewer renders decoded `Engine.TerrainInfo` geometry with vertex colors and
instances of embedded or externally loaded `Engine.StaticMesh` objects. Static
mesh sections resolve their UE2 materials through `.utx` packages and support
BGRA8, DXT1, DXT3, and DXT5 textures, alpha masking, and two-sided materials.
The scene uses a bright sun, cool fill light, and ambient light while original
map lights remain unsupported. It reports packages and mesh objects that remain
unresolved. BSP model geometry and emitters are not implemented yet. Bevy is
optional so ordinary export, import, object editing, encryption, and decryption
builds do not pull in the renderer.

## Workspace layout

The project is split into independently cached internal crates:

- `unreal-package` owns the binary codec, package document, native object
  decoding and encoding, encryption, and object-table editing.
- `unreal-viewer` owns Bevy scene construction and rendering and depends only on
  `unreal-package`.
- `unreal-rs` is the root CLI crate and keeps command parsing and JSON/XML file
  adapters separate from both implementations.

Ordinary `cargo build` and `cargo test` target the lightweight CLI and package
crates and do not compile Bevy. Use `cargo test --workspace` to test every
internal crate, or `cargo test --features viewer` to test the installed CLI
configuration.

`encrypt` writes the `Lineage2Ver111` format. `decrypt` recognizes both
`Lineage2Ver111` and filename-keyed `Lineage2Ver121` files. These commands
inspect file contents and do not restrict filename extensions.

The output format is inferred from `.xml` or `.json`. Use `--format xml` or
`--format json` only when the output filename has a different extension.

Passing a Lineage II installation directory automatically selects its `MAPS` or
`ANIMATIONS` subdirectory for the chosen command. When a directory is passed,
every file with the corresponding extension is included beneath one
`<unrealPackages>` document root.

Directory exports are self-contained. For every decoded package, the manifest's
`wireResource` points to an exact byte-for-byte copy under `wire/`, and
`wireSize` records its length. This wire source preserves details that readable
JSON or XML necessarily normalizes, including the encryption wrapper, package
GUID and generations, string and compact-integer encodings, table layout,
padding, and original native payloads. A package importer can therefore reuse
unchanged spans exactly and rebuild only values edited in the readable export.

`unr import` and `ukx import` accept a JSON directory export containing this
wire source. An
unchanged export reproduces the input byte-for-byte. Edited tagged properties,
`Engine.MeshAnimation`, and `Engine.SkeletalMesh` objects are encoded into new
export payloads, with export-table and absolute lazy-array offsets recalculated.
For maps this covers actor properties such as `Location`, `Rotation`, and
`ColLocation`; native map objects (level, BSP model, terrain) can be preserved
but not yet re-encoded after edits. Property `value` edits support scalar numbers, object/name references, vectors,
rotators, and strings; `raw` remains available for other property encodings.
Names and object paths used by edits must already exist in the package tables.

The `ukx object` commands operate directly on packages. `object export` writes a
portable JSON bundle containing the selected export, recursively referenced
exports, required imports, and name-table vocabulary. `add-export` adds missing
dependencies before the selected object and remaps typed references by object
path. It also accepts a JSON resource from a directory export when that resource
is kept alongside its `manifest.json` and preserved `wire/` directory. `--name`
can assign a new root-export name and is required when the original object name
already exists in the target. `add-import` creates missing `Core.Package` outer
entries for a dotted path.

Removal is deliberately index-stable: after proving that no table metadata,
tagged property, or decoded native object references the target, the command
replaces its table entry with an inert `None` tombstone. This prevents unrelated
compact object indices from shifting. Removal refuses referenced objects and
packages containing opaque data where absence of a reference cannot be proven.
