//! Real Windows hvsocket (AF_HYPERV) transport — guest listener + host dialer.
//! Replaces the tokio duplex in MockHyperVDriver for production + host tests.
#![cfg(windows)]

use std::io;
use std::os::windows::io::RawSocket;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const AF_HYPERV: u16 = 34;
const HV_PROTOCOL_RAW: u32 = 1;

#[repr(C)]
struct SockaddrHv {
    family: u16,
    reserved: u16,
    vm_id: [u8; 16],
    service_id: [u8; 16],
}

const GUID_ZERO: [u8; 16] = [0; 16];

/// Parse "3045196F-2A11-4D65-BCC7-3F9EAB09B7ED" → 16 bytes in Windows GUID
/// binary layout (Data1/Data2/Data3 little-endian, Data4 big-endian).
fn parse_guid(s: &str) -> io::Result<[u8; 16]> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("bad GUID: {s}")));
    }
    let bytes: Vec<u8> = (0..16)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16))
        .collect::<Result<_, _>>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut out = [0u8; 16];
    out[0] = bytes[3]; out[1] = bytes[2]; out[2] = bytes[1]; out[3] = bytes[0];
    out[4] = bytes[5]; out[5] = bytes[4];
    out[6] = bytes[7]; out[7] = bytes[6];
    out[8..16].copy_from_slice(&bytes[8..16]);
    Ok(out)
}

use windows_sys::Win32::Networking::WinSock::{
    bind, listen, accept, socket, connect, closesocket, WSAGetLastError,
    WSADATA, WSAStartup, INVALID_SOCKET, SOCKET_ERROR,
    SOCK_STREAM, ioctlsocket, recv, send,
};
const FIONBIO: i32 = 0x8004667eu32 as i32;

fn wsa_init() -> io::Result<()> {
    unsafe {
        let mut data: WSADATA = std::mem::zeroed();
        let r = WSAStartup(0x0202, &mut data);
        if r != 0 { return Err(io::Error::from_raw_os_error(r)); }
    }
    Ok(())
}

fn last_wsa_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}

fn set_nonblocking(sock: usize) -> io::Result<()> {
    let mut mode: u32 = 1;
    let r = unsafe { ioctlsocket(sock, FIONBIO, &mut mode) };
    if r == SOCKET_ERROR { Err(last_wsa_error()) } else { Ok(()) }
}

/// A listener bound to an AF_HYPERV service GUID (guest side).
pub struct HvSocketListener {
    sock: RawSocket,
}

impl HvSocketListener {
    /// Bind + listen on the service GUID. Call inside the guest VM.
    pub fn bind(service_guid: &str) -> io::Result<Self> {
        wsa_init()?;
        let service_id = parse_guid(service_guid)?;
        unsafe {
            let s = socket(AF_HYPERV as i32, SOCK_STREAM as i32, HV_PROTOCOL_RAW as i32);
            if s == INVALID_SOCKET { return Err(last_wsa_error()); }
            let addr = SockaddrHv {
                family: AF_HYPERV, reserved: 0,
                vm_id: GUID_ZERO, service_id,
            };
            if bind(s, &addr as *const _ as *const _, std::mem::size_of::<SockaddrHv>() as i32) == SOCKET_ERROR {
                let e = last_wsa_error(); closesocket(s); return Err(e);
            }
            if listen(s, 1) == SOCKET_ERROR {
                let e = last_wsa_error(); closesocket(s); return Err(e);
            }
            Ok(HvSocketListener { sock: s as RawSocket })
        }
    }

    /// Accept one incoming connection (blocks until a host connects).
    pub fn accept(&self) -> io::Result<HvRawStream> {
        unsafe {
            let mut addr: SockaddrHv = std::mem::zeroed();
            let mut len = std::mem::size_of::<SockaddrHv>() as i32;
            let s = accept(self.sock as _, &mut addr as *mut _ as *mut _, &mut len);
            if s == INVALID_SOCKET { return Err(last_wsa_error()); }
            set_nonblocking(s)?;
            Ok(HvRawStream { sock: s })
        }
    }
}

impl Drop for HvSocketListener {
    fn drop(&mut self) { unsafe { closesocket(self.sock as _); } }
}

/// Connect to a guest's hvsocket from the host.
pub fn connect_hvsocket(vm_guid: &str, service_guid: &str) -> io::Result<HvRawStream> {
    wsa_init()?;
    let vm_id = parse_guid(vm_guid)?;
    let service_id = parse_guid(service_guid)?;
    unsafe {
        let s = socket(AF_HYPERV as i32, SOCK_STREAM as i32, HV_PROTOCOL_RAW as i32);
        if s == INVALID_SOCKET { return Err(last_wsa_error()); }
        let addr = SockaddrHv {
            family: AF_HYPERV, reserved: 0,
            vm_id, service_id,
        };
        if connect(s, &addr as *const _ as *const _, std::mem::size_of::<SockaddrHv>() as i32) == SOCKET_ERROR {
            let e = last_wsa_error(); closesocket(s); return Err(e);
        }
        set_nonblocking(s)?;
        Ok(HvRawStream { sock: s })
    }
}

/// A raw non-blocking AF_HYPERV stream (AsyncRead + AsyncWrite).
pub struct HvRawStream {
    sock: usize,
}

impl AsyncRead for HvRawStream {
    fn poll_read(self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        unsafe {
            let unfilled = buf.initialize_unfilled();
            let r = recv(self.sock, unfilled.as_mut_ptr() as *mut _, unfilled.len() as i32, 0);
            if r == SOCKET_ERROR {
                let e = last_wsa_error();
                if e.kind() == io::ErrorKind::WouldBlock { return Poll::Pending; }
                return Poll::Ready(Err(e));
            }
            if r == 0 { return Poll::Ready(Ok(())); }
            buf.advance(r as usize);
            Poll::Ready(Ok(()))
        }
    }
}

impl AsyncWrite for HvRawStream {
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        unsafe {
            let r = send(self.sock, buf.as_ptr() as *const _, buf.len() as i32, 0);
            if r == SOCKET_ERROR {
                let e = last_wsa_error();
                if e.kind() == io::ErrorKind::WouldBlock { return Poll::Pending; }
                return Poll::Ready(Err(e));
            }
            Poll::Ready(Ok(r as usize))
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> { Poll::Ready(Ok(())) }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> { Poll::Ready(Ok(())) }
}

impl Drop for HvRawStream {
    fn drop(&mut self) { unsafe { closesocket(self.sock); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_guid_roundtrips() {
        let g = parse_guid("3045196F-2A11-4D65-BCC7-3F9EAB09B7ED").unwrap();
        assert_eq!(&g[0..4], &[0x6F, 0x19, 0x45, 0x30]); // LE first field
    }
}

