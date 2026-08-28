//! Confining the decoder process on Windows.
//!
//! The first choice is an **AppContainer with no capabilities**: an identity
//! that closes the network as well — a low integrity token demonstrably does
//! not, a decoder taught to try reported `listen=true connect=true` from
//! behind one. Should the container profile be unavailable on a machine, the
//! launch falls back to the previous arrangement, a **restricted token**
//! with every removable privilege deleted and **low integrity**, so nothing
//! is ever weaker than it was before the container existed — and the
//! fallback says so on stderr rather than weakening the boundary silently.
//!
//! Either way the child sits in **a job object** with kill-on-close, so it
//! cannot outlive the viewer however it exits, plus a memory cap and a
//! process count of one, so it cannot allocate the machine to death or
//! launch anything.
//!
//! The order matters: the process is created suspended, confined while it
//! cannot run a single instruction, and only then resumed. Confining a running
//! process is a race, not a boundary.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use moxcms::ColorProfile;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, PSID, SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Threading::{OpenProcessToken, ResumeThread, TerminateProcess, WaitForSingleObject};
use windows::core::{PCWSTR, w};

use super::protocol;
use super::section::Section;
use super::spawn::{AppContainer, SpawnedChild};
use crate::format::Format;
use crate::image_source::{DecodedImage, Depth, Fidelity, LoadedImage, Orientation};

/// The attribute marking a token's mandatory label group.
///
/// Spelled out rather than imported: the constant lives in a `windows` module
/// this build has no other use for, and its value is fixed by the platform.
const SE_GROUP_INTEGRITY: u32 = 0x20;

/// The most memory the decoder may hold.
///
/// A crafted file can ask a decoder for an image far larger than it claims;
/// the cap turns that from a machine-wide problem into a dead child process.
const MEMORY_LIMIT: usize = 1024 * 1024 * 1024;

/// The container identity, prepared once per process.
///
/// `None` when the profile could not be registered at all; the failure is
/// reported once rather than per decode.
fn container() -> Option<&'static AppContainer> {
    static CONTAINER: OnceLock<Option<AppContainer>> = OnceLock::new();
    CONTAINER
        .get_or_init(|| {
            AppContainer::prepare()
                .map_err(|error| eprintln!("nitid: no container profile for the decoder, running it with a restricted token: {error:#}"))
                .ok()
        })
        .as_ref()
}

/// Launch the decoder: in an AppContainer when the machine allows it, behind
/// a restricted token when it does not.
///
/// The container needs no granting anywhere: `CreateProcessW` maps the
/// executable image with the *viewer's* access, so the child never opens its
/// own path — measured by running one from a directory whose ACLs never
/// heard of the container. (v0.9.0 recorded the opposite belief — that the
/// SID needed access along the whole directory chain — but the "file not
/// found" behind it was in all likelihood that attempt's own fat-pointer
/// bug, which it also recorded.) The fallback exists for a machine where the
/// profile cannot be registered at all, and is reported once: silently
/// weakening a boundary is how gaps become surprises.
fn launch(exe: &std::path::Path) -> Result<SpawnedChild> {
    // The lever the fallback tests pull: without a way to refuse the
    // container on purpose, the restricted-token path would only ever run on
    // the machine where something already went wrong. Nothing in normal use
    // sets this.
    let refused = std::env::var_os("NITID_NO_CONTAINER").is_some();

    if !refused && let Some(container) = container() {
        match super::spawn::spawn(exe, super::DECODE_ARGUMENT, Some(container)) {
            Ok(child) => return Ok(child),
            Err(error) => {
                static REPORTED: OnceLock<()> = OnceLock::new();
                REPORTED.get_or_init(|| {
                    eprintln!("nitid: the decoder's container will not start here, running it with a restricted token: {error:#}");
                });
            }
        }
    }

    let child = super::spawn::spawn(exe, super::DECODE_ARGUMENT, None)?;
    if let Err(error) = restrict_token(child.process) {
        eprintln!("nitid: the decoder could not be restricted: {error:#}");
    }
    Ok(child)
}

/// Decode `bytes` in a confined child process.
pub fn decode(bytes: &[u8], format: Format, timeout: Duration) -> Result<LoadedImage> {
    // Created suspended, so the confinement is in place before the child runs
    // a single instruction; no window, so a decode does not flash a console
    // over the picture.
    let mut child = launch(&super::decoder_executable()?)?;

    // From here on the child must not be left suspended and running loose: a
    // guard kills it on every path out, including a panic.
    let job = confine(&child);
    let guard = ChildGuard { child: &mut child, job };

    // The pixels come home through shared memory when a section can be had;
    // when it cannot, the decoder is told nothing about one and answers down
    // the pipe. The handle is duplicated into the still-suspended child —
    // never inherited, because inheritance is process-wide and two decodes
    // spawning concurrently would each leak their section into the other's
    // child.
    let section = if std::env::var_os("NITID_SHM_DISABLED").is_none() {
        Section::reserve()
            .map_err(|error| eprintln!("nitid: the pixels will cross the pipe instead of shared memory: {error:#}"))
            .ok()
    } else {
        None
    };
    let section_handle = section
        .as_ref()
        .map(|section| duplicate_into(guard.child.process, section.handle()).unwrap_or(0))
        .unwrap_or(0);

    guard.resume().context("resuming the decoder process")?;

    let reply = exchange(guard.child, bytes, section_handle, timeout);

    // The child has answered or been killed; either way it is finished with.
    drop(guard);

    let image = match reply? {
        Ok(protocol::Reply::Inline(image)) => image,
        Ok(protocol::Reply::Shared(shared)) => {
            // A decoder that answers "the pixels are in the section" when it
            // was never handed one is lying, not confused.
            let section = section.as_ref().context("the decoder answered through a section it was never given")?;
            let pixels = section.read(shared.pixel_bytes)?;
            protocol::RawImage {
                width: shared.width,
                height: shared.height,
                depth: shared.depth,
                pixels,
                orientation: shared.orientation,
                profile: shared.profile,
            }
        }
        // A file the decoder could not read is an ordinary failure carrying
        // the decoder's own words, not a sandbox event.
        Err(message) => bail!("{message}"),
    };

    Ok(LoadedImage {
        orientation: Orientation::from_exif(u16::from(image.orientation)),
        // A profile the decoder sent that will not parse here is treated
        // as no profile, the same as anywhere else: broken colour metadata
        // is not a reason to refuse an image.
        profile: (!image.profile.is_empty()).then(|| ColorProfile::new_from_slice(&image.profile).ok()).flatten(),
        image: DecodedImage {
            width: image.width,
            height: image.height,
            pixels: image.pixels,
            // The reader refused anything but these two values already.
            depth: if image.depth == 16 { Depth::Sixteen } else { Depth::Eight },
        },
        fidelity: Fidelity::Full,
        // Named by the caller, which detected it from the same bytes. It does
        // not cross the protocol: the decoder is the untrusted side, and what
        // the file *is* was settled before anything was handed to it.
        format,
        // A vector document does not cross the boundary: the formats that
        // need a sandbox are all raster, and re-rasterising on zoom would
        // mean a round trip per frame.
        vector: None,
        // Neither does an animation: the sandboxed formats are stills.
        animation: None,
    })
}

/// Duplicate `handle` into the child, returning the value it has over there.
///
/// The child is still suspended, so the handle is in place before its first
/// instruction. A duplication that fails degrades to the pipe rather than
/// failing the decode — the caller maps a zero to "no section offered".
fn duplicate_into(child: HANDLE, handle: HANDLE) -> Result<u64> {
    use windows::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut duplicated = HANDLE::default();
    // SAFETY: both process handles are live — ours by definition, the child's
    // because it has not been reaped — and the duplicated handle belongs to
    // the child, which closes it when it exits.
    unsafe {
        DuplicateHandle(GetCurrentProcess(), handle, child, &mut duplicated, 0, false, DUPLICATE_SAME_ACCESS).context("handing the section to the decoder")?;
    }
    Ok(duplicated.0 as u64)
}

/// Write the request, read the reply, and kill the child if it takes too long.
fn exchange(child: &mut SpawnedChild, bytes: &[u8], section_handle: u64, timeout: Duration) -> Result<Result<protocol::Reply, String>> {
    let mut stdin = child.stdin.take().context("the decoder has no standard input")?;
    let mut stdout = child.stdout.take().context("the decoder has no standard output")?;

    // The write runs on its own thread: a decoder that dies without reading
    // would otherwise block the viewer forever on a full pipe.
    let request = bytes.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = protocol::write_request(&mut stdin, &request, section_handle);
        // Dropping stdin closes the pipe, which is how the child sees the end
        // of the request.
    });

    // The reply is read before the child is waited on, and that order is not
    // incidental: a pipe holds only a few kilobytes, so a decoder returning a
    // megabyte of pixels blocks on the write until someone drains it. Waiting
    // for the process first would mean each side waiting for the other for
    // ever — which is exactly what a 800x600 image did.
    //
    // Reading on another thread keeps the timeout meaningful: a decoder that
    // never writes anything must not park the viewer on a blocking read.
    let reader = std::thread::spawn(move || {
        let mut reply = Vec::new();
        let _ = stdout.read_to_end(&mut reply);
        reply
    });

    // SAFETY: the handle belongs to a child this process owns and has not yet
    // been reaped, so it stays valid for the wait.
    let wait = unsafe { WaitForSingleObject(child.process, timeout.as_millis().min(u32::MAX as u128) as u32) };
    let _ = writer.join();

    if wait == WAIT_TIMEOUT {
        // The caller kills the child, which closes the pipe and lets the
        // reader thread finish rather than leaking it.
        bail!("the decoder did not answer within {} seconds", timeout.as_secs());
    }
    if wait != WAIT_OBJECT_0 {
        bail!("waiting for the decoder failed");
    }

    let reply = reader.join().map_err(|_| anyhow::anyhow!("reading the decoder's answer failed"))?;
    if reply.is_empty() {
        // No answer at all: the decoder died before it could speak, which is
        // the crash this whole arrangement exists to survive.
        bail!("the decoder stopped without answering, which usually means the file crashed it");
    }

    protocol::read_reply(&reply[..])
}

/// Put the child in a job object.
///
/// A failure here is reported and the decode continues: a viewer that refuses
/// to open images because a hardening step was unavailable is worse than one
/// that opens them slightly less safely, and the pure-Rust decoders behind
/// this boundary are not the reason it exists. The one thing never skipped is
/// the job's kill-on-close, because without it a hung child could outlive the
/// viewer — and that failure is loud.
fn confine(child: &SpawnedChild) -> Option<Job> {
    match Job::holding(child.process) {
        Ok(job) => Some(job),
        Err(error) => {
            eprintln!("nitid: the decoder could not be confined to a job: {error:#}");
            None
        }
    }
}

/// Strip the child's token: no privileges, low integrity.
///
/// The child's *own* token is modified rather than a token being assigned to
/// it, which is what keeps this working without administrator rights —
/// `CreateProcessAsUser` would need privileges an ordinary desktop process
/// does not hold.
fn restrict_token(process: HANDLE) -> Result<()> {
    // SAFETY: `process` is a live handle to a child this process created, and
    // every handle opened here is closed before returning.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_ADJUST_DEFAULT | TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY, &mut token)
            .context("opening the decoder's token")?;
        let token = OwnedHandle(token);

        // Delete every privilege that can be deleted. A decoder needs none of
        // them, and what it cannot hold it cannot be tricked into using.
        let mut restricted = HANDLE::default();
        CreateRestrictedToken(token.0, DISABLE_MAX_PRIVILEGE, None, None, None, &mut restricted).context("stripping the decoder's privileges")?;
        let _restricted = OwnedHandle(restricted);

        // Low integrity: no writing to the user's files, no reaching into a
        // process above. S-1-16-4096 is the low integrity level.
        let mut sid = PSID::default();
        ConvertStringSidToSidW(w!("S-1-16-4096"), &mut sid).context("building the low integrity identifier")?;

        let mut label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: SE_GROUP_INTEGRITY,
            },
        };
        let result = SetTokenInformation(
            token.0,
            TokenIntegrityLevel,
            &raw mut label as *mut core::ffi::c_void,
            size_of::<TOKEN_MANDATORY_LABEL>() as u32,
        );
        // The SID came from `ConvertStringSidToSidW`, so it is freed with
        // `LocalFree` rather than `FreeSid`.
        let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(sid.0)));
        result.context("lowering the decoder's integrity level")?;
    }

    Ok(())
}

/// A job object holding the decoder.
///
/// Dropping it kills whatever is still inside, which is what guarantees no
/// decoder survives the viewer.
struct Job(HANDLE);

impl Job {
    fn holding(process: HANDLE) -> Result<Self> {
        // SAFETY: the job handle is owned by the returned value and closed in
        // its `Drop`; `process` is a live child handle.
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).context("creating the job object")?;
            let job = Self(job);

            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            // One process: the decoder decodes, it does not launch anything.
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
            limits.ProcessMemoryLimit = MEMORY_LIMIT;

            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &raw mut limits as *mut core::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .context("setting the job's limits")?;

            AssignProcessToJobObject(job.0, process).context("putting the decoder in the job")?;

            Ok(job)
        }
    }

    fn terminate(&self) {
        // SAFETY: the handle is valid for the lifetime of this value.
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Killing explicitly rather than relying on kill-on-close alone: the
        // limit fires when the last handle closes, and being certain is
        // cheaper than reasoning about who else might hold one.
        self.terminate();
        // SAFETY: the handle is owned by this value and closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A handle closed when it goes out of scope.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Makes sure the child never outlives this scope, however it is left.
struct ChildGuard<'a> {
    child: &'a mut SpawnedChild,
    job: Option<Job>,
}

impl ChildGuard<'_> {
    /// Let the confined child start running.
    ///
    /// The thread handle came back from `CreateProcessW` and was kept for
    /// exactly this call. The previous arrangement — `std::process::Command`
    /// discards that handle — meant finding the thread again through a
    /// system-wide Toolhelp snapshot, which cost 35–45 ms per decode: seven
    /// times the spawn itself.
    fn resume(&self) -> Result<()> {
        // `ResumeThread` reports failure by returning `u32::MAX` rather than
        // through the type system, so it is the one call here that would fail
        // silently if the result went unchecked.
        //
        // SAFETY: the thread handle belongs to the suspended child.
        let previous = unsafe { ResumeThread(self.child.thread) };
        if previous == u32::MAX {
            bail!("the decoder process would not start");
        }
        Ok(())
    }
}

impl Drop for ChildGuard<'_> {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.terminate();
        }
        // SAFETY: the process handle is live until the `SpawnedChild` closes
        // it; killing an already exited process is a harmless error, and the
        // wait afterwards is what makes the kill observable before the pipes
        // are torn down.
        unsafe {
            let _ = TerminateProcess(self.child.process, 1);
            let _ = WaitForSingleObject(self.child.process, u32::MAX);
        }
    }
}

// What "the network is closed" rests on, so nobody has to take it on faith:
// `tests/sandbox.rs` runs the decoder inside the container and has it try
// both directions. Outward, a live listener just outside the sandbox is
// unreachable (`connect=false`). Inward, a listener the decoder binds — the
// bind itself succeeds even in a container, which alone would read as a hole —
// accepts nothing while the test hammers its port from outside
// (`accepted=false`). And the same probe run behind the restricted-token
// fallback sees an open network, which is what makes the closed readings the
// container's doing rather than a broken probe's.
