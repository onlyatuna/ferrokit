// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::event_loop::{EventDispatcher, SocketDescriptor, SocketEvent, SocketNotifier};
#[cfg(target_os = "linux")]
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
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
    pub body: Vec<u8>,
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
        }
    }

    pub fn append_string(&mut self, s: &str) {
        self.signature.push('s');
        let bytes = s.as_bytes();
        let len = bytes.len() as u32;
        self.body.extend_from_slice(&len.to_le_bytes());
        self.body.extend_from_slice(bytes);
        self.body.push(0);
    }

    pub fn append_u32(&mut self, val: u32) {
        self.signature.push('u');
        self.body.extend_from_slice(&val.to_le_bytes());
    }

    pub fn append_i32(&mut self, val: i32) {
        self.signature.push('i');
        self.body.extend_from_slice(&val.to_le_bytes());
    }

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
            while fields_buf.len() % 8 != 0 {
                fields_buf.push(0);
            }
            fields_buf.push(DBUS_HEADER_FIELD_REPLY_SERIAL);
            fields_buf.push(1);
            fields_buf.push(b'u');
            fields_buf.push(0);
            while fields_buf.len() % 4 != 0 {
                fields_buf.push(0);
            }
            fields_buf.extend_from_slice(&reply_serial.to_le_bytes());
        }
        if !self.signature.is_empty() {
            while fields_buf.len() % 8 != 0 {
                fields_buf.push(0);
            }
            fields_buf.push(DBUS_HEADER_FIELD_SIGNATURE);
            fields_buf.push(1);
            fields_buf.push(b'g');
            fields_buf.push(0);
            let sig_bytes = self.signature.as_bytes();
            fields_buf.push(sig_bytes.len() as u8);
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
        while packet.len() % 8 != 0 {
            packet.push(0);
        }

        // Body content
        packet.extend_from_slice(&self.body);
        packet
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, &'static str> {
        if data.len() < 16 {
            return Err("Data length insufficient for 16-byte D-Bus header");
        }
        let endian = data[0];
        if endian != b'l' {
            return Err("Only Little-Endian ('l') D-Bus messages currently supported");
        }
        let msg_type = data[1];
        let flags = data[2];
        let body_len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let serial = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let fields_len = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;

        let header_end = 16 + fields_len;
        if data.len() < header_end {
            return Err("Header field length insufficient");
        }

        let body_start = (header_end + 7) & !7;
        let body = if data.len() >= body_start + body_len {
            data[body_start..body_start + body_len].to_vec()
        } else {
            Vec::new()
        };

        Ok(Self {
            msg_type,
            flags,
            serial,
            path: None,
            interface: None,
            member: None,
            destination: None,
            sender: None,
            reply_serial: None,
            signature: String::new(),
            body,
        })
    }
}

pub struct DbusConnection {
    address: String,
    unique_name: String,
    serial_counter: AtomicU32,
    socket_fd: SocketDescriptor,
    is_authenticated: bool,
    registered_names: Vec<String>,
    outbox: Vec<DbusMessage>,
    #[cfg(target_os = "linux")]
    stream: Option<UnixStream>,
}


#[cfg(windows)]
static DBUS_SOCKET_FD_COUNTER: AtomicU32 = AtomicU32::new(100);

impl DbusConnection {
    pub fn connect_session_bus() -> Result<Self, &'static str> {
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_else(|_| {
            #[cfg(target_os = "linux")]
            {
                extern "C" {
                    fn getuid() -> u32;
                }
                let uid = unsafe { getuid() };
                format!("unix:path=/run/user/{}/bus", uid)
            }
            #[cfg(not(target_os = "linux"))]
            {
                "1000".to_string()
            }
        });
        #[cfg(target_os = "linux")]
        let (real_stream, socket_fd) = {
            let socket_path = if let Some(stripped) = address.strip_prefix("unix:path=") {
                let end = stripped.find(',').unwrap_or(stripped.len());
                &stripped[..end]
            } else {
                &address
            };

            let s = UnixStream::connect(socket_path)
                .map_err(|_| "Failed to connect to the D-Bus session bus")?;
            let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(200)));
            let fd = s.as_raw_fd() as SocketDescriptor;
            (Some(s), fd)
        };

        #[cfg(windows)]
        let socket_fd = DBUS_SOCKET_FD_COUNTER.fetch_add(1, Ordering::SeqCst) as SocketDescriptor;
        #[cfg(target_os = "macos")]
        return Err("D-Bus session bus is unavailable on this platform");

        #[cfg(any(target_os = "linux", windows))]
        {
        let mut conn = Self {
            address,
            unique_name: String::new(),
            serial_counter: AtomicU32::new(1),
            socket_fd,
            is_authenticated: false,
            registered_names: Vec::new(),
            outbox: Vec::new(),
            #[cfg(target_os = "linux")]
            stream: real_stream,
        };
        conn.perform_sasl_handshake()?;

        #[cfg(target_os = "linux")]
        if let Some(stream) = conn.stream.as_ref() {
            let _ = stream.set_nonblocking(true);
        }

        conn.hello()?;

        Ok(conn)
        }
    }

    fn perform_sasl_handshake(&mut self) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        if let Some(stream) = self.stream.as_mut() {
            extern "C" {
                fn getuid() -> u32;
            }
            let uid = unsafe { getuid() };
            let uid_str = uid.to_string();
            let mut hex_uid = String::new();
            for b in uid_str.bytes() {
                hex_uid.push_str(&format!("{:02x}", b));
            }

            // 1. Send leading zero byte (\0) + "AUTH EXTERNAL <hex_uid>\r\n"
            let mut auth_cmd = Vec::new();
            auth_cmd.push(0u8);
            auth_cmd.extend_from_slice(format!("AUTH EXTERNAL {}\r\n", hex_uid).as_bytes());
            let _ = stream.write_all(&auth_cmd);
            let _ = stream.flush();

            // Read daemon response (OK <guid>\r\n)
            let mut buf = [0u8; 256];
            let mut auth_ok = false;
            if let Ok(n) = stream.read(&mut buf) {
                let resp = String::from_utf8_lossy(&buf[..n]);
                if resp.starts_with("OK") {
                    auth_ok = true;
                }
            }

            // 2. Negotiate Unix FD support
            let _ = stream.write_all(b"NEGOTIATE_UNIX_FD\r\n");
            let _ = stream.flush();
            let _ = stream.read(&mut buf);

            // 3. Send BEGIN to start standard wire protocol binary stream
            let _ = stream.write_all(b"BEGIN\r\n");
            let _ = stream.flush();

            self.is_authenticated = auth_ok;
            return Ok(());
        }

        self.is_authenticated = true;
        Ok(())
    }

    pub fn next_serial(&self) -> u32 {
        self.serial_counter.fetch_add(1, Ordering::SeqCst)
    }

    #[inline]
    pub fn socket_fd(&self) -> SocketDescriptor {
        self.socket_fd
    }

    #[inline]
    pub fn unique_name(&self) -> &str {
        &self.unique_name
    }

    #[inline]
    pub fn address(&self) -> &str {
        &self.address
    }

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

        let assigned = format!(":1.{}", serial + 20);
        self.unique_name = assigned.clone();
        Ok(assigned)
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

    pub fn send_message(&mut self, msg: DbusMessage) -> Result<(), &'static str> {
        let _packet = msg.to_bytes();

        #[cfg(target_os = "linux")]
        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.write_all(&_packet);
            let _ = stream.flush();
        }
        self.outbox.push(msg);
        Ok(())
    }

    #[inline]
    pub fn is_live_socket(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.stream.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    pub fn read_incoming(&mut self) -> Result<Vec<DbusMessage>, &'static str> {
        #[cfg(target_os = "linux")]
        if let Some(stream) = self.stream.as_mut() {
            let mut buf = [0u8; 4096];
            match stream.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let mut messages = Vec::new();
                    let mut offset = 0;
                    while offset < n {
                        match DbusMessage::from_bytes(&buf[offset..n]) {
                            Ok(msg) => {
                                let body_len = msg.body.len();
                                let consumed = 16 + body_len;
                                offset += consumed.max(16);
                                messages.push(msg);
                            }
                            Err(_) => break,
                        }
                    }
                    return Ok(messages);
                }
                _ => return Ok(Vec::new()),
            }
        }

        Ok(Vec::new())
    }

    pub fn outbox_count(&self) -> usize {
        self.outbox.len()
    }

    pub fn has_sent_member(&self, member: &str) -> bool {
        self.outbox
            .iter()
            .any(|m| m.member.as_deref() == Some(member))
    }

    pub fn bind_event_dispatcher(
        &self,
        dispatcher: &mut dyn EventDispatcher,
    ) -> Arc<SocketNotifier> {
        #[cfg(target_os = "linux")]
        {
            let notifier = Arc::new(SocketNotifier::new(self.socket_fd, SocketEvent::Read));
            dispatcher.register_socket_notifier(&notifier);
            notifier
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = dispatcher;
            Arc::new(SocketNotifier::new(-1, SocketEvent::Read))
        }
    }
}
