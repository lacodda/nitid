//! Asking for a file over HTTPS, with what Windows already has.
//!
//! WinHTTP rather than a client crate: it brings its own TLS, checks the
//! certificate against the machine's store, follows GitHub's redirect to where
//! the asset is stored, and honours the proxy the machine is set up with. A
//! crate would add a TLS stack to a viewer for the sake of one small request a
//! day, and leave the proxy to be configured all over again.
//!
//! Nothing here is linked into anything the viewer does: only the resident and
//! `nitid update` ask, and only for the two files [`super::feed`] names. The
//! request carries no version and no identifier.

use std::ffi::c_void;

use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Why a file did not arrive.
#[derive(Debug, thiserror::Error)]
pub enum Failure {
    /// No connection, no name resolution, a timeout: the ordinary state of a
    /// laptop on a train. Nothing to report, and worth asking again soon.
    #[error("no answer: {0}")]
    NoAnswer(String),
    /// Something answered and said no.
    #[error("the server answered {0}")]
    Status(u32),
    /// More bytes than the file was said to be.
    #[error("the answer is longer than the {0} bytes expected")]
    TooLong(u64),
}

/// How long each stage may take, in milliseconds. These limit the silence
/// between packets rather than the whole download, so a slow connection still
/// finishes and a dead one does not hold a thread forever.
const RESOLVE: i32 = 30_000;
const CONNECT: i32 = 30_000;
const SEND: i32 = 30_000;
const RECEIVE: i32 = 60_000;

/// A WinHTTP handle, closed however the request ends.
struct Handle(*mut c_void);

impl Handle {
    fn from_call(handle: *mut c_void, what: &str) -> Result<Self, Failure> {
        if handle.is_null() {
            Err(Failure::NoAnswer(format!("{what}: {}", windows::core::Error::from_thread())))
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from WinHTTP and is closed exactly once.
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// An address, taken apart the way WinHTTP asks for it.
#[derive(PartialEq, Eq, Debug)]
struct Address<'a> {
    secure: bool,
    host: &'a str,
    port: u16,
    path: &'a str,
}

fn address(url: &str) -> Option<Address<'_>> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    let slash = rest.find('/')?;
    let (authority, path) = rest.split_at(slash);
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, if secure { 443 } else { 80 }),
    };
    (!host.is_empty()).then_some(Address { secure, host, port, path })
}

/// Fetch `url`, refusing more than `limit` bytes.
pub fn get(url: &str, limit: u64) -> Result<Vec<u8>, Failure> {
    let Some(address) = address(url) else {
        return Err(Failure::NoAnswer(format!("{url} is not an address")));
    };

    // SAFETY: every handle is checked before use and closed by `Handle`; the
    // strings outlive the calls that read them, and the buffers are locals of
    // the size each call is told.
    unsafe {
        let session = Handle::from_call(
            WinHttpOpen(w!("nitid"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0),
            "opening a session",
        )?;
        let _ = WinHttpSetTimeouts(session.0, RESOLVE, CONNECT, SEND, RECEIVE);

        let connection = Handle::from_call(WinHttpConnect(session.0, &HSTRING::from(address.host), address.port, 0), "connecting")?;
        let flags = if address.secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let request = Handle::from_call(
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &HSTRING::from(address.path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            ),
            "making the request",
        )?;

        let failed = |what: &str, error: windows::core::Error| Failure::NoAnswer(format!("{what}: {error}"));
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(|error| failed("sending the request", error))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|error| failed("waiting for the answer", error))?;

        let mut status = 0u32;
        let mut length = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&raw mut status as *mut c_void),
            &raw mut length,
            std::ptr::null_mut(),
        )
        .map_err(|error| failed("reading the answer's status", error))?;
        if status != 200 {
            return Err(Failure::Status(status));
        }

        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, chunk.as_mut_ptr() as *mut c_void, chunk.len() as u32, &raw mut read)
                .map_err(|error| failed("reading the answer", error))?;
            if read == 0 {
                return Ok(body);
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() as u64 > limit {
                return Err(Failure::TooLong(limit));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_comes_apart_into_what_winhttp_asks_for() {
        assert_eq!(
            address("https://github.com/lacodda/nitid/releases/latest/download/update.txt"),
            Some(Address {
                secure: true,
                host: "github.com",
                port: 443,
                path: "/lacodda/nitid/releases/latest/download/update.txt",
            })
        );
        assert_eq!(
            address("http://127.0.0.1:8123/x"),
            Some(Address {
                secure: false,
                host: "127.0.0.1",
                port: 8123,
                path: "/x",
            })
        );
    }

    #[test]
    fn what_is_not_an_address_is_refused() {
        for wrong in ["", "github.com/x", "ftp://github.com/x", "https://github.com", "https:///x", "http://h:port/x"] {
            assert_eq!(address(wrong), None, "`{wrong}` was taken for an address");
        }
    }

    /// A server of this test's own: one answer, then the connection closes.
    fn serving(answer: &'static [u8]) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("its address").port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(answer);
            }
        });
        format!("http://127.0.0.1:{port}/x")
    }

    #[test]
    fn a_file_arrives_whole() {
        let url = serving(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
        assert_eq!(get(&url, 5).expect("the file"), b"hello");
    }

    /// A 404 with a body is not a file with that body in it.
    #[test]
    fn a_refusal_is_not_taken_for_the_file() {
        let url = serving(b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope");
        assert!(matches!(get(&url, 100), Err(Failure::Status(404))));
    }

    #[test]
    fn more_than_was_promised_is_refused() {
        let url = serving(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
        assert!(matches!(get(&url, 4), Err(Failure::TooLong(4))));
    }

    /// Nothing listening is the laptop on a train, not a broken release.
    #[test]
    fn nobody_there_is_no_answer() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("a free port")
            .port();
        assert!(matches!(get(&format!("http://127.0.0.1:{port}/x"), 100), Err(Failure::NoAnswer(_))));
    }
}
