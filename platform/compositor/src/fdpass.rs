//! Sending a client's shared-memory pool fd across the compositor socket via
//! `SCM_RIGHTS` (WWW-78) — the fd `pool.rs` says a real client sends
//! "alongside" its [`ClientHello`](crate::wire::ClientHello).
//!
//! Follows `pool.rs`'s pattern for this crate's other real system call:
//! `#![allow(unsafe_code)]` at the module, a written reason on every block.

#![allow(unsafe_code)]

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

// `cmsghdr`'s own alignment (not the byte array's) is what `CMSG_DATA`'s
// internal rounding assumes when it hands back a pointer to lay `fd` at. A
// plain `[u8; N]` has alignment 1, which would make that pointer only
// coincidentally aligned; this union forces the whole buffer to `cmsghdr`'s
// alignment so it never is just coincidence.
#[repr(C)]
union Control {
    bytes: [u8; CONTROL_LEN],
    _align: libc::cmsghdr,
}

const CONTROL_LEN: usize =
    // SAFETY: pure arithmetic (alignment/size rounding) — no syscall, no
    // pointer dereference. Every libc target this workspace builds for
    // implements it that way.
    unsafe { libc::CMSG_SPACE(mem::size_of::<RawFd>() as u32) as usize };

/// Sends `bytes` over `stream` with `fd` attached as `SCM_RIGHTS` ancillary
/// data, in one `sendmsg` call.
///
/// One call, not a `write` followed by a separate fd send: on a stream
/// socket, ancillary data is associated with whichever regular bytes are
/// sent in the *same* `sendmsg`, so a receiver reading `bytes` back over more
/// than one `recvmsg` would only see the fd attached to whichever chunk
/// happened to arrive first.
pub fn send_with_fd(stream: &UnixStream, bytes: &[u8], fd: RawFd) -> io::Result<()> {
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let mut control = Control {
        bytes: [0u8; CONTROL_LEN],
    };
    // SAFETY: `libc::msghdr` is a C struct of plain integers and pointers —
    // every bit pattern, including all zero, is one of its valid values —
    // and every field is overwritten below before `msg` is read.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    // SAFETY: `control` outlives this call (it is a local dropped only after
    // `sendmsg` returns), and nothing else holds a reference into it.
    msg.msg_control = unsafe { control.bytes.as_mut_ptr() }.cast();
    msg.msg_controllen = CONTROL_LEN as _;

    // SAFETY: `msg.msg_control`/`msg.msg_controllen` were just set above to
    // describe `control`, a live local sized by `CMSG_SPACE` to hold exactly
    // one `cmsghdr` plus one fd's worth of data, so `CMSG_FIRSTHDR` returns a
    // non-null pointer into it.
    let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    // SAFETY: `cmsg` was just shown non-null by construction (`CONTROL_LEN`
    // is exactly `CMSG_SPACE` of one fd, so `CMSG_FIRSTHDR` always finds
    // room for one header), and it points into `control`, which is still
    // alive.
    let cmsg = unsafe { cmsg.as_mut() }.expect("CONTROL_LEN fits exactly one cmsghdr");
    cmsg.cmsg_level = libc::SOL_SOCKET;
    cmsg.cmsg_type = libc::SCM_RIGHTS;
    // SAFETY: pure arithmetic, the same category as `CONTROL_LEN` above.
    cmsg.cmsg_len = unsafe { libc::CMSG_LEN(mem::size_of::<RawFd>() as u32) } as _;
    // SAFETY: `CMSG_DATA` on a `cmsghdr` sized as above returns a pointer
    // with room for one `RawFd` and the alignment `Control`'s `cmsghdr`
    // union member guarantees.
    unsafe { libc::CMSG_DATA(cmsg).cast::<RawFd>().write(fd) };

    // SAFETY: `msg` describes one iovec borrowing `bytes` for the duration
    // of this call (not moved) and one control message naming `fd`, which
    // the caller still owns — `sendmsg` duplicates the right rather than
    // consuming it, matching `fd: RawFd` (a borrow) rather than `OwnedFd`.
    let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, 0) };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Reads one message from `stream` into `buf`, returning the bytes read and
/// an fd if `SCM_RIGHTS` ancillary data came with it.
///
/// Only [`ClientHello`](crate::wire::ClientHello) ever carries an fd, so
/// every other caller gets back `None` and simply ignores it.
pub fn recv_with_fd(stream: &UnixStream, buf: &mut [u8]) -> io::Result<(usize, Option<OwnedFd>)> {
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let mut control = Control {
        bytes: [0u8; CONTROL_LEN],
    };
    // SAFETY: as `send_with_fd` — every field is overwritten below.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    // SAFETY: as `send_with_fd`, `control` is a live local outliving this
    // call.
    msg.msg_control = unsafe { control.bytes.as_mut_ptr() }.cast();
    msg.msg_controllen = CONTROL_LEN as _;

    // SAFETY: `iov`/`control` are live locals `msg`'s pointer fields above
    // point into; `recvmsg` only ever writes within the bounds each was
    // constructed with (`buf.len()`, `CONTROL_LEN`).
    let received = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) };
    if received < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `msg.msg_control`/`msg.msg_controllen` were set above and only
    // ever narrowed by `recvmsg`, never invalidated, so `CMSG_FIRSTHDR`
    // reads memory this function itself owns.
    let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    // SAFETY: `cmsg` is either null (handled by `as_ref` returning `None`)
    // or a pointer `CMSG_FIRSTHDR` derived from `msg.msg_control`, which
    // still points into `control`, a live local.
    let fd = match unsafe { cmsg.as_ref() } {
        // Checked before the data is trusted as a `RawFd`: a level/type this
        // socket did not ask for is a peer sending ancillary data other than
        // the one kind this wire defines, not a value to read as an fd.
        Some(cmsg) if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_RIGHTS => {
            // SAFETY: `cmsg` is the `SCM_RIGHTS` header `recvmsg` wrote, sized
            // for exactly one fd by `CONTROL_LEN`; `CMSG_DATA` returns a
            // pointer to that fd's bytes.
            let raw = unsafe { libc::CMSG_DATA(cmsg).cast::<RawFd>().read() };
            // SAFETY: `recvmsg` returning this cmsg means the kernel just
            // installed `raw` as a fresh, open fd in this process on our
            // behalf; nothing else in this function has taken ownership of
            // it yet.
            Some(unsafe { OwnedFd::from_raw_fd(raw) })
        }
        _ => None,
    };

    Ok((received as usize, fd))
}

#[cfg(test)]
mod tests {
    use super::{recv_with_fd, send_with_fd};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    /// Writing through the received fd and reading back through the
    /// original proves it is a real, independently usable duplicate of the
    /// same file — not just a copy of the integer that happened to decode to
    /// the sender's fd number.
    #[test]
    fn an_fd_sent_alongside_bytes_arrives_as_a_distinct_open_fd() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut payload = tempfile::tempfile().unwrap();

        send_with_fd(&a, b"hi", payload.as_raw_fd()).unwrap();

        let mut buf = [0u8; 16];
        let (len, fd) = recv_with_fd(&b, &mut buf).unwrap();
        assert_eq!(&buf[..len], b"hi");
        let received = fd.expect("an fd was attached");

        std::fs::File::from(received).write_all(b"ok!").unwrap();
        payload.seek(SeekFrom::Start(0)).unwrap();
        let mut contents = String::new();
        payload.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "ok!");
    }

    #[test]
    fn a_message_with_no_fd_attached_decodes_as_none() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut sender = &a;
        sender.write_all(b"hi").unwrap();

        let mut buf = [0u8; 16];
        let (len, fd) = recv_with_fd(&b, &mut buf).unwrap();
        assert_eq!(&buf[..len], b"hi");
        assert!(fd.is_none());
    }
}
