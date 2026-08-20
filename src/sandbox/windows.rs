//! Confining the decoder process on Windows.
//!
//! Three mechanisms, each covering what the others do not:
//!
//! - **A job object** with kill-on-close, so the child cannot outlive the
//!   viewer however it exits, plus a memory cap and a process count of one, so
//!   it cannot allocate the machine to death or launch anything.
//! - **A low integrity level**, so it cannot write to the user's files or open
//!   a handle to any process above it.
//! - **A restricted token** with every removable privilege deleted, so nothing
//!   it inherited from the viewer is available to it.
//!
//! The order matters: the process is created suspended, confined while it
//! cannot run a single instruction, and only then resumed. Confining a running
//! process is a race, not a boundary.
//!
//! What this deliberately does *not* claim is network isolation. A low
//! integrity process can still open a socket — that is a common
//! misconception, and pretending otherwise in a comment would be worse than
//! the gap itself. Shutting the network needs an AppContainer or a firewall
//! rule; see the note at the end of this module.

use std::io::Read;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
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
use windows::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenProcessToken, ResumeThread, WaitForSingleObject};
use windows::core::{PCWSTR, w};

use super::protocol;
use crate::image_source::{DecodedImage, Fidelity, LoadedImage, Orientation};

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

/// Decode `bytes` in a confined child process.
pub fn decode(bytes: &[u8], timeout: Duration) -> Result<LoadedImage> {
    let mut child = Command::new(super::decoder_executable()?)
        .arg(super::DECODE_ARGUMENT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Suspended, so the confinement below is in place before the child
        // runs anything at all; no window, so a decode does not flash a
        // console over the picture.
        .creation_flags(CREATE_SUSPENDED.0 | CREATE_NO_WINDOW.0)
        .spawn()
        .context("starting the decoder process")?;

    // From here on the child must not be left suspended and running loose: a
    // guard kills it on every path out, including a panic.
    let job = confine(&child);
    let guard = ChildGuard { child: &mut child, job };

    guard.resume().context("resuming the decoder process")?;

    let reply = exchange(guard.child, bytes, timeout);

    // The child has answered or been killed; either way it is finished with.
    drop(guard);

    match reply? {
        Ok(image) => Ok(LoadedImage {
            orientation: Orientation::from_exif(u16::from(image.orientation)),
            // A profile the decoder sent that will not parse here is treated
            // as no profile, the same as anywhere else: broken colour metadata
            // is not a reason to refuse an image.
            profile: (!image.profile.is_empty()).then(|| ColorProfile::new_from_slice(&image.profile).ok()).flatten(),
            image: DecodedImage {
                width: image.width,
                height: image.height,
                pixels: image.pixels,
            },
            fidelity: Fidelity::Full,
            // A vector document does not cross the boundary: the formats that
            // need a sandbox are all raster, and re-rasterising on zoom would
            // mean a round trip per frame.
            vector: None,
        }),
        // A file the decoder could not read is an ordinary failure carrying
        // the decoder's own words, not a sandbox event.
        Err(message) => bail!("{message}"),
    }
}

/// Write the request, read the reply, and kill the child if it takes too long.
fn exchange(child: &mut Child, bytes: &[u8], timeout: Duration) -> Result<Result<protocol::RawImage, String>> {
    let mut stdin = child.stdin.take().context("the decoder has no standard input")?;
    let mut stdout = child.stdout.take().context("the decoder has no standard output")?;

    // The write runs on its own thread: a decoder that dies without reading
    // would otherwise block the viewer forever on a full pipe.
    let request = bytes.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = protocol::write_request(&mut stdin, &request);
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

    let handle = HANDLE(child.as_raw_handle());
    // SAFETY: the handle belongs to a child this process owns and has not yet
    // been reaped, so it stays valid for the wait.
    let wait = unsafe { WaitForSingleObject(handle, timeout.as_millis().min(u32::MAX as u128) as u32) };
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

/// Put the child in a job object and drop what its token can do.
///
/// Failures here are reported and the decode continues: a viewer that refuses
/// to open images because a hardening step was unavailable is worse than one
/// that opens them slightly less safely, and the pure-Rust decoders behind
/// this boundary are not the reason it exists. The one thing never skipped is
/// the job's kill-on-close, because without it a hung child could outlive the
/// viewer — and that failure is loud.
fn confine(child: &Child) -> Option<Job> {
    let handle = HANDLE(child.as_raw_handle());

    if let Err(error) = restrict_token(handle) {
        eprintln!("nitid: the decoder could not be restricted: {error:#}");
    }

    match Job::holding(handle) {
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
    child: &'a mut Child,
    job: Option<Job>,
}

impl ChildGuard<'_> {
    /// Let the confined child start running.
    fn resume(&self) -> Result<()> {
        // `ResumeThread` reports failure by returning `u32::MAX` rather than
        // through the type system, so it is the one call here that would fail
        // silently if the result went unchecked.
        //
        // SAFETY: the thread handle belongs to the suspended child.
        let previous = unsafe { ResumeThread(main_thread(self.child)?) };
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
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The handle of the child's initial thread.
///
/// `std` keeps the process handle but closes the thread handle it was given,
/// so the thread is reopened by identifier to resume it.
fn main_thread(child: &Child) -> Result<HANDLE> {
    use windows::Win32::System::Threading::{OpenThread, THREAD_SUSPEND_RESUME};

    // The initial thread of a process created suspended is the only one it
    // has, so the first thread of that process is the one to resume.
    let thread_id = first_thread_of(child.id()).context("finding the decoder's thread")?;

    // SAFETY: the identifier names a thread of a child this process owns.
    let handle = unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, thread_id) }.context("opening the decoder's thread")?;
    Ok(handle)
}

/// The first thread identifier belonging to `process_id`.
fn first_thread_of(process_id: u32) -> Result<u32> {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next};

    // SAFETY: the snapshot handle is closed before returning on every path.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).context("listing threads")?;
        let snapshot = OwnedHandle(snapshot);

        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };

        if Thread32First(snapshot.0, &mut entry).is_ok() {
            loop {
                if entry.th32OwnerProcessID == process_id {
                    return Ok(entry.th32ThreadID);
                }
                if Thread32Next(snapshot.0, &mut entry).is_err() {
                    break;
                }
            }
        }
    }

    bail!("the decoder process has no threads")
}

// Not done here, and worth saying plainly rather than leaving to be assumed:
// the network is still reachable from the decoder. That is no longer a
// suspicion — a decoder taught to try reported `listen=true connect=true` from
// inside this boundary, so a low integrity token demonstrably does not close a
// socket.
//
// Closing it needs an AppContainer with no capabilities, which was attempted
// in v0.9.0 and turned out to be a stage of its own: the container needs its
// own SID granted access not only to the executable but along every directory
// leading to it, which means the viewer editing permissions on folders it does
// not own. That work has its own version in the plan. Until then the gap costs
// nothing measurable — every decoder behind this boundary is Rust and none
// opens a socket — but it is a gap, and it is written down rather than papered
// over.
