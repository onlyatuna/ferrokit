// SPDX-License-Identifier: LGPL-3.0-only
//! A minimal D-Bus client: message framing, `AUTH EXTERNAL`, `Hello`, and a send queue that
//! honours a non-blocking socket.
//!
//! The protocol is implemented over [`DbusTransport`], so everything except opening the Unix
//! socket (Linux only) runs and is tested on every platform.
use ferrokit_core::event_loop::{EventDispatcher, SocketDescriptor, SocketEvent, SocketNotifier};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

pub const DBUS_MESSAGE_TYPE_METHOD_CALL: u8 = 1;
pub const DBUS_MESSAGE_TYPE_METHOD_RETURN: u8 = 2;
pub const DBUS_MESSAGE_TYPE_ERROR: u8 = 3;
pub const DBUS_MESSAGE_TYPE_SIGNAL: u8 = 4;

pub const DBUS_HEADER_FIELD_PATH: u8 = 1;
pub const DBUS_HEADER_FIELD_INTERFACE: u8 = 2;
pub const DBUS_HEADER_FIELD_MEMBER: u8 = 3;
pub const DBUS_HEADER_FIELD_ERROR_NAME: u8 = 4;
pub const DBUS_HEADER_FIELD_REPLY_SERIAL: u8 = 5;
pub const DBUS_HEADER_FIELD_DESTINATION: u8 = 6;
pub const DBUS_HEADER_FIELD_SENDER: u8 = 7;
pub const DBUS_HEADER_FIELD_SIGNATURE: u8 = 8;

/// Largest array on the wire (the header-field array is one), per the D-Bus specification.
const MAX_ARRAY_LEN: usize = 64 * 1024 * 1024;
/// Largest message, per the D-Bus specification.
const MAX_MESSAGE_LEN: usize = 128 * 1024 * 1024;
/// Bytes that may wait for a slow bus before `send_message` reports the connection as stuck.
const MAX_TX_BACKLOG: usize = 1024 * 1024;
/// Reads per `read_incoming` call, so a flooding peer cannot monopolise the event loop.
const MAX_READS_PER_POLL: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbusMessage {
    pub msg_type: u8,
    pub flags: u8,
    pub serial: u32,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    pub reply_serial: Option<u32>,
    pub signature: String,
    /// Body bytes exactly as on the wire (aligned relative to the body start).
    pub body: Vec<u8>,
    /// The message was received in big-endian byte order, so `body` is too. Messages built by
    /// this crate are always little-endian.
    pub big_endian: bool,
}

/// Bounds-checked reader over a D-Bus byte stream; alignment is relative to `data[0]`.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    big_endian: bool,
}

impl<'a> Reader<'a> {
    fn align(&mut self, to: usize) -> Result<(), &'static str> {
        let aligned = (self.pos + to - 1) & !(to - 1);
        if aligned > self.data.len() {
            return Err("D-Bus data ends inside alignment padding");
        }
        self.pos = aligned;
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.data.len())
            .ok_or("D-Bus data is truncated")?;
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        self.align(4)?;
        let bytes: [u8; 4] = self.take(4)?.try_into().unwrap();
        Ok(if self.big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    }

    /// A `string` or `object path`: aligned u32 length, UTF-8 bytes, NUL.
    fn string(&mut self) -> Result<String, &'static str> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        if self.u8()? != 0 {
            return Err("D-Bus string is not NUL-terminated");
        }
        String::from_utf8(bytes.to_vec()).map_err(|_| "D-Bus string is not valid UTF-8")
    }

    /// A `signature`: u8 length, bytes, NUL.
    fn signature(&mut self) -> Result<String, &'static str> {
        let len = self.u8()? as usize;
        let bytes = self.take(len)?;
        if self.u8()? != 0 {
            return Err("D-Bus signature is not NUL-terminated");
        }
        String::from_utf8(bytes.to_vec()).map_err(|_| "D-Bus signature is not valid UTF-8")
    }
}

fn align_up(n: usize, to: usize) -> usize {
    (n + to - 1) & !(to - 1)
}

impl DbusMessage {
    pub fn method_call(
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        serial: u32,
    ) -> Self {
        Self {
            msg_type: DBUS_MESSAGE_TYPE_METHOD_CALL,
            flags: 0,
            serial,
            path: Some(path.to_string()),
            interface: Some(interface.to_string()),
            member: Some(member.to_string()),
            destination: Some(destination.to_string()),
            sender: None,
            reply_serial: None,
            signature: String::new(),
            body: Vec::new(),
            big_endian: false,
        }
    }

    pub fn signal(path: &str, interface: &str, member: &str, serial: u32) -> Self {
        Self {
            msg_type: DBUS_MESSAGE_TYPE_SIGNAL,
            flags: 0,
            serial,
            path: Some(path.to_string()),
            interface: Some(interface.to_string()),
            member: Some(member.to_string()),
            destination: None,
            sender: None,
            reply_serial: None,
            signature: String::new(),
            body: Vec::new(),
            big_endian: false,
        }
    }

    pub fn method_return(reply_serial: u32, destination: &str, serial: u32) -> Self {
        Self {
            msg_type: DBUS_MESSAGE_TYPE_METHOD_RETURN,
            flags: 0,
            serial,
            path: None,
            interface: None,
            member: None,
            destination: Some(destination.to_string()),
            sender: None,
            reply_serial: Some(reply_serial),
            signature: String::new(),
            body: Vec::new(),
            big_endian: false,
        }
    }

    fn pad_body(&mut self, to: usize) {
        while !self.body.len().is_multiple_of(to) {
            self.body.push(0);
        }
    }

    pub fn append_string(&mut self, s: &str) {
        self.signature.push('s');
        self.pad_body(4);
        let bytes = s.as_bytes();
        self.body
            .extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        self.body.extend_from_slice(bytes);
        self.body.push(0);
    }

    pub fn append_u32(&mut self, val: u32) {
        self.signature.push('u');
        self.pad_body(4);
        self.body.extend_from_slice(&val.to_le_bytes());
    }

    pub fn append_i32(&mut self, val: i32) {
        self.signature.push('i');
        self.pad_body(4);
        self.body.extend_from_slice(&val.to_le_bytes());
    }

    /// An empty `as` (array of strings).
    pub fn append_empty_string_array(&mut self) {
        self.signature.push_str("as");
        self.pad_body(4);
        self.body.extend_from_slice(&0u32.to_le_bytes());
    }

    /// An empty `a{sv}` (string-to-variant dictionary). The element padding is written even
    /// though there are no elements: array data starts at the element alignment.
    pub fn append_empty_dict_sv(&mut self) {
        self.signature.push_str("a{sv}");
        self.pad_body(4);
        self.body.extend_from_slice(&0u32.to_le_bytes());
        self.pad_body(8);
    }

    /// The first body argument when it is a string or object path.
    pub fn body_string(&self) -> Option<String> {
        if !(self.signature.starts_with('s') || self.signature.starts_with('o')) {
            return None;
        }
        Reader {
            data: &self.body,
            pos: 0,
            big_endian: self.big_endian,
        }
        .string()
        .ok()
    }

    /// Rejects a message that cannot be encoded.
    fn validate(&self) -> Result<(), &'static str> {
        if self.signature.len() > 255 {
            return Err("D-Bus signature exceeds 255 bytes");
        }
        if self.body.len() > MAX_MESSAGE_LEN {
            return Err("D-Bus message body exceeds the protocol limit");
        }
        Ok(())
    }

    /// Encodes the message; a signature longer than 255 bytes is rejected by `send_message`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut fields_buf = Vec::new();

        // Helper: append string field as variant (yv)
        let append_field_str = |buf: &mut Vec<u8>, code: u8, sig: &str, val: &str| {
            // Align to 8 bytes
            while !buf.len().is_multiple_of(8) {
                buf.push(0);
            }
            buf.push(code); // Field code
            buf.push(sig.len() as u8); // Signature length
            buf.extend_from_slice(sig.as_bytes());
            buf.push(0);
            // Align string content to 4 bytes
            while !buf.len().is_multiple_of(4) {
                buf.push(0);
            }
            let str_bytes = val.as_bytes();
            buf.extend_from_slice(&(str_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(str_bytes);
            buf.push(0);
        };

        if let Some(path) = &self.path {
            append_field_str(&mut fields_buf, DBUS_HEADER_FIELD_PATH, "o", path);
        }
        if let Some(iface) = &self.interface {
            append_field_str(&mut fields_buf, DBUS_HEADER_FIELD_INTERFACE, "s", iface);
        }
        if let Some(member) = &self.member {
            append_field_str(&mut fields_buf, DBUS_HEADER_FIELD_MEMBER, "s", member);
        }
        if let Some(dest) = &self.destination {
            append_field_str(&mut fields_buf, DBUS_HEADER_FIELD_DESTINATION, "s", dest);
        }
        if let Some(sender) = &self.sender {
            append_field_str(&mut fields_buf, DBUS_HEADER_FIELD_SENDER, "s", sender);
        }
        if let Some(reply_serial) = self.reply_serial {
            while !fields_buf.len().is_multiple_of(8) {
                fields_buf.push(0);
            }
            fields_buf.push(DBUS_HEADER_FIELD_REPLY_SERIAL);
            fields_buf.push(1);
            fields_buf.push(b'u');
            fields_buf.push(0);
            while !fields_buf.len().is_multiple_of(4) {
                fields_buf.push(0);
            }
            fields_buf.extend_from_slice(&reply_serial.to_le_bytes());
        }
        if !self.signature.is_empty() {
            while !fields_buf.len().is_multiple_of(8) {
                fields_buf.push(0);
            }
            fields_buf.push(DBUS_HEADER_FIELD_SIGNATURE);
            fields_buf.push(1);
            fields_buf.push(b'g');
            fields_buf.push(0);
            let sig_bytes = self.signature.as_bytes();
            fields_buf
                .push(u8::try_from(sig_bytes.len()).expect("D-Bus signature exceeds 255 bytes"));
            fields_buf.extend_from_slice(sig_bytes);
            fields_buf.push(0);
        }

        let mut packet = vec![
            b'l', // Little endian
            self.msg_type,
            self.flags,
            1, // Major Version = 1
        ];
        packet.extend_from_slice(&(self.body.len() as u32).to_le_bytes());
        packet.extend_from_slice(&self.serial.to_le_bytes());
        packet.extend_from_slice(&(fields_buf.len() as u32).to_le_bytes());

        // Header fields array
        packet.extend_from_slice(&fields_buf);
        // Align to 8 bytes after header
        while !packet.len().is_multiple_of(8) {
            packet.push(0);
        }

        // Body content
        packet.extend_from_slice(&self.body);
        packet
    }

    /// Length of the complete message at the start of `data`: `Ok(None)` while more bytes are
    /// needed, `Err` when the stream cannot be a D-Bus message (bad endianness marker or version,
    /// or lengths over the protocol limits).
    pub fn frame_len(data: &[u8]) -> Result<Option<usize>, &'static str> {
        if let Some(&marker) = data.first() {
            if marker != b'l' && marker != b'B' {
                return Err("Invalid D-Bus endianness marker");
            }
        }
        if data.len() < 16 {
            return Ok(None);
        }
        if data[3] != 1 {
            return Err("Unsupported D-Bus protocol version");
        }
        let read_u32 = |at: usize| {
            let bytes: [u8; 4] = data[at..at + 4].try_into().unwrap();
            if data[0] == b'B' {
                u32::from_be_bytes(bytes)
            } else {
                u32::from_le_bytes(bytes)
            }
        };
        let body_len = read_u32(4) as usize;
        let fields_len = read_u32(12) as usize;
        if fields_len > MAX_ARRAY_LEN {
            return Err("D-Bus header-field array exceeds the protocol limit");
        }
        let total = align_up(16 + fields_len, 8) + body_len;
        if total > MAX_MESSAGE_LEN {
            return Err("D-Bus message exceeds the protocol limit");
        }
        Ok((data.len() >= total).then_some(total))
    }

    /// Decodes one complete message (`data` may be longer; the rest is ignored).
    pub fn from_bytes(data: &[u8]) -> Result<Self, &'static str> {
        let total = Self::frame_len(data)?.ok_or("Incomplete D-Bus message")?;
        let data = &data[..total];
        let big_endian = data[0] == b'B';
        let u32_at = |at: usize| {
            let bytes: [u8; 4] = data[at..at + 4].try_into().unwrap();
            if big_endian {
                u32::from_be_bytes(bytes)
            } else {
                u32::from_le_bytes(bytes)
            }
        };
        let fields_len = u32_at(12) as usize;
        let header_end = 16 + fields_len;
        let body_start = align_up(header_end, 8);

        let mut msg = Self {
            msg_type: data[1],
            flags: data[2],
            serial: u32_at(8),
            path: None,
            interface: None,
            member: None,
            destination: None,
            sender: None,
            reply_serial: None,
            signature: String::new(),
            body: data[body_start..].to_vec(),
            big_endian,
        };

        let mut reader = Reader {
            data: &data[..header_end],
            pos: 16,
            big_endian,
        };
        while reader.pos < header_end {
            reader.align(8)?;
            let code = reader.u8()?;
            let variant_signature = reader.signature()?;
            match variant_signature.as_str() {
                "o" | "s" => {
                    let value = reader.string()?;
                    match code {
                        DBUS_HEADER_FIELD_PATH => msg.path = Some(value),
                        DBUS_HEADER_FIELD_INTERFACE => msg.interface = Some(value),
                        DBUS_HEADER_FIELD_MEMBER => msg.member = Some(value),
                        DBUS_HEADER_FIELD_DESTINATION => msg.destination = Some(value),
                        DBUS_HEADER_FIELD_SENDER => msg.sender = Some(value),
                        // ERROR_NAME has no field on the message; unknown codes are skipped.
                        _ => {}
                    }
                }
                "g" => {
                    let value = reader.signature()?;
                    if code == DBUS_HEADER_FIELD_SIGNATURE {
                        msg.signature = value;
                    }
                }
                "u" => {
                    let value = reader.u32()?;
                    if code == DBUS_HEADER_FIELD_REPLY_SERIAL {
                        msg.reply_serial = Some(value);
                    }
                }
                _ => return Err("Unsupported D-Bus header field type"),
            }
        }
        Ok(msg)
    }
}

/// One entry of a D-Bus server address list (`DBUS_SESSION_BUS_ADDRESS`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbusAddress {
    /// `unix:path=...`, or `unix:runtime=yes` resolved to `$XDG_RUNTIME_DIR/bus`.
    Path(String),
    /// `unix:abstract=...` (Linux abstract socket namespace).
    Abstract(String),
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Parses a `;`-separated D-Bus address list, keeping the entries this client can connect to
/// (the `unix` transport with `path`, `abstract` or `runtime=yes`), in order.
pub fn parse_bus_address(address: &str, runtime_dir: Option<&str>) -> Vec<DbusAddress> {
    let mut out = Vec::new();
    for entry in address.split(';') {
        let Some(params) = entry.strip_prefix("unix:") else {
            continue;
        };
        let mut found = None;
        for pair in params.split(',') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let parsed = match key {
                "path" => percent_decode(value).map(DbusAddress::Path),
                "abstract" => percent_decode(value).map(DbusAddress::Abstract),
                "runtime" if value == "yes" => {
                    runtime_dir.map(|dir| DbusAddress::Path(format!("{}/bus", dir)))
                }
                _ => None,
            };
            if parsed.is_some() {
                found = parsed;
                break;
            }
        }
        out.extend(found);
    }
    out
}

/// A byte stream to a bus daemon. `Read`/`Write` must report `WouldBlock` (or `TimedOut`) rather
/// than block once [`set_nonblocking(true)`](Self::set_nonblocking) was called.
pub trait DbusTransport: Read + Write + Send + Sync {
    /// The OS descriptor to watch for readability, or `-1` when the transport has none.
    fn socket_fd(&self) -> SocketDescriptor;
    fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()>;
}

#[cfg(target_os = "linux")]
impl DbusTransport for std::os::unix::net::UnixStream {
    fn socket_fd(&self) -> SocketDescriptor {
        std::os::unix::io::AsRawFd::as_raw_fd(self) as SocketDescriptor
    }

    fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()> {
        std::os::unix::net::UnixStream::set_nonblocking(self, nonblocking)
    }
}

pub struct DbusConnection {
    address: String,
    unique_name: String,
    serial_counter: AtomicU32,
    transport: Box<dyn DbusTransport>,
    registered_names: Vec<String>,
    /// Encoded messages not yet accepted by the transport; `tx_pos` bytes were already written.
    tx_buf: Vec<u8>,
    tx_pos: usize,
    /// Bytes received that do not yet form a whole message.
    rx_buf: Vec<u8>,
    /// Messages decoded while waiting for a reply, handed out by `read_incoming`.
    inbox: VecDeque<DbusMessage>,
    closed: bool,
    #[cfg(any(test, feature = "test-hooks"))]
    outbox: Vec<DbusMessage>,
}

impl DbusConnection {
    /// Connects to the session bus named by `DBUS_SESSION_BUS_ADDRESS`, or to the systemd default
    /// `/run/user/<uid>/bus`. Errors when no bus can be reached and on platforms without D-Bus.
    pub fn connect_session_bus() -> Result<Self, &'static str> {
        #[cfg(target_os = "linux")]
        {
            let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
                .unwrap_or_else(|_| format!("unix:path=/run/user/{}/bus", crate::current_uid()));
            Self::connect_to_address(&address)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err("D-Bus session bus is unavailable on this platform")
        }
    }

    /// Connects to the first reachable entry of a D-Bus address list, authenticates and says
    /// `Hello`.
    pub fn connect_to_address(address: &str) -> Result<Self, &'static str> {
        #[cfg(target_os = "linux")]
        {
            use std::os::linux::net::SocketAddrExt;
            use std::os::unix::net::{SocketAddr, UnixStream};

            let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
            let candidates = parse_bus_address(address, runtime_dir.as_deref());
            if candidates.is_empty() {
                return Err("No supported D-Bus transport in the bus address");
            }
            let timeout = Some(std::time::Duration::from_secs(5));
            for candidate in candidates {
                let stream = match &candidate {
                    DbusAddress::Path(path) => UnixStream::connect(path),
                    DbusAddress::Abstract(name) => SocketAddr::from_abstract_name(name.as_bytes())
                        .and_then(|addr| UnixStream::connect_addr(&addr)),
                };
                let Ok(stream) = stream else {
                    continue;
                };
                stream
                    .set_read_timeout(timeout)
                    .and_then(|_| stream.set_write_timeout(timeout))
                    .map_err(|_| "Failed to configure the D-Bus socket")?;
                return Self::from_transport(Box::new(stream), address, crate::current_uid());
            }
            Err("Failed to connect to the D-Bus bus")
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = address;
            Err("D-Bus session bus is unavailable on this platform")
        }
    }

    /// Authenticates over `transport` as `uid` (`AUTH EXTERNAL`) and says `Hello`. The transport
    /// must be in blocking mode with a read timeout; it is switched to non-blocking afterwards.
    pub fn from_transport(
        transport: Box<dyn DbusTransport>,
        address: &str,
        uid: u32,
    ) -> Result<Self, &'static str> {
        let mut conn = Self {
            address: address.to_string(),
            unique_name: String::new(),
            serial_counter: AtomicU32::new(1),
            transport,
            registered_names: Vec::new(),
            tx_buf: Vec::new(),
            tx_pos: 0,
            rx_buf: Vec::new(),
            inbox: VecDeque::new(),
            closed: false,
            #[cfg(any(test, feature = "test-hooks"))]
            outbox: Vec::new(),
        };
        conn.authenticate(uid)?;
        conn.hello()?;
        conn.transport
            .set_nonblocking(true)
            .map_err(|_| "Failed to make the D-Bus socket non-blocking")?;
        Ok(conn)
    }

    fn authenticate(&mut self, uid: u32) -> Result<(), &'static str> {
        let hex_uid: String = uid
            .to_string()
            .bytes()
            .map(|b| format!("{:02x}", b))
            .collect();
        let mut auth = vec![0u8];
        auth.extend_from_slice(format!("AUTH EXTERNAL {}\r\n", hex_uid).as_bytes());
        self.transport
            .write_all(&auth)
            .and_then(|_| self.transport.flush())
            .map_err(|_| "Failed to send D-Bus authentication")?;

        // One byte at a time: nothing after the reply line may be consumed here.
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while !line.ends_with(b"\r\n") {
            if line.len() > 1024 {
                return Err("D-Bus authentication reply is too long");
            }
            match self.transport.read(&mut byte) {
                Ok(0) => return Err("D-Bus connection closed during authentication"),
                Ok(_) => line.push(byte[0]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err("Failed to read the D-Bus authentication reply"),
            }
        }
        if !(line.starts_with(b"OK ") || line.starts_with(b"OK\r")) {
            return Err("D-Bus authentication was rejected");
        }
        self.transport
            .write_all(b"BEGIN\r\n")
            .and_then(|_| self.transport.flush())
            .map_err(|_| "Failed to start the D-Bus message stream")
    }

    pub fn next_serial(&self) -> u32 {
        self.serial_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// The descriptor to watch for readability, or `-1` when the transport has none.
    #[inline]
    pub fn socket_fd(&self) -> SocketDescriptor {
        self.transport.socket_fd()
    }

    #[inline]
    pub fn unique_name(&self) -> &str {
        &self.unique_name
    }

    #[inline]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Sends `org.freedesktop.DBus.Hello` and waits for the unique name the bus assigns.
    pub fn hello(&mut self) -> Result<String, &'static str> {
        let serial = self.next_serial();
        let msg = DbusMessage::method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            serial,
        );
        self.send_message(msg)?;

        let reply = self.wait_for_reply(serial)?;
        if reply.msg_type == DBUS_MESSAGE_TYPE_ERROR {
            return Err("The D-Bus Hello call was rejected");
        }
        let name = reply
            .body_string()
            .ok_or("The D-Bus Hello reply carries no unique name")?;
        self.unique_name = name.clone();
        Ok(name)
    }

    pub fn request_name(&mut self, service_name: &str) -> Result<bool, &'static str> {
        let serial = self.next_serial();
        let mut msg = DbusMessage::method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            serial,
        );
        msg.append_string(service_name);
        self.send_message(msg)?;
        self.registered_names.push(service_name.to_string());
        Ok(true)
    }
    pub fn add_match(&mut self, match_rule: &str) -> Result<bool, &'static str> {
        let serial = self.next_serial();
        let mut msg = DbusMessage::method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "AddMatch",
            serial,
        );
        msg.append_string(match_rule);
        self.send_message(msg)?;
        Ok(true)
    }

    pub fn register_status_notifier_item(
        &mut self,
        service_or_path: &str,
    ) -> Result<bool, &'static str> {
        let serial = self.next_serial();
        let mut msg = DbusMessage::method_call(
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
            "RegisterStatusNotifierItem",
            serial,
        );
        msg.append_string(service_or_path);
        self.send_message(msg)?;
        Ok(true)
    }

    /// Queues the message and writes as much as the socket accepts now. A message the socket only
    /// partly took stays queued and goes out whole on a later `send_message` or `read_incoming`,
    /// so the bus never sees a torn message. Errors when the connection is closed or when the
    /// backlog (a stuck bus) exceeds 1 MiB.
    pub fn send_message(&mut self, msg: DbusMessage) -> Result<(), &'static str> {
        if self.closed {
            return Err("The D-Bus connection is closed");
        }
        msg.validate()?;
        let packet = msg.to_bytes();
        if self.tx_buf.len() - self.tx_pos + packet.len() > MAX_TX_BACKLOG {
            return Err("The D-Bus send buffer is full");
        }
        self.tx_buf.extend_from_slice(&packet);
        #[cfg(any(test, feature = "test-hooks"))]
        self.outbox.push(msg);
        self.flush_tx()
    }

    /// Writes queued bytes until the transport would block.
    fn flush_tx(&mut self) -> Result<(), &'static str> {
        while self.tx_pos < self.tx_buf.len() {
            match self.transport.write(&self.tx_buf[self.tx_pos..]) {
                Ok(0) => return Err(self.close("The D-Bus connection is closed")),
                Ok(n) => self.tx_pos += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    if self.tx_pos > 0 {
                        self.tx_buf.drain(..self.tx_pos);
                        self.tx_pos = 0;
                    }
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(self.close("Failed to write to the D-Bus connection")),
            }
        }
        self.tx_buf.clear();
        self.tx_pos = 0;
        let _ = self.transport.flush();
        Ok(())
    }

    fn close(&mut self, reason: &'static str) -> &'static str {
        self.closed = true;
        self.tx_buf.clear();
        self.tx_pos = 0;
        self.rx_buf.clear();
        reason
    }

    /// Decodes every complete message in `rx_buf` into `inbox`.
    fn decode_frames(&mut self) -> Result<(), &'static str> {
        let mut offset = 0;
        let result = loop {
            match DbusMessage::frame_len(&self.rx_buf[offset..]) {
                Ok(None) => break Ok(()),
                Ok(Some(len)) => {
                    match DbusMessage::from_bytes(&self.rx_buf[offset..offset + len]) {
                        Ok(msg) => {
                            self.inbox.push_back(msg);
                            offset += len;
                        }
                        Err(e) => break Err(e),
                    }
                }
                Err(e) => break Err(e),
            }
        };
        match result {
            Ok(()) => {
                self.rx_buf.drain(..offset);
                Ok(())
            }
            // The stream can no longer be framed.
            Err(e) => Err(self.close(e)),
        }
    }

    /// Reads once from the transport into `rx_buf`; `Ok(0)` means nothing is available. An `Err`
    /// leaves the connection open so the caller can still decode what was buffered first.
    fn read_chunk(&mut self) -> Result<usize, &'static str> {
        let mut chunk = [0u8; 4096];
        loop {
            match self.transport.read(&mut chunk) {
                Ok(0) => return Err("The D-Bus connection was closed by the bus"),
                Ok(n) => {
                    self.rx_buf.extend_from_slice(&chunk[..n]);
                    return Ok(n);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(0);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err("Failed to read from the D-Bus connection"),
            }
        }
    }

    /// Blocks (up to the transport's read timeout) for the reply to `serial`; other messages
    /// received meanwhile stay queued for `read_incoming`.
    fn wait_for_reply(&mut self, serial: u32) -> Result<DbusMessage, &'static str> {
        loop {
            self.decode_frames()?;
            let position = self.inbox.iter().position(|m| {
                m.reply_serial == Some(serial)
                    && (m.msg_type == DBUS_MESSAGE_TYPE_METHOD_RETURN
                        || m.msg_type == DBUS_MESSAGE_TYPE_ERROR)
            });
            if let Some(position) = position {
                return Ok(self.inbox.remove(position).unwrap());
            }
            match self.read_chunk() {
                Ok(0) => return Err("Timed out waiting for a D-Bus reply"),
                Ok(_) => {}
                Err(e) => return Err(self.close(e)),
            }
        }
    }

    #[inline]
    pub fn is_live_socket(&self) -> bool {
        !self.closed && self.transport.socket_fd() >= 0
    }

    /// Everything the bus has sent that forms a whole message; never blocks. A message split over
    /// several reads is returned once its last byte arrives. Also retries queued output.
    pub fn read_incoming(&mut self) -> Result<Vec<DbusMessage>, &'static str> {
        if self.closed {
            // Messages queued before the close (e.g. while waiting for a reply) still go out.
            if self.inbox.is_empty() {
                return Err("The D-Bus connection is closed");
            }
            return Ok(self.inbox.drain(..).collect());
        }
        self.flush_tx()?;
        let mut read_error = None;
        for _ in 0..MAX_READS_PER_POLL {
            match self.read_chunk() {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    read_error = Some(e);
                    break;
                }
            }
        }
        // Decode before reporting a read failure: messages that arrived whole are delivered, and
        // the error surfaces on the next call (the connection is closed).
        let mut error = self.decode_frames().err();
        if error.is_none() {
            if let Some(e) = read_error {
                error = Some(self.close(e));
            }
        }
        match error {
            Some(e) if self.inbox.is_empty() => Err(e),
            _ => Ok(self.inbox.drain(..).collect()),
        }
    }

    /// Bytes queued for the bus that the socket has not accepted yet.
    pub fn pending_output(&self) -> usize {
        self.tx_buf.len() - self.tx_pos
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn outbox_count(&self) -> usize {
        self.outbox.len()
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn has_sent_member(&self, member: &str) -> bool {
        self.outbox
            .iter()
            .any(|m| m.member.as_deref() == Some(member))
    }

    /// Watches the bus socket for readability. A transport without a descriptor (`-1`) has
    /// nothing to watch, so nothing is registered and `None` is returned.
    pub fn bind_event_dispatcher(
        &self,
        dispatcher: &mut dyn EventDispatcher,
    ) -> Option<Arc<SocketNotifier>> {
        let fd = self.transport.socket_fd();
        if self.closed || fd < 0 {
            return None;
        }
        let notifier = Arc::new(SocketNotifier::new(fd, SocketEvent::Read));
        dispatcher.register_socket_notifier(&notifier);
        Some(notifier)
    }
}

/// An in-memory bus daemon for tests: speaks `AUTH EXTERNAL`/`BEGIN`, answers `Hello` (and
/// `RequestName`), and records the messages it receives. Reads report `WouldBlock` when it has
/// nothing to say, like a non-blocking socket.
#[cfg(any(test, feature = "test-hooks"))]
pub mod scripted {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct State {
        from_client: Vec<u8>,
        to_client: VecDeque<u8>,
        began: bool,
        auth_lines: Vec<String>,
        received: Vec<DbusMessage>,
        reject_auth: bool,
        fail_hello: bool,
        eof: bool,
        max_read: Option<usize>,
        write_budget: Option<usize>,
        unique_name: String,
        fd: SocketDescriptor,
    }

    /// The transport half, handed to [`DbusConnection::from_transport`].
    pub struct ScriptedDbusTransport {
        state: Arc<Mutex<State>>,
    }

    /// The observing half: injects bytes, inspects what the client sent, tunes behaviour.
    #[derive(Clone)]
    pub struct ScriptedDbusBus {
        state: Arc<Mutex<State>>,
    }

    impl ScriptedDbusTransport {
        /// A transport with no OS descriptor (`socket_fd() == -1`).
        pub fn new(unique_name: &str) -> (Self, ScriptedDbusBus) {
            let state = Arc::new(Mutex::new(State {
                unique_name: unique_name.to_string(),
                fd: -1,
                ..State::default()
            }));
            (
                Self {
                    state: Arc::clone(&state),
                },
                ScriptedDbusBus { state },
            )
        }
    }

    impl ScriptedDbusBus {
        pub fn reject_auth(&self) {
            self.state.lock().unwrap().reject_auth = true;
        }

        /// Answers `Hello` with an error message instead of a unique name.
        pub fn fail_hello(&self) {
            self.state.lock().unwrap().fail_hello = true;
        }

        /// Once the queued bytes are read, `read` reports end of stream.
        pub fn close_after_queued(&self) {
            self.state.lock().unwrap().eof = true;
        }

        /// Hands the client at most `n` bytes per `read`.
        pub fn limit_read_size(&self, n: usize) {
            self.state.lock().unwrap().max_read = Some(n);
        }

        /// Accepts only `n` more bytes from the client, then reports `WouldBlock` on `write`.
        /// `None` lifts the limit.
        pub fn set_write_budget(&self, n: Option<usize>) {
            self.state.lock().unwrap().write_budget = n;
        }

        /// Pretends the transport owns this OS descriptor.
        pub fn set_socket_fd(&self, fd: SocketDescriptor) {
            self.state.lock().unwrap().fd = fd;
        }

        /// Queues bytes for the client to read, as the bus would send them.
        pub fn push_to_client(&self, bytes: &[u8]) {
            self.state.lock().unwrap().to_client.extend(bytes.iter());
        }

        /// Messages the client sent after `BEGIN`.
        pub fn received(&self) -> Vec<DbusMessage> {
            self.state.lock().unwrap().received.clone()
        }

        /// Lines the client sent before `BEGIN` (without the CR LF).
        pub fn auth_lines(&self) -> Vec<String> {
            self.state.lock().unwrap().auth_lines.clone()
        }
    }

    impl State {
        fn process(&mut self) {
            while !self.began {
                // The very first byte of a connection is a NUL credential byte.
                if self.from_client.first() == Some(&0) {
                    self.from_client.remove(0);
                }
                let Some(end) = self.from_client.windows(2).position(|w| w == b"\r\n") else {
                    return;
                };
                let line = String::from_utf8_lossy(&self.from_client[..end]).into_owned();
                self.from_client.drain(..end + 2);
                if line.starts_with("AUTH") {
                    let reply: &[u8] = if self.reject_auth {
                        b"REJECTED EXTERNAL\r\n"
                    } else {
                        b"OK 0123456789abcdef0123456789abcdef\r\n"
                    };
                    self.to_client.extend(reply.iter());
                } else if line == "BEGIN" {
                    self.began = true;
                }
                self.auth_lines.push(line);
            }
            while let Ok(Some(len)) = DbusMessage::frame_len(&self.from_client) {
                let msg = DbusMessage::from_bytes(&self.from_client[..len]).unwrap();
                self.from_client.drain(..len);
                self.answer(&msg);
                self.received.push(msg);
            }
        }

        fn answer(&mut self, call: &DbusMessage) {
            if call.msg_type != DBUS_MESSAGE_TYPE_METHOD_CALL {
                return;
            }
            let reply = match call.member.as_deref() {
                Some("Hello") if self.fail_hello => {
                    let mut error = DbusMessage::method_return(call.serial, "", 1);
                    error.msg_type = DBUS_MESSAGE_TYPE_ERROR;
                    error
                }
                Some("Hello") => {
                    let mut reply = DbusMessage::method_return(call.serial, &self.unique_name, 1);
                    reply.sender = Some("org.freedesktop.DBus".to_string());
                    reply.append_string(&self.unique_name);
                    reply
                }
                Some("RequestName") => {
                    let mut reply = DbusMessage::method_return(call.serial, &self.unique_name, 2);
                    reply.append_u32(1);
                    reply
                }
                _ => return,
            };
            self.to_client.extend(reply.to_bytes().iter());
        }
    }

    impl Read for ScriptedDbusTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut state = self.state.lock().unwrap();
            if state.to_client.is_empty() {
                if state.eof {
                    return Ok(0);
                }
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = buf
                .len()
                .min(state.to_client.len())
                .min(state.max_read.unwrap_or(usize::MAX));
            for slot in buf.iter_mut().take(n) {
                *slot = state.to_client.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    impl Write for ScriptedDbusTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut state = self.state.lock().unwrap();
            let n = match state.write_budget {
                Some(0) => return Err(io::ErrorKind::WouldBlock.into()),
                Some(budget) => buf.len().min(budget),
                None => buf.len(),
            };
            if let Some(budget) = state.write_budget.as_mut() {
                *budget -= n;
            }
            state.from_client.extend_from_slice(&buf[..n]);
            state.process();
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl DbusTransport for ScriptedDbusTransport {
        fn socket_fd(&self) -> SocketDescriptor {
            self.state.lock().unwrap().fd
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::scripted::ScriptedDbusTransport;
    use super::*;

    fn connect(unique_name: &str) -> (DbusConnection, scripted::ScriptedDbusBus) {
        let (transport, bus) = ScriptedDbusTransport::new(unique_name);
        let conn = DbusConnection::from_transport(Box::new(transport), "scripted:", 1000)
            .expect("handshake against the scripted bus");
        (conn, bus)
    }

    fn call(member: &str, serial: u32) -> DbusMessage {
        DbusMessage::method_call("org.test", "/org/test", "org.test.Iface", member, serial)
    }

    #[test]
    fn header_fields_round_trip() {
        let mut msg = call("Frobnicate", 7);
        msg.sender = Some(":1.5".to_string());
        msg.reply_serial = Some(3);
        msg.append_string("hello");
        msg.append_u32(42);
        let parsed = DbusMessage::from_bytes(&msg.to_bytes()).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn body_arguments_are_aligned() {
        let mut msg = call("Notify", 1);
        msg.append_string("ferrokit.app"); // length 13 with the NUL: next u32 needs padding
        msg.append_u32(0);
        msg.append_string("");
        msg.append_string("title");
        msg.append_string("body");
        msg.append_empty_string_array();
        msg.append_empty_dict_sv();
        msg.append_i32(-1);
        assert_eq!(msg.signature, "susssasa{sv}i");

        // "ferrokit.app" = 4 + 12 + 1 = 17 bytes, padded to 20 before the u32.
        assert_eq!(&msg.body[17..20], &[0, 0, 0]);
        assert_eq!(&msg.body[20..24], &0u32.to_le_bytes());
        assert!(msg.body.len().is_multiple_of(4));
        // The a{sv} array data starts on an 8-byte boundary even when empty.
        let tail = &msg.body[msg.body.len() - 4..];
        assert_eq!(tail, &(-1i32).to_le_bytes());
        assert!((msg.body.len() - 4).is_multiple_of(8));
    }

    #[test]
    fn frame_len_waits_for_the_whole_message() {
        let mut msg = call("Frobnicate", 1);
        msg.append_string("payload");
        let bytes = msg.to_bytes();
        for cut in 0..bytes.len() {
            assert_eq!(
                DbusMessage::frame_len(&bytes[..cut]).unwrap(),
                None,
                "{cut}"
            );
        }
        assert_eq!(DbusMessage::frame_len(&bytes).unwrap(), Some(bytes.len()));
        let mut two = bytes.clone();
        two.extend_from_slice(&bytes);
        assert_eq!(DbusMessage::frame_len(&two).unwrap(), Some(bytes.len()));
    }

    #[test]
    fn frame_len_rejects_garbage_and_oversize() {
        assert!(DbusMessage::frame_len(b"x").is_err());
        let mut bytes = call("Frobnicate", 1).to_bytes();
        bytes[3] = 2; // protocol version
        assert!(DbusMessage::frame_len(&bytes).is_err());
        let mut huge = call("Frobnicate", 1).to_bytes();
        huge[12..16].copy_from_slice(&(MAX_ARRAY_LEN as u32 + 1).to_le_bytes());
        assert!(DbusMessage::frame_len(&huge).is_err());
        let mut huge = call("Frobnicate", 1).to_bytes();
        huge[4..8].copy_from_slice(&(MAX_MESSAGE_LEN as u32).to_le_bytes());
        assert!(DbusMessage::frame_len(&huge).is_err());
    }

    #[test]
    fn big_endian_message_parses() {
        // METHOD_CALL, serial 9, one field: MEMBER = "Ping"; body is a big-endian string "hi".
        let mut bytes = vec![b'B', 1, 0, 1];
        bytes.extend_from_slice(&7u32.to_be_bytes()); // body length: 4 + 2 + 1
        bytes.extend_from_slice(&9u32.to_be_bytes());
        let mut fields = vec![DBUS_HEADER_FIELD_MEMBER, 1, b's', 0];
        fields.extend_from_slice(&4u32.to_be_bytes());
        fields.extend_from_slice(b"Ping\0");
        let mut sig = vec![0u8; 3];
        sig.extend_from_slice(&[DBUS_HEADER_FIELD_SIGNATURE, 1, b'g', 0, 1, b's', 0]);
        fields.extend_from_slice(&sig);
        bytes.extend_from_slice(&(fields.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&fields);
        while !bytes.len().is_multiple_of(8) {
            bytes.push(0);
        }
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(b"hi\0");

        let msg = DbusMessage::from_bytes(&bytes).unwrap();
        assert!(msg.big_endian);
        assert_eq!(msg.serial, 9);
        assert_eq!(msg.member.as_deref(), Some("Ping"));
        assert_eq!(msg.signature, "s");
        assert_eq!(msg.body_string().as_deref(), Some("hi"));
    }

    #[test]
    fn malformed_header_fields_are_errors_not_panics() {
        let mut bytes = call("Frobnicate", 1).to_bytes();
        // Claim a string length far larger than the header holds.
        let first_len = 16 + 4; // after code, signature (`o`) and NUL: the u32 string length
        bytes[first_len..first_len + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(DbusMessage::from_bytes(&bytes).is_err());
        // Header field array ending mid-field.
        let mut truncated = call("Frobnicate", 1).to_bytes();
        truncated[12..16].copy_from_slice(&5u32.to_le_bytes());
        assert!(DbusMessage::from_bytes(&truncated).is_err());
    }

    #[test]
    fn address_lists_are_parsed() {
        assert_eq!(
            parse_bus_address("unix:path=/run/user/1000/bus", None),
            vec![DbusAddress::Path("/run/user/1000/bus".into())]
        );
        assert_eq!(
            parse_bus_address(
                "unix:abstract=/tmp/dbus-ab%3dc,guid=x;unix:runtime=yes;tcp:host=h,port=1",
                Some("/run/user/7")
            ),
            vec![
                DbusAddress::Abstract("/tmp/dbus-ab=c".into()),
                DbusAddress::Path("/run/user/7/bus".into())
            ]
        );
        assert!(parse_bus_address("tcp:host=h,port=1", None).is_empty());
        assert!(parse_bus_address("unix:runtime=yes", None).is_empty());
        assert!(parse_bus_address("unix:path=%zz", None).is_empty());
    }

    #[test]
    fn handshake_authenticates_and_reads_the_hello_reply() {
        let (conn, bus) = connect(":1.77");
        assert_eq!(conn.unique_name(), ":1.77");
        // uid 1000 is "1000" in ASCII hex.
        assert_eq!(
            bus.auth_lines(),
            vec!["AUTH EXTERNAL 31303030".to_string(), "BEGIN".to_string()]
        );
        assert_eq!(bus.received().len(), 1);
        assert_eq!(bus.received()[0].member.as_deref(), Some("Hello"));
    }

    #[test]
    fn rejected_authentication_is_an_error() {
        let (transport, bus) = ScriptedDbusTransport::new(":1.1");
        bus.reject_auth();
        let result = DbusConnection::from_transport(Box::new(transport), "scripted:", 1000);
        assert_eq!(result.err(), Some("D-Bus authentication was rejected"));
    }

    #[test]
    fn rejected_hello_is_an_error() {
        let (transport, bus) = ScriptedDbusTransport::new(":1.1");
        bus.fail_hello();
        let result = DbusConnection::from_transport(Box::new(transport), "scripted:", 1000);
        assert_eq!(result.err(), Some("The D-Bus Hello call was rejected"));
    }

    #[test]
    fn hello_reply_split_across_reads_is_reassembled() {
        let (transport, bus) = ScriptedDbusTransport::new(":1.9");
        bus.limit_read_size(3);
        let conn = DbusConnection::from_transport(Box::new(transport), "scripted:", 1000).unwrap();
        assert_eq!(conn.unique_name(), ":1.9");
    }

    #[test]
    fn read_incoming_frames_back_to_back_and_split_messages() {
        let (mut conn, bus) = connect(":1.2");
        let mut first = DbusMessage::signal("/a", "org.test", "First", 10);
        first.append_string("one");
        let second = DbusMessage::signal("/b", "org.test", "Second", 11);
        let mut stream = first.to_bytes();
        stream.extend_from_slice(&second.to_bytes());

        // Both at once.
        bus.push_to_client(&stream);
        let got = conn.read_incoming().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].member.as_deref(), Some("First"));
        assert_eq!(got[0].body_string().as_deref(), Some("one"));
        assert_eq!(got[1].member.as_deref(), Some("Second"));
        assert_eq!(got[1].path.as_deref(), Some("/b"));

        // Split mid-header and mid-body across polls: nothing until the last byte.
        let split = first.to_bytes();
        bus.push_to_client(&split[..10]);
        assert!(conn.read_incoming().unwrap().is_empty());
        bus.push_to_client(&split[10..split.len() - 1]);
        assert!(conn.read_incoming().unwrap().is_empty());
        bus.push_to_client(&split[split.len() - 1..]);
        let got = conn.read_incoming().unwrap();
        assert_eq!(got, vec![DbusMessage::from_bytes(&split).unwrap()]);
    }

    #[test]
    fn complete_messages_are_delivered_before_a_bad_frame_closes() {
        let (mut conn, bus) = connect(":1.2");
        let first = DbusMessage::signal("/a", "org.test", "First", 10);
        let mut stream = first.to_bytes();
        stream.extend_from_slice(&[b'x'; 32]);
        bus.push_to_client(&stream);
        let got = conn.read_incoming().unwrap();
        assert_eq!(got, vec![DbusMessage::from_bytes(&first.to_bytes()).unwrap()]);
        assert!(!conn.is_live_socket());
        assert!(conn.read_incoming().is_err());
    }

    #[test]
    fn complete_messages_are_delivered_before_eof_closes() {
        let (mut conn, bus) = connect(":1.2");
        let first = DbusMessage::signal("/a", "org.test", "First", 10);
        bus.push_to_client(&first.to_bytes());
        bus.close_after_queued();
        let got = conn.read_incoming().unwrap();
        assert_eq!(got, vec![DbusMessage::from_bytes(&first.to_bytes()).unwrap()]);
        assert!(!conn.is_live_socket());
        assert!(conn.read_incoming().is_err());
    }

    #[test]
    fn messages_queued_while_waiting_for_a_reply_survive_a_close() {
        let (mut conn, bus) = connect(":1.2");
        let sig = DbusMessage::signal("/a", "org.test", "First", 10);
        bus.push_to_client(&sig.to_bytes());
        bus.close_after_queued();
        assert!(conn.wait_for_reply(9999).is_err());
        let got = conn.read_incoming().unwrap();
        assert_eq!(got, vec![DbusMessage::from_bytes(&sig.to_bytes()).unwrap()]);
        assert!(conn.read_incoming().is_err());
    }

    #[test]
    fn corrupt_stream_closes_the_connection() {
        let (mut conn, bus) = connect(":1.2");
        bus.push_to_client(&[b'x'; 32]);
        assert!(conn.read_incoming().is_err());
        assert!(!conn.is_live_socket());
        assert!(conn.send_message(call("Ping", 1)).is_err());
    }

    #[test]
    fn closed_peer_is_an_error() {
        let (transport, bus) = ScriptedDbusTransport::new(":1.2");
        let mut conn = DbusConnection::from_transport(Box::new(transport), "s:", 1).unwrap();
        // EOF is modelled by the transport reporting 0 bytes: drop the bus so reads stay
        // WouldBlock, then check an explicit Ok(0) path through a one-off transport.
        drop(bus);
        assert!(conn.read_incoming().unwrap().is_empty());

        struct Eof;
        impl Read for Eof {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
        }
        impl Write for Eof {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl DbusTransport for Eof {
            fn socket_fd(&self) -> SocketDescriptor {
                -1
            }
            fn set_nonblocking(&mut self, _: bool) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(DbusConnection::from_transport(Box::new(Eof), "eof:", 1).is_err());
    }

    #[test]
    fn would_block_on_write_queues_whole_messages_only() {
        let (mut conn, bus) = connect(":1.2");
        let before = bus.received().len();
        bus.set_write_budget(Some(10));
        let mut msg = call("Ping", 100);
        msg.append_string("a long enough argument to span many writes");
        let packet_len = msg.to_bytes().len();
        conn.send_message(msg.clone()).unwrap();
        // Only 10 bytes went out: no message reached the bus yet, the rest is queued.
        assert_eq!(bus.received().len(), before);
        assert_eq!(conn.pending_output(), packet_len - 10);

        // Another send while stuck queues behind the first instead of interleaving.
        conn.send_message(call("Pong", 101)).unwrap();
        assert_eq!(bus.received().len(), before);

        bus.set_write_budget(None);
        conn.read_incoming().unwrap();
        assert_eq!(conn.pending_output(), 0);
        let received = bus.received();
        assert_eq!(received.len(), before + 2);
        assert_eq!(received[before].member.as_deref(), Some("Ping"));
        assert_eq!(received[before].body_string(), msg.body_string());
        assert_eq!(received[before + 1].member.as_deref(), Some("Pong"));
    }

    #[test]
    fn stuck_bus_backlog_is_bounded() {
        let (mut conn, bus) = connect(":1.2");
        bus.set_write_budget(Some(0));
        let mut sent = 0usize;
        loop {
            let mut msg = call("Ping", 1);
            msg.append_string(&"x".repeat(64 * 1024));
            match conn.send_message(msg) {
                Ok(()) => sent += 1,
                Err(e) => {
                    assert_eq!(e, "The D-Bus send buffer is full");
                    break;
                }
            }
            assert!(sent < 64, "backlog never bounded");
        }
        assert!(conn.pending_output() <= MAX_TX_BACKLOG);
    }

    #[test]
    fn oversized_signature_is_rejected_before_encoding() {
        let (mut conn, _bus) = connect(":1.2");
        let mut msg = call("Ping", 1);
        for _ in 0..256 {
            msg.append_u32(0);
        }
        assert_eq!(
            conn.send_message(msg),
            Err("D-Bus signature exceeds 255 bytes")
        );
    }

    #[test]
    fn socket_notifier_only_for_a_real_descriptor() {
        use ferrokit_core::event_loop::UnixEventDispatcher;
        let (conn, bus) = connect(":1.2");
        let mut dispatcher = UnixEventDispatcher::new();
        assert!(conn.bind_event_dispatcher(&mut dispatcher).is_none());
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let (stream, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
            bus.set_socket_fd(stream.as_raw_fd());
            assert!(conn.bind_event_dispatcher(&mut dispatcher).is_some());
        }
        #[cfg(not(target_os = "linux"))]
        let _ = bus;
    }
}
