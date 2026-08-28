# Changelog

All notable changes to this project are documented in this file.

## [0.18.0] - 2026-08-28

### Documentation
- Describe the view controls, and why orientation composes as it does

### Bug Fixes
- The toolbar buttons introduced in 0.17.0 did nothing. The interface was
  laid out only when its own status changed, and a press changes none of
  it, so the frame carrying the press was never laid out.

### Features
- A zoom lock, a viewing rotation, and a backdrop for transparency

## [0.17.0] - 2026-08-28

### Breaking Changes
- The minimum supported Rust version is now 1.95, raised from 1.89 by
  `egui-wgpu` 0.36 - the first release built against wgpu 30. Building with
  an older toolchain fails to resolve.

### Documentation
- Describe the interface, and fix what had gone stale

### Features
- A status line, a toolbar on approach, and a key sheet

### Performance
- Let the picture reach the screen before the chrome

## [0.16.0] - 2026-08-26

### Features
- Hand a file to the window already open

## [0.15.0] - 2026-08-26

### Features
- Draw images past the texture limit as tiles

## [0.14.0] - 2026-08-25

### Features
- Carry 10- and 12-bit sources through at sixteen bits

## [0.13.0] - 2026-08-25

### Features
- Output high dynamic range, and follow the display

## [0.12.0] - 2026-08-23

### Features
- Play GIF, APNG and animated WebP

## [0.11.0] - 2026-08-22

### Features
- Close the network to the decoder and cheapen the bridge

## [0.10.0] - 2026-08-21

### Features
- Show a HEIC from its thumbnail, and read the colour it states

## [0.9.0] - 2026-08-20

### Features
- Stop a decode that nobody is waiting for

## [0.8.0] - 2026-08-20

### Features
- Open AVIF, with its colour applied on the GPU

## [0.7.0] - 2026-08-19

### Features
- Open HEIC, the format a phone photographs in

## [0.6.0] - 2026-08-14

### Features
- Define the protocol between the viewer and a decoder process
- Decode in a process that can do nothing else

## [0.5.0] - 2026-08-14

### Bug Fixes
- Redraw a vector image when it is first fitted to the window

### Features
- Open SVG and redraw it when the zoom changes

## [0.4.2] - 2026-08-14

### Bug Fixes
- Open greyscale JPEG XL instead of refusing it

### Documentation
- Render breaking changes from the commit that makes them

### Features
- Open JPEG XL
## [0.4.1] - 2026-08-12

### Bug Fixes
- Show an untagged image as it is, without assuming sRGB
## [0.4.0] - 2026-08-12

### Breaking Changes
- The minimum supported Rust version is now 1.89, raised from 1.88 by the
  `moxcms` 0.9 upgrade. Building with an older toolchain fails to resolve.

### CI
- Hold the build to the declared MSRV

### Features
- Open WebP, and name every format in one place
## [0.3.1] - 2026-08-10

### Bug Fixes
- Remove the application key left by v0.2.0
- Keep Cargo.lock in step with the manifest
## [0.3.0] - 2026-08-10

### Features
- Convert images to the display's colour profile on the GPU
## [0.2.0] - 2026-08-10

### Bug Fixes
- Keep the argument-parsing test portable

### Features
- Install nitid for the current user and register its file types
- Show the embedded thumbnail first and decode off the event loop
## [0.1.0] - 2026-08-10

### CI
- Draft a GitHub release from a version tag

### Features
- Scaffold the project
- Add the lacodda line mark and derived assets
- Show images in a window with zoom, pan, and folder navigation
