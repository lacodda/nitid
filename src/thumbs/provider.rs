//! The DLL Explorer asks for thumbnails.
//!
//! As little as a thumbnail provider can be: it is handed a stream, passes the
//! stream's bytes to the `nitid.exe` beside it, and makes a bitmap of what
//! comes back. It reads no picture. A file from anywhere is read in one place,
//! the decoder's sandbox, and this is not a second one
//! (`docs/adr/0049-explorers-thumbnails-come-from-a-thin-dll.md`).
//!
//! **A crate of its own, with no dependencies.** `build.rs` compiles this file
//! into `nitid_thumbs.dll` with `rustc` alone and the program carries the result
//! inside itself, because an installed copy takes exactly two files out of the
//! release that replaces it (ADR 0043) and a third would never arrive. COM is
//! four small tables written out below.
//!
//! **A stream, and not a path.** Explorer runs a handler initialised with a
//! stream in a host process of its own, so nothing here is ever loaded into
//! Explorer; one that wants a path has to ask for that isolation to be
//! switched off. It is also what lets a picture inside a zip, or one still in
//! the cloud, have a thumbnail: neither is at a path.
//!
//! It must not panic: the host is shared with every other handler, and this
//! build aborts on a panic.

use std::ffi::c_void;
use std::io::{Read, Write};
use std::mem::offset_of;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

// Read by the program as well, which is another crate when this one is built.
#[path = "wire.rs"]
#[allow(clippy::duplicate_mod)]
mod wire;

/// How long one thumbnail may take, from the file's first byte to the reply.
///
/// The decoder has a shorter limit of its own (`sandbox::THUMB_TIMEOUT`); this
/// is for whatever that one cannot see, such as a disk that stopped answering.
const PATIENCE: Duration = Duration::from_secs(25);

const S_OK: i32 = 0;
const S_FALSE: i32 = 1;
const E_POINTER: i32 = 0x8000_4003_u32 as i32;
const E_NOINTERFACE: i32 = 0x8000_4002_u32 as i32;
const E_FAIL: i32 = 0x8000_4005_u32 as i32;
const E_UNEXPECTED: i32 = 0x8000_FFFF_u32 as i32;
const E_OUTOFMEMORY: i32 = 0x8007_000E_u32 as i32;
const ALREADY_INITIALIZED: i32 = 0x8007_04DF_u32 as i32;
const CLASS_E_NOAGGREGATION: i32 = 0x8004_0110_u32 as i32;
const CLASS_E_CLASSNOTAVAILABLE: i32 = 0x8004_0111_u32 as i32;

/// The bitmap has an alpha channel (`WTS_ALPHATYPE`).
const WTSAT_ARGB: u32 = 2;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// A folder being drawn gives way to whatever the person is doing, a picture
/// being opened in nitid first of all. The decoder started in turn inherits it.
const BELOW_NORMAL_PRIORITY: u32 = 0x0000_4000;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    first: u32,
    second: u16,
    third: u16,
    rest: [u8; 8],
}

impl Guid {
    const fn of(value: u128) -> Self {
        Self {
            first: (value >> 96) as u32,
            second: (value >> 80) as u16,
            third: (value >> 64) as u16,
            rest: (value as u64).to_be_bytes(),
        }
    }
}

const CLASS: Guid = Guid::of(wire::CLASS);
const IUNKNOWN: Guid = Guid::of(0x00000000_0000_0000_c000_000000000046);
const ICLASSFACTORY: Guid = Guid::of(0x00000001_0000_0000_c000_000000000046);
const IINITIALIZEWITHSTREAM: Guid = Guid::of(0xb824b49d_22ac_4161_ac8a_9916e8fa3f7f);
const ITHUMBNAILPROVIDER: Guid = Guid::of(0xe357fccd_a995_4576_b01f_234630154e96);

type Query = unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32;
type Count = unsafe extern "system" fn(*mut c_void) -> u32;

#[repr(C)]
struct FactoryTable {
    query: Query,
    add: Count,
    release: Count,
    create: unsafe extern "system" fn(*mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32,
    lock: unsafe extern "system" fn(*mut c_void, i32) -> i32,
}

#[repr(C)]
struct InitializeTable {
    query: Query,
    add: Count,
    release: Count,
    initialize: unsafe extern "system" fn(*mut c_void, *mut Stream, u32) -> i32,
}

#[repr(C)]
struct ThumbnailTable {
    query: Query,
    add: Count,
    release: Count,
    get: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> i32,
}

/// An `IStream`, as far into its table as this goes.
#[repr(C)]
struct Stream {
    table: *const StreamTable,
}

#[repr(C)]
struct StreamTable {
    query: Query,
    add: Count,
    release: Count,
    read: unsafe extern "system" fn(*mut Stream, *mut c_void, u32, *mut u32) -> i32,
    write: unsafe extern "system" fn(*mut Stream, *const c_void, u32, *mut u32) -> i32,
    seek: unsafe extern "system" fn(*mut Stream, i64, u32, *mut u64) -> i32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleExW(flags: u32, address: *const u16, module: *mut *mut c_void) -> i32;
    fn GetModuleFileNameW(module: *mut c_void, name: *mut u16, size: u32) -> u32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateDIBSection(dc: *mut c_void, info: *const BitmapInfo, usage: u32, bits: *mut *mut c_void, section: *mut c_void, offset: u32) -> *mut c_void;
}

#[repr(C)]
struct BitmapInfo {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bits: u16,
    compression: u32,
    image_size: u32,
    across: i32,
    down: i32,
    used: u32,
    important: u32,
    colours: [u32; 1],
}

/// What is out: objects, references to the factory, and locks. Nothing may be
/// unloaded while this is not zero.
static OUT: AtomicUsize = AtomicUsize::new(0);

// --- The two functions the host calls ---

/// The factory for [`CLASS`].
///
/// # Safety
///
/// Called by COM, with pointers it vouches for.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(class: *const Guid, interface: *const Guid, out: *mut *mut c_void) -> i32 {
    if class.is_null() || interface.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked above; the rest is COM's promise.
    unsafe {
        *out = ptr::null_mut();
        if *class != CLASS {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        factory_query(FACTORY.this(), interface, out)
    }
}

/// Whether nothing of this module is in use.
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> i32 {
    if OUT.load(Ordering::SeqCst) == 0 { S_OK } else { S_FALSE }
}

// --- The factory: one, and never freed ---

#[repr(C)]
struct Factory {
    table: &'static FactoryTable,
}

static FACTORY: Factory = Factory {
    table: &FactoryTable {
        query: factory_query,
        add: factory_add,
        release: factory_release,
        create: factory_create,
        lock: factory_lock,
    },
};

impl Factory {
    fn this(&'static self) -> *mut c_void {
        ptr::from_ref(self).cast_mut().cast()
    }
}

unsafe extern "system" fn factory_query(this: *mut c_void, interface: *const Guid, out: *mut *mut c_void) -> i32 {
    if interface.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked above; `this` is the factory.
    unsafe {
        if *interface == IUNKNOWN || *interface == ICLASSFACTORY {
            factory_add(this);
            *out = this;
            S_OK
        } else {
            *out = ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn factory_add(_this: *mut c_void) -> u32 {
    OUT.fetch_add(1, Ordering::SeqCst);
    2
}

unsafe extern "system" fn factory_release(_this: *mut c_void) -> u32 {
    OUT.fetch_sub(1, Ordering::SeqCst);
    1
}

unsafe extern "system" fn factory_create(_this: *mut c_void, outer: *mut c_void, interface: *const Guid, out: *mut *mut c_void) -> i32 {
    if interface.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked above; the object is made here with one reference, which
    // is given up again once the caller has its own.
    unsafe {
        *out = ptr::null_mut();
        if !outer.is_null() {
            return CLASS_E_NOAGGREGATION;
        }
        OUT.fetch_add(1, Ordering::SeqCst);
        let provider: *mut c_void = Box::into_raw(Box::new(Provider {
            initialize: &INITIALIZE,
            thumbnail: &THUMBNAIL,
            references: AtomicU32::new(1),
            stream: AtomicPtr::new(ptr::null_mut()),
        }))
        .cast();
        let answer = provider_query(provider, interface, out);
        provider_release(provider);
        answer
    }
}

unsafe extern "system" fn factory_lock(_this: *mut c_void, lock: i32) -> i32 {
    if lock != 0 {
        OUT.fetch_add(1, Ordering::SeqCst);
    } else {
        OUT.fetch_sub(1, Ordering::SeqCst);
    }
    S_OK
}

// --- The provider: two interfaces on one object ---

#[repr(C)]
struct Provider {
    /// `IInitializeWithStream`, and the object's identity.
    initialize: &'static InitializeTable,
    /// `IThumbnailProvider`: a pointer to this field is that interface.
    thumbnail: &'static ThumbnailTable,
    references: AtomicU32,
    stream: AtomicPtr<Stream>,
}

static INITIALIZE: InitializeTable = InitializeTable {
    query: provider_query,
    add: provider_add,
    release: provider_release,
    initialize: provider_initialize,
};

static THUMBNAIL: ThumbnailTable = ThumbnailTable {
    query: thumbnail_query,
    add: thumbnail_add,
    release: thumbnail_release,
    get: thumbnail_get,
};

/// The object behind its second interface.
unsafe fn whole(thumbnail: *mut c_void) -> *mut c_void {
    // SAFETY: the pointer is to that field of a `Provider`, which is how it
    // was handed out.
    unsafe { thumbnail.byte_sub(offset_of!(Provider, thumbnail)) }
}

unsafe extern "system" fn provider_query(this: *mut c_void, interface: *const Guid, out: *mut *mut c_void) -> i32 {
    if interface.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked above; `this` is a `Provider`.
    unsafe {
        let found = if *interface == IUNKNOWN || *interface == IINITIALIZEWITHSTREAM {
            this
        } else if *interface == ITHUMBNAILPROVIDER {
            this.byte_add(offset_of!(Provider, thumbnail))
        } else {
            *out = ptr::null_mut();
            return E_NOINTERFACE;
        };
        provider_add(this);
        *out = found;
    }
    S_OK
}

unsafe extern "system" fn provider_add(this: *mut c_void) -> u32 {
    // SAFETY: `this` is a `Provider` the caller holds a reference to.
    let provider = unsafe { &*this.cast::<Provider>() };
    provider.references.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn provider_release(this: *mut c_void) -> u32 {
    // SAFETY: as above; the last reference takes the object back from the raw
    // pointer it was made into, and nobody is left to use it.
    unsafe {
        let left = (*this.cast::<Provider>()).references.fetch_sub(1, Ordering::SeqCst) - 1;
        if left == 0 {
            let provider = Box::from_raw(this.cast::<Provider>());
            let stream = provider.stream.load(Ordering::SeqCst);
            if !stream.is_null() {
                ((*(*stream).table).release)(stream.cast());
            }
            drop(provider);
            OUT.fetch_sub(1, Ordering::SeqCst);
        }
        left
    }
}

unsafe extern "system" fn provider_initialize(this: *mut c_void, stream: *mut Stream, _mode: u32) -> i32 {
    if stream.is_null() {
        return E_POINTER;
    }
    // SAFETY: `this` is a `Provider`; the stream is the caller's, and is kept
    // only with a reference of its own.
    unsafe {
        let provider = &*this.cast::<Provider>();
        let first = provider
            .stream
            .compare_exchange(ptr::null_mut(), stream, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if !first {
            return ALREADY_INITIALIZED;
        }
        ((*(*stream).table).add)(stream.cast());
    }
    S_OK
}

unsafe extern "system" fn thumbnail_query(this: *mut c_void, interface: *const Guid, out: *mut *mut c_void) -> i32 {
    // SAFETY: `this` is the second interface of a `Provider`.
    unsafe { provider_query(whole(this), interface, out) }
}

unsafe extern "system" fn thumbnail_add(this: *mut c_void) -> u32 {
    // SAFETY: as above.
    unsafe { provider_add(whole(this)) }
}

unsafe extern "system" fn thumbnail_release(this: *mut c_void) -> u32 {
    // SAFETY: as above.
    unsafe { provider_release(whole(this)) }
}

unsafe extern "system" fn thumbnail_get(this: *mut c_void, side: u32, bitmap: *mut *mut c_void, alpha: *mut u32) -> i32 {
    if bitmap.is_null() || alpha.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked above; the stream holds a reference for as long as the
    // object does, and the caller holds the object.
    unsafe {
        *bitmap = ptr::null_mut();
        let stream = (*whole(this).cast::<Provider>()).stream.load(Ordering::SeqCst);
        if stream.is_null() {
            return E_UNEXPECTED;
        }
        // Asked twice, it answers twice.
        ((*(*stream).table).seek)(stream, 0, 0, ptr::null_mut());
        let Some(picture) = ask(stream, side.clamp(1, wire::LONGEST)) else {
            return E_FAIL;
        };
        let made = bitmap_of(&picture);
        if made.is_null() {
            return E_OUTOFMEMORY;
        }
        *bitmap = made;
        *alpha = WTSAT_ARGB;
    }
    S_OK
}

// --- The work, which is somebody else's ---

/// The `nitid.exe` this DLL is installed beside.
fn program() -> Option<PathBuf> {
    /// The module this address is in, without counting a reference.
    const FROM_ADDRESS: u32 = 0x4;
    const UNCOUNTED: u32 = 0x2;

    let mut module = ptr::null_mut();
    let mut name = vec![0u16; 32 * 1024];
    // SAFETY: the address is this module's own, and the buffer is as long as
    // the call is told.
    let length = unsafe {
        if GetModuleHandleExW(FROM_ADDRESS | UNCOUNTED, ptr::from_ref(&OUT).cast(), &mut module) == 0 {
            return None;
        }
        GetModuleFileNameW(module, name.as_mut_ptr(), name.len() as u32) as usize
    };
    let here = PathBuf::from(std::ffi::OsString::from_wide(name.get(..length)?));
    let program = here.with_file_name(wire::PROGRAM);
    program.is_file().then_some(program)
}

/// Have `nitid.exe` make the thumbnail.
fn ask(stream: *mut Stream, side: u32) -> Option<wire::Picture> {
    let mut child = Command::new(program()?)
        .args([wire::ASKED_FIRST, wire::THUMB, &side.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY)
        .spawn()
        .ok()?;
    let input = child.stdin.take()?;
    let output = child.stdout.take()?;

    // Everything below waits on the child, so a child that never answers is
    // ended by somebody who is not waiting.
    let child = Arc::new(Mutex::new(child));
    let (finished, waiting) = mpsc::channel::<()>();
    let watched = Arc::clone(&child);
    let watchdog = std::thread::Builder::new().spawn(move || {
        if waiting.recv_timeout(PATIENCE) == Err(RecvTimeoutError::Timeout) {
            end(&watched);
        }
    });
    let picture = match &watchdog {
        Ok(_) => exchange(stream, input, output, side),
        Err(_) => None,
    };

    // The thread is gone before this returns: the host may unload the module
    // as soon as nothing is out, and a thread still in it would be running
    // code that is no longer there.
    drop(finished);
    if let Ok(watchdog) = watchdog {
        let _ = watchdog.join();
    }
    end(&child);
    picture
}

/// See that a child is over, and take its exit off the system's hands.
fn end(child: &Mutex<Child>) {
    let mut child = child.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = child.kill();
    let _ = child.wait();
}

/// The file's bytes one way, a thumbnail the other.
///
/// Passed on a piece at a time, so a picture of any size costs the host the
/// size of one piece. The program reads all of it before it says anything,
/// which is what makes writing first and reading afterwards safe.
fn exchange(stream: *mut Stream, mut input: ChildStdin, output: ChildStdout, side: u32) -> Option<wire::Picture> {
    let mut piece = vec![0u8; 64 * 1024];
    let mut passed = 0u64;
    loop {
        let mut read = 0u32;
        // SAFETY: the stream is held by the object that was asked, and the
        // buffer is as long as the call is told.
        let answer = unsafe { ((*(*stream).table).read)(stream, piece.as_mut_ptr().cast(), piece.len() as u32, &mut read) };
        if answer < 0 {
            return None;
        }
        if read == 0 {
            break;
        }
        passed += u64::from(read);
        if passed > wire::LARGEST_FILE {
            return None;
        }
        // A program that has stopped reading has also stopped answering,
        // which the reply below will show.
        if input.write_all(piece.get(..read as usize)?).is_err() {
            break;
        }
    }
    // The end of the file, as the program sees it.
    drop(input);

    let mut reply = Vec::new();
    output.take(wire::LARGEST_REPLY + 1).read_to_end(&mut reply).ok()?;
    wire::read(&reply, side)
}

/// The picture as a bitmap, which belongs to whoever asked. Null if there is no
/// memory for it.
fn bitmap_of(picture: &wire::Picture) -> *mut c_void {
    let info = BitmapInfo {
        size: offset_of!(BitmapInfo, colours) as u32,
        width: picture.width as i32,
        // Negative: the top row first, as the pixels are.
        height: -(picture.height as i32),
        planes: 1,
        bits: 32,
        compression: 0,
        image_size: 0,
        across: 0,
        down: 0,
        used: 0,
        important: 0,
        colours: [0],
    };
    let mut bits = ptr::null_mut();
    // SAFETY: the section is width by height of four bytes, which is exactly
    // what `wire::read` held the pixels to.
    unsafe {
        let bitmap = CreateDIBSection(ptr::null_mut(), &info, 0, &mut bits, ptr::null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            return ptr::null_mut();
        }
        ptr::copy_nonoverlapping(picture.pixels.as_ptr(), bits.cast(), picture.pixels.len());
        bitmap
    }
}
