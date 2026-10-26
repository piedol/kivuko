//! The utun interface macOS creates for the tunnel, as a packet source and sink for GotaTun.
//!
//! NEPacketTunnelProvider doesn't hand out the interface's file descriptor, so we find it the
//! way wireguard-apple does: look for the socket connected to the utun kernel control. Packets
//! on a utun carry a 4-byte address-family header, which we add and strip here.

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;

use gotatun::packet::{Ip, Packet, PacketBufPool};
use gotatun::tun::{IpRecv, IpSend, MtuWatcher};
use tokio::io::unix::AsyncFd;

/// Find the file descriptor of this process's utun control socket.
pub fn find_fd() -> Option<RawFd> {
    let mut info: libc::ctl_info = unsafe { std::mem::zeroed() };
    for (dst, src) in info.ctl_name.iter_mut().zip(b"com.apple.net.utun_control") {
        *dst = *src as libc::c_char;
    }
    for fd in 0..1024 {
        let mut addr: libc::sockaddr_ctl = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::sockaddr_ctl>() as libc::socklen_t;
        let ok = unsafe { libc::getpeername(fd, (&raw mut addr).cast(), &mut len) } == 0;
        if !ok || i32::from(addr.sc_family) != libc::AF_SYSTEM {
            continue;
        }
        if info.ctl_id == 0 && unsafe { libc::ioctl(fd, libc::CTLIOCGINFO, &mut info) } != 0 {
            continue;
        }
        if addr.sc_id == info.ctl_id {
            return Some(fd);
        }
    }
    None
}

/// A descriptor we use but don't own: macOS closes it when the tunnel stops.
struct Borrowed(RawFd);

impl AsRawFd for Borrowed {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

#[derive(Clone)]
pub struct Utun {
    io: Arc<AsyncFd<Borrowed>>,
    mtu: u16,
}

impl Utun {
    pub fn new(fd: RawFd, mtu: u16) -> io::Result<Self> {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // Safety: macOS keeps the utun descriptor open for as long as the tunnel runs, and
        // the device (and this registration) is dropped before the tunnel stops.
        let io = unsafe { AsyncFd::register(Borrowed(fd)) }.map_err(std::io::Error::from)?;
        Ok(Utun { io: Arc::new(io), mtu })
    }
}

fn family_header(packet: &[u8]) -> [u8; 4] {
    let family = if packet.first().map(|b| b >> 4) == Some(6) { libc::AF_INET6 } else { libc::AF_INET };
    (family as u32).to_be_bytes()
}

impl IpSend for Utun {
    async fn send(&mut self, packet: Packet<Ip>) -> io::Result<()> {
        let packet = packet.into_bytes();
        let data: &[u8] = packet.as_ref();
        let header = family_header(data);
        loop {
            let mut guard = self.io.writable().await?;
            let result = guard.try_io(|fd| {
                let iov = [
                    libc::iovec { iov_base: header.as_ptr() as *mut _, iov_len: header.len() },
                    libc::iovec { iov_base: data.as_ptr() as *mut _, iov_len: data.len() },
                ];
                let n = unsafe { libc::writev(fd.as_raw_fd(), iov.as_ptr(), 2) };
                if n < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
            });
            if let Ok(r) = result {
                return r;
            }
        }
    }
}

impl IpRecv for Utun {
    async fn recv<'a>(
        &'a mut self,
        pool: &mut PacketBufPool,
    ) -> io::Result<impl Iterator<Item = Packet<Ip>> + Send + 'a> {
        let mut packet = pool.get();
        let mut header = [0u8; 4];
        let n = loop {
            let mut guard = self.io.readable().await?;
            let result = guard.try_io(|fd| {
                let buf: &mut [u8] = &mut packet;
                let iov = [
                    libc::iovec { iov_base: header.as_mut_ptr().cast(), iov_len: header.len() },
                    libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() },
                ];
                let n = unsafe { libc::readv(fd.as_raw_fd(), iov.as_ptr(), 2) };
                if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
            });
            if let Ok(r) = result {
                break r?;
            }
        };
        if n <= header.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read from utun"));
        }
        packet.truncate(n - header.len());
        packet.try_into_ip().map(std::iter::once).map_err(|e| io::Error::other(e.to_string()))
    }

    fn mtu(&self) -> MtuWatcher {
        MtuWatcher::new(self.mtu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_matches_ip_version() {
        assert_eq!(family_header(&[0x45, 0]), (libc::AF_INET as u32).to_be_bytes());
        assert_eq!(family_header(&[0x60, 0]), (libc::AF_INET6 as u32).to_be_bytes());
    }

    /// Round trip through a socket pair standing in for the utun.
    #[tokio::test]
    async fn sends_and_receives_with_headers() {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) }, 0);
        let mut a = Utun::new(fds[0], 1420).unwrap();
        let mut b = Utun::new(fds[1], 1420).unwrap();
        // A minimal IPv4 header: version 4, IHL 5, total length 20.
        let mut ip = [0u8; 20];
        ip[0] = 0x45;
        ip[3] = 20;
        ip[8] = 64;
        let packet = Packet::from_bytes(bytes::BytesMut::from(&ip[..])).try_into_ip().unwrap();
        a.send(packet).await.unwrap();
        let mut pool = PacketBufPool::new(4);
        let mut got: Vec<_> = b.recv(&mut pool).await.unwrap().collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got.remove(0).into_bytes().as_ref(), &ip[..]);
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
