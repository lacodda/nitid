//! The shared memory the pixels come home through.
//!
//! A 12-megapixel decode is 48 MB of RGBA; pushed through a pipe it was
//! measured to add roughly a quarter to the decode time (v0.9.0: 832 ms in
//! process, 1060–1221 ms across the pipe). A pagefile-backed section turns
//! that into two plain memory copies: the decoder writes pixels in, the viewer
//! reads them out.
//!
//! The section is *reserved*, not committed: `MAX_PAYLOAD` of address space
//! costs nothing until pages are actually touched, so a section sized for the
//! largest permissible image does not charge half a gigabyte to every decode
//! of a thumbnail. Whoever touches a page first commits it.
//!
//! The viewer commits the range it is about to read before reading it. That
//! detail is load-bearing: the decoder is the process that parsed the hostile
//! file, and a decoder that claims more bytes than it committed must cost the
//! viewer nothing worse than reading zeroes — never a fault on an uncommitted
//! page.
//!
//! The handle crosses by `DuplicateHandle` into the suspended child, not by
//! inheritance. Inheritance is process-wide: two decodes spawning concurrently
//! would each leak their section into the other's child, and a compromised
//! decoder could then scribble over a sibling's pixels.

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, FILE_MAP_WRITE, MEM_COMMIT, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE, SEC_RESERVE,
    UnmapViewOfFile, VirtualAlloc,
};
use windows::core::PCWSTR;

use super::protocol::MAX_PAYLOAD;

/// A reserved section owned by the viewer, mapped into it for reading back.
pub struct Section {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: the handle and the view are process-wide resources, not tied to the
// thread that created them; the reply is read on the thread that waits for the
// child.
unsafe impl Send for Section {}

impl Section {
    /// Reserve a section large enough for any reply the protocol allows.
    pub fn reserve() -> Result<Self> {
        // SAFETY: the mapping handle and the view are both owned by the
        // returned value and released in its `Drop`.
        unsafe {
            let handle = CreateFileMappingW(
                // Backed by the pagefile: there is no file, only memory.
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE | SEC_RESERVE,
                ((MAX_PAYLOAD as u64) >> 32) as u32,
                MAX_PAYLOAD as u32,
                PCWSTR::null(),
            )
            .context("creating the pixel section")?;

            let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if view.Value.is_null() {
                let _ = CloseHandle(handle);
                return Err(windows::core::Error::from_thread()).context("mapping the pixel section");
            }

            Ok(Self { handle, view })
        }
    }

    /// The handle to duplicate into the decoder.
    pub fn handle(&self) -> HANDLE {
        self.handle
    }

    /// Copy `length` bytes out of the section.
    ///
    /// The length has already been checked against the image dimensions by the
    /// protocol; what this guards is the memory itself. The range is committed
    /// here, in the viewer, before it is read: pages the decoder never wrote
    /// arrive as zeroes rather than as an access violation.
    pub fn read(&self, length: usize) -> Result<Vec<u8>> {
        if length > MAX_PAYLOAD {
            bail!("the decoder claims {length} bytes in the section, past the {MAX_PAYLOAD} limit");
        }

        // SAFETY: the view spans MAX_PAYLOAD bytes and `length` is within it;
        // committing an already committed page is a no-op, so this cannot
        // conflict with what the decoder committed.
        unsafe {
            let committed = VirtualAlloc(Some(self.view.Value), length, MEM_COMMIT, PAGE_READWRITE);
            if committed.is_null() {
                return Err(windows::core::Error::from_thread()).context("committing the pixel section for reading");
            }

            let mut pixels = vec![0u8; length];
            std::ptr::copy_nonoverlapping(self.view.Value as *const u8, pixels.as_mut_ptr(), length);
            Ok(pixels)
        }
    }
}

impl Drop for Section {
    fn drop(&mut self) {
        // SAFETY: both were acquired in `reserve` and are released exactly once.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// The decoder's side: the section it was handed, mapped for writing.
pub struct DecoderView {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl DecoderView {
    /// Map the section whose handle value arrived in the request.
    ///
    /// The value came from the viewer, which duplicated the handle into this
    /// process before resuming it; a value that maps to nothing — a viewer
    /// that never created a section sends zero — is an ordinary `None`, and
    /// the pixels go back inline instead.
    pub fn open(handle_value: u64) -> Option<Self> {
        if handle_value == 0 {
            return None;
        }

        // SAFETY: mapping a handle value is safe to *attempt* with any value —
        // a stale or wrong one fails and is handled; the view, once obtained,
        // is unmapped in `Drop`.
        unsafe {
            let view = MapViewOfFile(HANDLE(handle_value as isize as *mut core::ffi::c_void), FILE_MAP_WRITE, 0, 0, 0);
            (!view.Value.is_null()).then_some(Self { view })
        }
    }

    /// Write the pixels at the start of the section.
    pub fn write(&mut self, pixels: &[u8]) -> Result<()> {
        if pixels.len() > MAX_PAYLOAD {
            bail!("{} bytes of pixels do not fit the {MAX_PAYLOAD}-byte section", pixels.len());
        }

        // SAFETY: the section is MAX_PAYLOAD bytes and the length was just
        // checked against that; the pages are committed before being written.
        unsafe {
            let committed = VirtualAlloc(Some(self.view.Value), pixels.len(), MEM_COMMIT, PAGE_READWRITE);
            if committed.is_null() {
                return Err(windows::core::Error::from_thread()).context("committing the pixel section for writing");
            }

            std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.view.Value as *mut u8, pixels.len());
        }
        Ok(())
    }
}

impl Drop for DecoderView {
    fn drop(&mut self) {
        // SAFETY: the view was mapped in `open` and is unmapped exactly once.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
        }
    }
}
