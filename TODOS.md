# Features

- [ ] Implement patch morphing
- [ ] Add debug for the patch AABBs
- [ ] Builds config.hlsli from the terrain/config.rs
- [ ] Upload budget
- [ ] Culling in the quad tree
- [ ] Make adaptive ATLAS_SLOT_COUNT_PER_SIDE to the PATCH_LOD_COUNT and PATCH_SIZE_IN_METERS
- [ ] PatchCoords could be packed to reduce QuadTree size
- [ ] Move sun using mouse
- [ ] Quad tree tests

# Refactor | Fix

- [ ] Make separate terrain::Renderer from terrain::Instance
- [ ] Fix that direction in the worls is opposite to the direction in the quad tree minimap
- [ ] Move all DX12 code to the separate gpu mod

# Global

- [ ] Replace the per-patch println! at generator.rs:153 with counters in the ImGui panel — generation ms histogram, queue depth, patches/sec, cache hit rate, eviction count
- [ ] Basic shadow mapping
    - [x] Move sun direction to CPU
    - [x] Create shadow map resource
    - [ ] Compute light view-projection matrix
    - [ ] Shadow depth pass
    - [ ] Sample shadow map in pixel shader
    - [ ] Fix shadow acne
    - [ ] Debug view + PCF
    - [ ] Stabilization
- [ ] Terrain generation
    - [ ] Move heights generations into compute shader
- [ ] Material support. Triplanar, Splatting
- [ ] Water rendering
- [ ] Sky + atmospheric scaterring

