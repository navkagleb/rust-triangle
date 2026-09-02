# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Windows-only Direct3D 12 renderer built from scratch in Rust: a quadtree-based clipmap/CDLOD-style terrain
system, rendered through raw D3D12 calls (via `windows-rs`), with a Dear ImGui debug UI and HLSL shaders
compiled offline with `dxc`.

This is a personal learning project: the author is a professional 3D programmer using it to learn Rust and
to build a procedural terrain renderer on DX12. Favor idiomatic-but-explicit Rust and explanations that
connect to the underlying D3D12/GPU concepts over abstracting them away.

### End goal

Fully GPU-driven terrain rendering, in the style of the Far Cry 5 GDC talk (GPU-driven geometry clipmaps).
Getting there is deliberately incremental: implement the basics CPU-side first, iterate/improve there, then
progressively move pieces of the pipeline onto the GPU. Rust itself is also being learned along the way, so
prefer the CPU/simpler approach when it's also a good opportunity to learn a Rust concept, rather than
jumping straight to the most GPU-driven design.

## Architecture

Cargo workspace with two crates:

- `crates/app` — the executable. Win32/D3D12 setup and frame loop, the free-fly camera, and the terrain
  system (CPU quadtree LOD selection, GPU patch streaming/caching, texture atlases, HLSL shaders).
- `crates/imgui-sys` — FFI bindings crate. Compiles the vendored Dear ImGui + `dcimgui` C wrapper
  (`vendor/imgui` git submodule, `vendor/dcimgui`) via `cc`, and generates Rust bindings via `bindgen`.
