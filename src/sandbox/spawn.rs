//! Launching the decoder process by hand.
//!
//! `std::process::Command` cannot express the two things this launch needs.
//! An AppContainer is asked for through a `PROC_THREAD_ATTRIBUTE_LIST`, which
//! `Command` has no stable door to. And `Command` discards the initial
//! thread's handle, which forced resuming a suspended child through a
//! system-wide Toolhelp thread walk — measured at 35–45 ms per decode, seven
//! times the spawn itself.
//!
//! Calling `CreateProcessW` directly buys both: the attribute list carries the
//! container identity, and the thread handle comes back in
//! `PROCESS_INFORMATION`, making the resume a single cheap call.
//!
//! The pipe handles are passed through `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`
//! rather than blanket inheritance. Inheritance is process-wide: two decodes
//! spawning concurrently would each leak their handles into the other's
//! child.

use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};
use windows::Win32::Security::Isolation::{CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName};
use windows::Win32::Security::{PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};
use windows::core::{HSTRING, PWSTR};

/// The AppContainer profile the decoder runs in.
///
/// One name for the machine: the profile is registered on first use and
/// reused ever after, including across versions — its SID is what `nitid
/// install` grants read access to.
pub const CONTAINER_NAME: &str = "lacodda.nitid.decoder";

/// The decoder's container identity: a SID and nothing else.
///
/// No capabilities — the container grants the decoder exactly none of the
/// brokered rights, which is the point: an AppContainer without capabilities
/// cannot open a socket, which is the network isolation a low integrity
/// token was measured not to provide.
pub struct AppContainer {
    sid: PSID,
}

// SAFETY: the SID is an allocation, not a thread resource; it is only read
// after creation.
unsafe impl Send for AppContainer {}
unsafe impl Sync for AppContainer {}

impl AppContainer {
    /// Register the profile, or find the one already registered.
    pub fn prepare() -> Result<Self> {
        let name = HSTRING::from(CONTAINER_NAME);
        // SAFETY: the strings outlive the calls; the SID is owned by the
        // returned value.
        unsafe {
            let sid = match CreateAppContainerProfile(&name, &HSTRING::from("nitid decoder"), &HSTRING::from("Confines nitid's image decoder"), None) {
                Ok(sid) => sid,
                // Registered by an earlier run: the profile carries no state,
                // so the existing one is exactly as good.
                Err(error) if error.code() == ERROR_ALREADY_EXISTS.to_hresult() => {
                    DeriveAppContainerSidFromAppContainerName(&name).context("finding the decoder's container profile")?
                }
                Err(error) => return Err(error).context("registering the decoder's container profile"),
            };
            Ok(Self { sid })
        }
    }

    /// The container's identifier, carried into the launch attributes.
    pub fn sid(&self) -> PSID {
        self.sid
    }
}

/// Remove the container profile from the machine.
///
/// For `nitid uninstall`: the profile is machine state nitid created, so
/// taking nitid off the machine takes the profile too. A viewer run after
/// that simply registers it again on its first decode.
pub fn delete_profile() -> Result<()> {
    use windows::Win32::Security::Isolation::DeleteAppContainerProfile;

    // SAFETY: plain call over an owned string.
    unsafe { DeleteAppContainerProfile(&HSTRING::from(CONTAINER_NAME)).context("removing the decoder's container profile") }
}

impl Drop for AppContainer {
    fn drop(&mut self) {
        // SAFETY: the SID came from the profile functions, which allocate
        // with the SID allocator.
        unsafe {
            windows::Win32::Security::FreeSid(self.sid);
        }
    }
}

/// A confined child mid-launch: created suspended, resumed by the caller once
/// every restriction is in place.
pub struct SpawnedChild {
    pub process: HANDLE,
    pub thread: HANDLE,
    /// The viewer's end of the child's standard input.
    pub stdin: Option<File>,
    /// The viewer's end of the child's standard output.
    pub stdout: Option<File>,
}

impl Drop for SpawnedChild {
    fn drop(&mut self) {
        // The caller has already killed or waited for the process; what is
        // owed here is only the handles.
        //
        // SAFETY: both handles came from `CreateProcessW` and are closed once.
        unsafe {
            let _ = CloseHandle(self.thread);
            let _ = CloseHandle(self.process);
        }
    }
}

/// Launch `exe --decode-stdin`, suspended, with pipes for stdin and stdout —
/// inside `container` when one is given.
pub fn spawn(exe: &Path, argument: &str, container: Option<&AppContainer>) -> Result<SpawnedChild> {
    // Both pipes are born inheritable and the parent ends are then taken back
    // out of inheritance: only the child ends may cross.
    let (child_stdin, parent_stdin) = pipe().context("creating the decoder's input pipe")?;
    let (parent_stdout, child_stdout) = pipe().context("creating the decoder's output pipe")?;
    uninherit(parent_stdin.0)?;
    uninherit(parent_stdout.0)?;

    // The command line: "exe" --decode-stdin. CreateProcessW may write into
    // the buffer, so it is a mutable Vec rather than a borrowed string.
    let mut command_line: Vec<u16> = Vec::new();
    command_line.push(b'"' as u16);
    command_line.extend(exe.as_os_str().encode_wide());
    command_line.push(b'"' as u16);
    command_line.push(b' ' as u16);
    command_line.extend(argument.encode_utf16());
    command_line.push(0);

    let exe_wide = HSTRING::from(exe.as_os_str());

    // The handles the child may inherit: exactly its ends of the two pipes.
    let mut inheritable = [child_stdin.0, child_stdout.0];

    // The container identity, when there is one. This struct is pointed into
    // by the attribute list, so it must stay alive until `CreateProcessW` has
    // returned — as must the attribute buffer itself, which is why both are
    // plain locals rather than temporaries.
    let mut capabilities = SECURITY_CAPABILITIES::default();
    if let Some(container) = container {
        capabilities.AppContainerSid = container.sid();
        capabilities.CapabilityCount = 0;
    }

    // SAFETY: every pointer handed to the attribute list and to
    // `CreateProcessW` refers to a local that outlives the call; the
    // attribute list is deleted before the buffer is dropped.
    unsafe {
        let attribute_count = if container.is_some() { 2 } else { 1 };

        let mut size = 0usize;
        // The sizing call reports "insufficient buffer" by design; the size
        // is what it was asked for.
        let _ = InitializeProcThreadAttributeList(None, attribute_count, None, &mut size);
        let mut buffer = vec![0u8; size];
        let attributes = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr() as *mut core::ffi::c_void);
        InitializeProcThreadAttributeList(Some(attributes), attribute_count, None, &mut size).context("initialising the launch attributes")?;
        // Deleted on every path out from here.
        let attributes = AttributeList(attributes);

        windows::Win32::System::Threading::UpdateProcThreadAttribute(
            attributes.0,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(inheritable.as_mut_ptr() as *const core::ffi::c_void),
            size_of_val(&inheritable),
            None,
            None,
        )
        .context("limiting the handles the decoder inherits")?;

        if container.is_some() {
            windows::Win32::System::Threading::UpdateProcThreadAttribute(
                attributes.0,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                Some(&raw const capabilities as *const core::ffi::c_void),
                size_of::<SECURITY_CAPABILITIES>(),
                None,
                None,
            )
            .context("attaching the container identity")?;
        }

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = child_stdin.0;
        startup.StartupInfo.hStdOutput = child_stdout.0;
        // No stderr: the decoder speaks the protocol on stdout and nothing
        // reads its stderr, exactly as before this module existed.
        startup.lpAttributeList = attributes.0;

        let mut information = PROCESS_INFORMATION::default();
        CreateProcessW(
            &exe_wide,
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            // Inheritance is on, and the attribute list above narrows it to
            // the two pipe ends.
            true,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_NO_WINDOW,
            None,
            None,
            &startup.StartupInfo,
            &mut information,
        )
        .context("starting the decoder process")?;

        Ok(SpawnedChild {
            process: information.hProcess,
            thread: information.hThread,
            stdin: Some(File::from_raw_handle(parent_stdin.take().0.0)),
            stdout: Some(File::from_raw_handle(parent_stdout.take().0.0)),
        })
    }
}

/// An inheritable anonymous pipe, both ends owned until handed over.
fn pipe() -> Result<(PipeEnd, PipeEnd)> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };

    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // SAFETY: the out-parameters are locals; the returned owners close them.
    unsafe {
        CreatePipe(&mut read, &mut write, Some(&attributes), 0)?;
    }
    Ok((PipeEnd(read), PipeEnd(write)))
}

/// Take a pipe end back out of inheritance: the parent's ends stay home.
fn uninherit(handle: HANDLE) -> Result<()> {
    // SAFETY: the handle is a live pipe end owned by this function's caller.
    unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)).context("keeping a pipe end out of inheritance") }
}

/// A pipe end that closes itself unless taken.
struct PipeEnd(HANDLE);

impl PipeEnd {
    /// Hand the handle over; the new owner closes it.
    fn take(mut self) -> TakenEnd {
        let handle = self.0;
        self.0 = HANDLE::default();
        TakenEnd(handle)
    }
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            // SAFETY: the handle is owned by this value and closed once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// A handle whose ownership has moved on — a plain value, no `Drop`.
struct TakenEnd(HANDLE);

/// The attribute list, deleted when this scope is left.
struct AttributeList(LPPROC_THREAD_ATTRIBUTE_LIST);

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: the list was initialised on this pointer.
        unsafe {
            DeleteProcThreadAttributeList(self.0);
        }
    }
}
