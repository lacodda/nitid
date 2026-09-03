//! Dragging the picture out of the window.
//!
//! The other half of drag and drop: a file arrives by being dropped on the
//! window, and it leaves by being dragged off it. Dragging out is what makes
//! the viewer a step in somebody's work rather than a dead end — the picture
//! on screen goes into a chat, a mail, an editor without a trip through the
//! file manager to find it again.
//!
//! What travels is the file, and where there is no file, the picture. A chat
//! window and a mail client want a path (`CF_HDROP`, the same thing the shell
//! hands them); an editor that paints wants pixels (`CF_DIB`, the same bitmap
//! `Ctrl+C` puts on the clipboard, with the same honesty about what the
//! numbers mean — see `clipboard` and ADR 0019). Both are offered and the
//! receiving application takes whichever it understands, which is how the
//! shell's own drags work. A picture pasted from the clipboard has no file, so
//! it offers only the bitmap: inventing a temporary file for it would write to
//! disk without being asked, which ADR 0020 says the viewer does not do.
//!
//! The gesture is `Ctrl` plus a left drag (decision of the owner). The bare
//! left drag stays what it has always been — panning — because a viewer whose
//! main gesture changed meaning depending on where the picture sat would be a
//! viewer nobody could pan with confidence.
//!
//! The payload arithmetic — laying out a `CF_HDROP`, which is a header, a run
//! of doubly-null-terminated UTF-16 paths, and a final null — lives apart from
//! the Windows calls and is tested without a shell, the same split the
//! clipboard module uses.

use std::path::Path;

/// How far the pointer travels before a press becomes a drag.
///
/// A drag that starts on the first pixel of movement fires on every click,
/// because no hand holds a mouse perfectly still. Windows publishes its own
/// threshold as `SM_CXDRAG`, which is four pixels on a default install; that
/// value is used directly rather than asked for, so the decision to start a
/// drag can be tested without a desktop.
pub const DRAG_THRESHOLD: f64 = 4.0;

/// Whether a pointer that has moved this far from where it was pressed is
/// dragging rather than clicking.
///
/// Its own function because it is the whole difference between "the user
/// wanted to hand this file to another window" and "the user clicked": a
/// threshold of zero would send a drag off on every press, and a modifier
/// held during an ordinary click would stop being an ordinary click.
pub fn far_enough(from: (f64, f64), to: (f64, f64)) -> bool {
    (to.0 - from.0).abs() >= DRAG_THRESHOLD || (to.1 - from.1).abs() >= DRAG_THRESHOLD
}

/// Lay out a `CF_HDROP` payload for one file.
///
/// The format is a `DROPFILES` header followed by the paths as UTF-16, each
/// null-terminated, with one more null closing the list. `fWide` says the
/// paths are UTF-16 rather than ANSI, and `pFiles` is the byte offset from the
/// start of the block to the first path — getting either wrong hands the
/// receiving application a list it reads as garbage or as nothing at all.
pub fn to_hdrop(paths: &[&Path]) -> Vec<u8> {
    // DROPFILES: pFiles (u32), pt (two i32), fNC (BOOL), fWide (BOOL).
    const HEADER_SIZE: u32 = 20;

    let mut out = Vec::new();
    out.extend_from_slice(&HEADER_SIZE.to_le_bytes()); // pFiles
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    out.extend_from_slice(&0u32.to_le_bytes()); // fNC
    out.extend_from_slice(&1u32.to_le_bytes()); // fWide: the paths below are UTF-16

    for path in paths {
        for unit in path.to_string_lossy().encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    // The list itself is terminated by an empty string.
    out.extend_from_slice(&0u16.to_le_bytes());

    out
}

/// What a drag is carrying.
///
/// Built on the event-loop thread from what is on screen, then handed to the
/// Windows call. Keeping it a plain value means the decision about what
/// travels — file, pixels, or both — is made and can be read in one place,
/// rather than being spread through the COM object that serves it.
pub struct Payload {
    /// The `CF_HDROP` block, when the picture is a file on disk.
    pub hdrop: Option<Vec<u8>>,
    /// The `CF_DIB` block: the picture's own pixels.
    pub dib: Vec<u8>,
}

#[cfg(windows)]
pub use windows_drag::start;

/// The same surface where there is no shell to drag into.
///
/// As with the clipboard: the callers stay free of `cfg` attributes, and the
/// non-Windows build is what CI's MSRV job compiles.
#[cfg(not(windows))]
pub fn start(_payload: Payload) -> anyhow::Result<bool> {
    Ok(false)
}

#[cfg(windows)]
mod windows_drag {
    use anyhow::{Context, Result, bail};
    use windows::Win32::Foundation::{
        DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, DV_E_TYMED, E_NOTIMPL, HGLOBAL,
        OLE_E_ADVISENOTSUPPORTED, S_FALSE, S_OK,
    };
    use windows::Win32::System::Com::{
        DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumFORMATETC_Impl, IEnumSTATDATA, STGMEDIUM, TYMED_HGLOBAL,
    };
    use windows::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource, IDropSource_Impl};
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS};
    use windows::core::{BOOL, HRESULT, Ref, implement};

    use super::Payload;

    /// One offered format and the bytes behind it.
    struct Offer {
        format: u16,
        bytes: Vec<u8>,
    }

    /// The data object handed to the shell for the length of the drag.
    ///
    /// It holds the bytes rather than a reference to the viewer's state: a
    /// drop can be answered after the drag ends — some applications ask for
    /// the data late — and a borrow would be a promise the event loop cannot
    /// keep.
    #[implement(IDataObject)]
    struct DragData {
        offers: Vec<Offer>,
    }

    impl DragData {
        fn find(&self, format: u16) -> Option<&Offer> {
            self.offers.iter().find(|offer| offer.format == format)
        }

        fn formats(&self) -> Vec<FORMATETC> {
            self.offers.iter().map(|offer| formatetc(offer.format)).collect()
        }
    }

    /// The description of one offered format, in the shape the shell asks in.
    fn formatetc(format: u16) -> FORMATETC {
        FORMATETC {
            cfFormat: format,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }
    }

    /// Copy a payload into a movable global block, as every shell format is
    /// carried.
    fn to_global(bytes: &[u8]) -> Result<HGLOBAL> {
        let handle = unsafe { GlobalAlloc(GHND, bytes.len()) }.context("allocating for the drag")?;
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            bail!("locking the drag's memory");
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len());
            let _ = GlobalUnlock(handle);
        }
        Ok(handle)
    }

    impl IDataObject_Impl for DragData_Impl {
        fn GetData(&self, request: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
            let request = unsafe { request.as_ref() }.ok_or(windows::core::Error::from(DV_E_FORMATETC))?;
            if request.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
                return Err(DV_E_TYMED.into());
            }
            let offer = self.find(request.cfFormat).ok_or(windows::core::Error::from(DV_E_FORMATETC))?;

            // The receiver frees this block, which is what `pUnkForRelease`
            // being empty means: ownership travels with the medium.
            let handle = to_global(&offer.bytes).map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY))?;
            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: windows::Win32::System::Com::STGMEDIUM_0 { hGlobal: handle },
                pUnkForRelease: std::mem::ManuallyDrop::new(None),
            })
        }

        /// Filling a buffer the caller allocated. Declined: the sizes here are
        /// decided by the picture, not by the receiver, and a caller that
        /// wants the data has `GetData`.
        fn GetDataHere(&self, _format: *const FORMATETC, _medium: *mut STGMEDIUM) -> windows::core::Result<()> {
            Err(DV_E_FORMATETC.into())
        }

        /// Whether a format is on offer, without producing it. The shell asks
        /// this while the pointer moves, so it has to agree with `GetData` —
        /// answering yes here and failing there is what makes a drop land on
        /// a window that then refuses it.
        fn QueryGetData(&self, request: *const FORMATETC) -> HRESULT {
            let Some(request) = (unsafe { request.as_ref() }) else {
                return DV_E_FORMATETC;
            };
            if request.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
                return DV_E_TYMED;
            }
            if self.find(request.cfFormat).is_some() { S_OK } else { DV_E_FORMATETC }
        }

        /// No format here is a rendering of another, so the canonical form of
        /// a format is itself — which is what `DATA_S_SAMEFORMATETC` says.
        fn GetCanonicalFormatEtc(&self, _in: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
            if let Some(out) = unsafe { out.as_mut() } {
                out.ptd = std::ptr::null_mut();
            }
            DATA_S_SAMEFORMATETC
        }

        /// A drag source offers data; it does not receive any.
        fn SetData(&self, _format: *const FORMATETC, _medium: *const STGMEDIUM, _release: BOOL) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }

        fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
            // DATADIR_GET is 1. Only the get direction has anything to list:
            // this object never accepts data.
            if direction != 1 {
                return Err(E_NOTIMPL.into());
            }
            Ok(FormatEnum {
                formats: self.formats(),
                at: std::cell::Cell::new(0),
            }
            .into())
        }

        /// Change notification, which a drag's data never sends.
        fn DAdvise(&self, _format: *const FORMATETC, _flags: u32, _sink: Ref<IAdviseSink>) -> windows::core::Result<u32> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn DUnadvise(&self, _connection: u32) -> windows::core::Result<()> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }
    }

    /// The list of offered formats, walked by whoever asks what is available.
    ///
    /// The shell calls this to decide whether the pointer may be dropped at
    /// all, so an enumerator that reports nothing is a drag that is refused
    /// everywhere.
    #[implement(IEnumFORMATETC)]
    struct FormatEnum {
        formats: Vec<FORMATETC>,
        at: std::cell::Cell<usize>,
    }

    impl IEnumFORMATETC_Impl for FormatEnum_Impl {
        fn Next(&self, wanted: u32, out: *mut FORMATETC, fetched: *mut u32) -> HRESULT {
            let at = self.at.get();
            let available = self.formats.len().saturating_sub(at);
            let taking = available.min(wanted as usize);

            for index in 0..taking {
                unsafe { out.add(index).write(self.formats[at + index]) };
            }
            self.at.set(at + taking);

            if let Some(fetched) = unsafe { fetched.as_mut() } {
                *fetched = taking as u32;
            }
            // S_FALSE means "fewer than asked for", which is how the caller
            // knows the list ended.
            if taking == wanted as usize { S_OK } else { S_FALSE }
        }

        fn Skip(&self, count: u32) -> windows::core::Result<()> {
            let at = self.at.get() + count as usize;
            self.at.set(at.min(self.formats.len()));
            if at <= self.formats.len() { Ok(()) } else { Err(S_FALSE.into()) }
        }

        fn Reset(&self) -> windows::core::Result<()> {
            self.at.set(0);
            Ok(())
        }

        fn Clone(&self) -> windows::core::Result<IEnumFORMATETC> {
            Ok(FormatEnum {
                formats: self.formats.clone(),
                at: std::cell::Cell::new(self.at.get()),
            }
            .into())
        }
    }

    /// The half of the drag that watches the mouse and the keyboard.
    ///
    /// Escape cancels, letting go drops, and anything else keeps going. This
    /// is the whole of it: the shell runs the loop, and the source only says
    /// when to stop.
    #[implement(IDropSource)]
    struct DragSource;

    impl IDropSource_Impl for DragSource_Impl {
        fn QueryContinueDrag(&self, escape_pressed: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape_pressed.as_bool() {
                return DRAGDROP_S_CANCEL;
            }
            // Letting go of the buttons is the drop. Both are checked because
            // the drag may have been started with either — a right drag ends
            // when the right button comes up.
            if keys.0 & (MK_LBUTTON.0 | MK_RBUTTON.0) == 0 {
                return DRAGDROP_S_DROP;
            }
            S_OK
        }

        /// The shell's own cursors say what a drop would do, and they say it
        /// better than anything drawn here would.
        fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    /// Run a drag, returning whether the picture was actually dropped
    /// somewhere.
    ///
    /// This blocks: `DoDragDrop` runs its own message loop until the button
    /// comes up or Escape is pressed. That is the price of the shell's drag
    /// protocol and it is paid on the event-loop thread deliberately — OLE is
    /// initialised on that thread (winit does it to register the window as a
    /// drop target), and an apartment-threaded object cannot be handed to
    /// another thread without marshalling it.
    ///
    /// Copy is the only effect offered: a viewer that let a drag *move* the
    /// file would delete the picture the user is looking at because they
    /// dragged it into a chat.
    pub fn start(payload: Payload) -> Result<bool> {
        let mut offers = Vec::new();
        if let Some(hdrop) = payload.hdrop {
            // Offered first: an application that understands both should
            // take the file, which keeps the original's format and its
            // metadata instead of flattening it to a bitmap.
            offers.push(Offer {
                format: CF_HDROP.0,
                bytes: hdrop,
            });
        }
        offers.push(Offer {
            format: CF_DIB.0,
            bytes: payload.dib,
        });

        let data: IDataObject = DragData { offers }.into();
        let source: IDropSource = DragSource.into();

        let mut effect = DROPEFFECT_NONE;
        let result = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };

        match result {
            DRAGDROP_S_DROP => Ok(effect != DROPEFFECT_NONE),
            DRAGDROP_S_CANCEL => Ok(false),
            other => Err(windows::core::Error::from(other)).context("dragging the picture out"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The header is what tells the receiving application how to read the
    /// rest: a wrong offset or a missing `fWide` hands it garbage.
    #[test]
    fn the_hdrop_header_describes_the_list() {
        let path = PathBuf::from(r"C:\photos\a.jpg");
        let block = to_hdrop(&[path.as_path()]);

        assert_eq!(
            u32::from_le_bytes(block[0..4].try_into().unwrap()),
            20,
            "the paths do not start after the header"
        );
        assert_eq!(
            u32::from_le_bytes(block[16..20].try_into().unwrap()),
            1,
            "the paths were not declared as UTF-16"
        );
    }

    /// The paths are UTF-16 and the list ends with an empty one. Dropping the
    /// final terminator is the classic `CF_HDROP` bug: the receiver reads past
    /// the block looking for the next name.
    #[test]
    fn the_paths_are_utf16_and_doubly_terminated() {
        let path = PathBuf::from(r"C:\a.jpg");
        let block = to_hdrop(&[path.as_path()]);

        let units: Vec<u16> = block[20..].as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        let expected: Vec<u16> = r"C:\a.jpg".encode_utf16().chain([0, 0]).collect();
        assert_eq!(units, expected, "the path did not come out as a terminated UTF-16 string plus a final null");
    }

    /// Several files travel as several names in one block, which is what makes
    /// dragging a selection out possible later.
    #[test]
    fn several_paths_share_one_block() {
        let first = PathBuf::from("a.jpg");
        let second = PathBuf::from("b.png");
        let block = to_hdrop(&[first.as_path(), second.as_path()]);

        let units: Vec<u16> = block[20..].as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        let expected: Vec<u16> = "a.jpg".encode_utf16().chain([0]).chain("b.png".encode_utf16()).chain([0, 0]).collect();
        assert_eq!(units, expected);
    }

    /// A path outside the BMP — an emoji in a folder name — is two UTF-16
    /// units, and a converter that assumed one per character would truncate
    /// the name.
    #[test]
    fn a_path_beyond_the_basic_plane_survives() {
        let path = PathBuf::from("🌄.jpg");
        let block = to_hdrop(&[path.as_path()]);

        let units: Vec<u16> = block[20..].as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        let expected: Vec<u16> = "🌄.jpg".encode_utf16().chain([0, 0]).collect();
        assert_eq!(units, expected);
        assert_eq!(units.len(), expected.len(), "the astral character did not take two units");
    }

    /// An empty list is still a well-formed block: a header and the closing
    /// null. Nothing calls this, but a receiver handed a truncated block reads
    /// off the end of it.
    #[test]
    fn an_empty_list_is_still_terminated() {
        let block = to_hdrop(&[]);
        assert_eq!(block.len(), 22, "an empty list is not a header plus one null");
    }

    /// A click is not a drag. Without a threshold every press with the
    /// modifier down would send a drag off, and the button would stop
    /// working as a button.
    #[test]
    fn a_drag_needs_more_than_a_twitch() {
        let from = (100.0, 100.0);
        assert!(!far_enough(from, (100.0, 100.0)), "a stationary pointer started a drag");
        assert!(!far_enough(from, (103.0, 103.0)), "a three-pixel twitch started a drag");
        assert!(far_enough(from, (104.0, 100.0)), "four pixels sideways did not start a drag");
        assert!(far_enough(from, (100.0, 104.0)), "four pixels down did not start a drag");
        // And in both directions: a drag towards the top left is a drag.
        assert!(far_enough(from, (96.0, 100.0)));
        assert!(far_enough(from, (100.0, 96.0)));
    }
}
