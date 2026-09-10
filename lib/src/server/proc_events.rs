//! Linux process-event watcher (netlink `cn_proc`): EXEC/EXIT in real time.
//!
//! The periodic `/proc` scan stays as the source of truth (and the only
//! path off-Linux), but it is blind between ticks: game START waits up to
//! a full interval, game EXIT waits just as long to clear. This watcher
//! closes both gaps without polling:
//!
//! - `EXEC(pid)`: classify that ONE process immediately (same
//!   [`crate::server::process::ProcessServer::match_process`] as the scan
//!   loop) and emit hits at once — cards appear in milliseconds.
//! - `EXIT(pid)`: when a tracked game pid dies, wake the scan loop early
//!   so the natural full scan clears the slot at once instead of waiting
//!   out the interval.
//!
//! Best-effort by design: setup failure (hardened kernel, containers,
//! missing `cn_proc`) logs once and leaves pure polling in charge —
//! never an error, never a panic. Steady-state cost is ~zero syscalls
//! (one blocking `recv`); per-EXEC cost is one cmdline read plus the
//! normal classify chain, misses included. Build storms (`cargo build`
//! forks thousands of short-lived processes) only cost cmdline reads +
//! AC probes, still orders of magnitude below a full `/proc` sweep.

use std::sync::mpsc;

use crate::log;

/// Netlink family for the kernel connector multiplexer.
const NETLINK_CONNECTOR: i32 = 11;
/// `cn_proc` identifiers (`linux/connector.h`: `CN_IDX_PROC`, `CN_VAL_PROC`).
const CN_IDX_PROC: u32 = 0x1;
const CN_VAL_PROC: u32 = 0x1;
/// `PROC_CN_MCAST_LISTEN`: subscribe this socket to process events.
const PROC_CN_MCAST_LISTEN: u32 = 0x1;
/// `proc_cn_event` discriminants we act on (`linux/cn_proc.h`).
const PROC_EVENT_EXEC: i32 = 0x0000_0002;
const PROC_EVENT_EXIT: i32 = 0x0000_0100;
/// `struct cn_msg` header size: idx + val + seq + ack (u32) + len + flags (u16).
const CN_MSG_HEADER: usize = 20;
/// `struct proc_event` fixed prefix: what (i32) + cpu (u32) + timestamp (u64).
const PROC_EVENT_PREFIX: usize = 16;
/// Offset of `process_pid` inside the exec/exit union members, from the
/// START of the datagram (past the `cn_msg` header).
const PROC_EVENT_PID_OFFSET: usize = CN_MSG_HEADER + PROC_EVENT_PREFIX;

/// Lifecycle event worth waking up for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcEvent {
  Exec(u64),
  Exit(u64),
}

/// Parse one netlink datagram into a lifecycle event. Pure: every boundary
/// is checked, unknown/short/corrupt input is `None` (the watcher skips
/// it — the periodic scan is the backstop, so a dropped event only costs
/// latency, never correctness).
pub(crate) fn parse_event(buf: &[u8]) -> Option<ProcEvent> {
  if buf.len() < CN_MSG_HEADER + PROC_EVENT_PREFIX + 4 {
    return None;
  }
  let idx = u32::from_le_bytes(buf[0..4].try_into().ok()?);
  let val = u32::from_le_bytes(buf[4..8].try_into().ok()?);
  if idx != CN_IDX_PROC || val != CN_VAL_PROC {
    return None;
  }
  let what = i32::from_le_bytes(buf[CN_MSG_HEADER..CN_MSG_HEADER + 4].try_into().ok()?);
  let pid = u32::from_le_bytes(
    buf[PROC_EVENT_PID_OFFSET..PROC_EVENT_PID_OFFSET + 4]
      .try_into()
      .ok()?,
  ) as u64;
  match what {
    PROC_EVENT_EXEC => Some(ProcEvent::Exec(pid)),
    PROC_EVENT_EXIT => Some(ProcEvent::Exit(pid)),
    _ => None,
  }
}

/// Subscribe a netlink connector socket to `cn_proc` broadcasts.
/// Returns the fd, or a message when the kernel refuses (caller falls
/// back to polling).
fn subscribe() -> Result<i32, String> {
  // SAFETY: socket/bind/send/recv/close are called with valid arguments;
  // the fd is closed by the caller on every path (see `watch`).
  let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_DGRAM, NETLINK_CONNECTOR) };
  if fd < 0 {
    return Err(format!("socket failed: {}", last_os_error()));
  }
  // Bind with our pid AND join the cn_proc multicast group: the
  // PROC_CN_MCAST_LISTEN message below tells the kernel to start
  // multicasting, but only group members receive the broadcasts.
  let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
  addr.nl_family = libc::AF_NETLINK as u16;
  addr.nl_pid = unsafe { libc::getpid() } as u32;
  addr.nl_groups = CN_IDX_PROC;
  let bind_result = unsafe {
    libc::bind(
      fd,
      &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
      std::mem::size_of::<libc::sockaddr_nl>() as u32,
    )
  };
  if bind_result != 0 {
    let err = last_os_error();
    unsafe {
      libc::close(fd);
    }
    return Err(format!("bind failed: {err}"));
  }
  // PROC_CN_MCAST_LISTEN message: cn_msg header + one u32 payload.
  let mut message = [0u8; CN_MSG_HEADER + 4];
  message[0..4].copy_from_slice(&CN_IDX_PROC.to_le_bytes());
  message[4..8].copy_from_slice(&CN_VAL_PROC.to_le_bytes());
  // seq + ack stay zero; len = 4, flags = 0.
  message[16..18].copy_from_slice(&4u16.to_le_bytes());
  message[CN_MSG_HEADER..CN_MSG_HEADER + 4].copy_from_slice(&PROC_CN_MCAST_LISTEN.to_le_bytes());
  let sent = unsafe {
    libc::send(
      fd,
      message.as_ptr() as *const libc::c_void,
      message.len(),
      0,
    )
  };
  if sent != message.len() as isize {
    let err = last_os_error();
    unsafe {
      libc::close(fd);
    }
    return Err(format!("subscribe failed: {err}"));
  }
  Ok(fd)
}

fn last_os_error() -> String {
  std::io::Error::last_os_error().to_string()
}

/// Set (or clear) the receive timeout on the netlink fd.
fn set_recv_timeout(fd: i32, timeout: Option<std::time::Duration>) {
  let (secs, usecs) = match timeout {
    Some(duration) => (
      duration.as_secs() as libc::time_t,
      duration.subsec_micros() as libc::suseconds_t,
    ),
    None => (0, 0),
  };
  let timeval = libc::timeval {
    tv_sec: secs,
    tv_usec: usecs,
  };
  // SAFETY: setsockopt with a valid timeval pointer and length.
  unsafe {
    libc::setsockopt(
      fd,
      libc::SOL_SOCKET,
      libc::SO_RCVTIMEO,
      &timeval as *const libc::timeval as *const libc::c_void,
      std::mem::size_of::<libc::timeval>() as u32,
    );
  }
}

/// Prove the subscription actually delivers: spawn a trivial child (its
/// exec postdates our subscribe) and wait up to a second for ANY valid
/// proc event. Some kernels/LSMs accept the LISTEN message yet deliver
/// nothing — without this check the watcher would idle forever claiming
/// to be live while polling does all the work unnoticed.
fn self_test(fd: i32) -> bool {
  // Bound every recv below: without this, a silently non-delivering
  // kernel hangs the watcher thread forever with zero logs.
  set_recv_timeout(fd, Some(std::time::Duration::from_secs(1)));
  let probe = ["/bin/true", "/usr/bin/true"]
    .iter()
    .find(|path| std::path::Path::new(path).exists());
  let Some(probe) = probe else {
    // Nowhere to probe with: assume live, polling backstops us anyway.
    return true;
  };
  // The child must be spawned after our subscribe (its exec postdates
  // it) and reaped whatever happens next.
  let mut child = std::process::Command::new(probe).spawn().ok();
  let live = if child.is_some() {
    let mut buf = [0u8; 4096];
    let mut seen = false;
    // Up to ~1s total (socket timeout bounds each recv): first valid
    // event proves delivery — it need not be ours, any exec on a live
    // desktop arrives within milliseconds.
    for _ in 0..10 {
      let received = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
      if received <= 0 {
        // Timeout (EAGAIN/EWOULDBLOCK) or EINTR: keep waiting out the
        // second; anything else aborts the test.
        let err = std::io::Error::last_os_error();
        match err.kind() {
          std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => continue,
          _ => break,
        }
      }
      if parse_event(&buf[..received as usize]).is_some() {
        seen = true;
        break;
      }
    }
    seen
  } else {
    true
  };
  if let Some(mut child) = child.take() {
    let _ = child.wait();
  }
  live
}

/// Block on `cn_proc` broadcasts forever, forwarding lifecycle events.
/// Returns only on receive errors (the caller logs once and keeps
/// polling); the fd is closed on the way out.
pub(crate) fn watch(events: mpsc::Sender<ProcEvent>) -> Result<(), String> {
  let fd = subscribe()?;
  if !self_test(fd) {
    unsafe {
      libc::close(fd);
    }
    return Err("self-test got no exec event (kernel/LSM silently drops cn_proc?)".to_string());
  }
  log!("[Process Scanner] proc-events watcher live (netlink cn_proc)");
  // Back to blocking: the self-test's timeout was temporary.
  set_recv_timeout(fd, None);
  let mut buf = [0u8; 4096];
  loop {
    let received = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
    if received < 0 {
      let err = std::io::Error::last_os_error();
      if err.kind() == std::io::ErrorKind::Interrupted {
        continue;
      }
      unsafe {
        libc::close(fd);
      }
      return Err(format!("recv failed: {err}"));
    }
    if received == 0 {
      continue;
    }
    if let Some(event) = parse_event(&buf[..received as usize])
      && events.send(event).is_err()
    {
      // Receiver gone (daemon shutting down): quiet exit.
      unsafe {
        libc::close(fd);
      }
      return Ok(());
    }
  }
}
