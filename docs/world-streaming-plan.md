# Whole-world streaming viewer: plan

Goal: fly seamlessly across the whole Lineage II world instead of opening one
`XX_YY.unr` tile at a time, by streaming the tiles around the camera.

## Tile grid

- Every `XX_YY.unr` tile is 32768 UU (256 m) square. `TerrainInfo.Location` is
  the tile centre, e.g. 24_18 → (147456, 16384), 25_18 → (180224, 16384),
  24_19 → (147456, 49152).
- `tile_x = 20 + floor(ue_x / 32768)`, `tile_y = 18 + floor(ue_y / 32768)`.
  The bounds of tile (tx, ty) are `[(tx-20)*32768, (tx-19)*32768)` ×
  `[(ty-18)*32768, (ty-17)*32768)`. This matches `radar_map_name`.
- Today each map re-centres on its own first terrain vertex (`scene.origin`).
  Streaming needs one session-wide `WorldOrigin` instead. The world spans about
  ±3.6e5 UU; f32 spacing there is about 0.03 UU, so no floating origin is needed.
- Seam risk: a tile's last terrain vertex sits at corner + 255×128, which may
  leave a one-quad (128 UU) gap between tiles. Check this visually and stitch
  from the neighbour's first row or column if needed.

## Structure

- **`AssetLibrary`** (shared with worker threads): a package index built once,
  plus decoded meshes, textures and materials keyed by qualified name. These
  are already name-keyed, so they can be shared across tiles.
- **`GpuAssetCache`** (main thread): image, mesh and material handles. These are
  the per-`setup` maps hoisted out of `presentation.rs`.
- **`TileContent`**: the per-tile half of `MapScene` (terrain, instances,
  lights, effects, decals, BSP, sky, environment).
- **`TileId(i8, i8)`**: a component on every spawned entity, so a tile can be
  despawned as a unit.
- **`LoadedTiles`**: a map from `TileId` to `Loading(Task)`, `Spawning(cursor)`
  or `Ready { entities, editor }`.

## Loading

- Decoding 24_18 takes about 6 s optimized. Run `Package::read`, `dump`,
  `from_exports` and the external-asset step on `AsyncComputeTaskPool`.
- Spawn on the main thread within a per-frame budget.
- Extract `spawn_tile_content`, covering terrain, instances, BSP, particles,
  decals and lights.
- These stay global: the camera, the UI, `SelectionState` and the time of day.
- The sky, sun, ambient light and fog follow the tile the camera is in.

## Policy

- Keep a 3×3 block of tiles around the camera, with hysteresis: load within
  0.6 tile of a neighbour, unload beyond 1.6 tiles. Run at most 2 concurrent
  decodes.
- Evict unreferenced shared assets with an LRU cache. After upload, drop CPU
  texture copies; meshes stay in the main world for picking.
- The world map (M) teleports the camera instead of restarting the viewer.

## Editor

- Use `MapEditors(HashMap<TileId, MapEditor>)`, because export indices are only
  unique within one package.
- Selection, drag and save go to the editor of the tile that owns the entity.
- Never unload a tile that has unsaved changes.

## Phases

| Phase | Work | Estimate |
|---|---|---|
| 1 | `tile_of` / `tile_origin` helpers with tests | ~0.5 day |
| 2 | `WorldOrigin` replaces every `scene.origin` use; single-map output stays identical | ~1 day |
| 3 | Split `MapScene` into `AssetLibrary` + `TileContent`; cache the package index | ~2 days |
| 4 | `spawn_tile_content` + `GpuAssetCache` + `TileId`; Startup spawns one tile through it | ~2 days |
| 5 | Async decode plus budgeted spawn, with a loading indicator | ~1.5 days |
| 6 | Streaming manager (3×3, hysteresis, despawn, handle pruning) | ~2 days |
| 7 | Environment follows the camera tile; world map teleports | ~1 day |
| 8 | `MapEditors` per tile; tiles with unsaved changes are pinned | ~1 day |
| 9 | Terrain seam stitching and the memory cap | ~1.5 days |

## Risks

- Main-thread hitches while uploading about 3000 images per tile.
- RAM for 9 tiles of textures.
- Terrain seams.
- Sky and fog popping at tile borders.
- Clustered-light capacity.
