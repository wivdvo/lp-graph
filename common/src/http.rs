//! Minimal HTTPS GET + WebSocket client on top of WinHTTP (built into Windows).
//! Only ever talks to 127.0.0.1, where the League client uses a self-signed cert.

use crate::wide;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Networking::WinHttp::*;

struct H(*mut c_void);
impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

/// Session + connection + request kept together so they drop in the right order.
struct Req {
    req: H,
    _conn: H,
    _session: H,
}

fn open(port: u16, verb: &str, path: &str, timeout_ms: i32) -> Option<Req> {
    unsafe {
        let session = H(WinHttpOpen(wide("lp-graph").as_ptr(), WINHTTP_ACCESS_TYPE_NO_PROXY, null(), null(), 0));
        if session.0.is_null() {
            return None;
        }
        WinHttpSetTimeouts(session.0, timeout_ms, timeout_ms, timeout_ms, timeout_ms);
        // HTTP/1.1 only: WebSocket upgrades don't work over HTTP/2.
        let proto: u32 = 0;
        WinHttpSetOption(session.0, WINHTTP_OPTION_ENABLE_HTTP_PROTOCOL, &proto as *const u32 as *const c_void, 4);
        let conn = H(WinHttpConnect(session.0, wide("127.0.0.1").as_ptr(), port, 0));
        if conn.0.is_null() {
            return None;
        }
        let req = H(WinHttpOpenRequest(
            conn.0,
            wide(verb).as_ptr(),
            wide(path).as_ptr(),
            null(),
            null(),
            null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            return None;
        }
        // Self-signed localhost certificate: accept it (host is hard-coded to 127.0.0.1).
        let flags: u32 = SECURITY_FLAG_IGNORE_UNKNOWN_CA
            | SECURITY_FLAG_IGNORE_CERT_CN_INVALID
            | SECURITY_FLAG_IGNORE_CERT_DATE_INVALID
            | SECURITY_FLAG_IGNORE_CERT_WRONG_USAGE;
        WinHttpSetOption(req.0, WINHTTP_OPTION_SECURITY_FLAGS, &flags as *const u32 as *const c_void, 4);
        Some(Req { req, _conn: conn, _session: session })
    }
}

fn send(r: &Req, headers: &str, body: &[u8]) -> Option<u32> {
    unsafe {
        let hdr = wide(headers);
        let (ptr, len) = if body.is_empty() { (null(), 0) } else { (body.as_ptr() as *const c_void, body.len() as u32) };
        if WinHttpSendRequest(r.req.0, hdr.as_ptr(), u32::MAX, ptr, len, len, 0) == 0 {
            return None;
        }
        if WinHttpReceiveResponse(r.req.0, null_mut()) == 0 {
            return None;
        }
        let mut code: u32 = 0;
        let mut size: u32 = 4;
        WinHttpQueryHeaders(
            r.req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            &mut code as *mut u32 as *mut c_void,
            &mut size,
            null_mut(),
        );
        Some(code)
    }
}

/// HTTPS GET to 127.0.0.1:port. Returns the body for 2xx responses.
pub fn get(port: u16, path: &str, auth_header: &str) -> Option<Vec<u8>> {
    request(port, "GET", path, auth_header, &[])
}

/// HTTPS POST with a JSON body to 127.0.0.1:port. Returns the body for 2xx responses.
pub fn post_json(port: u16, path: &str, auth_header: &str, json: &str) -> Option<Vec<u8>> {
    let headers = format!("{auth_header}\r\nContent-Type: application/json");
    request(port, "POST", path, &headers, json.as_bytes())
}

fn request(port: u16, verb: &str, path: &str, headers: &str, body: &[u8]) -> Option<Vec<u8>> {
    let r = open(port, verb, path, 15_000)?;
    // No keep-alive: the client refuses WebSocket upgrades on a reused connection.
    let off: u32 = WINHTTP_DISABLE_KEEP_ALIVE;
    unsafe { WinHttpSetOption(r.req.0, WINHTTP_OPTION_DISABLE_FEATURE, &off as *const u32 as *const c_void, 4) };
    let code = send(&r, headers, body)?;
    if !(200..300).contains(&code) {
        return None;
    }
    let mut body = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let mut read: u32 = 0;
        let ok = unsafe { WinHttpReadData(r.req.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read) };
        if ok == 0 {
            return None;
        }
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buf[..read as usize]);
    }
    Some(body)
}

/// A connected WebSocket to the League client.
pub struct WebSocket {
    ws: H,
    _req: Req,
}

impl WebSocket {
    pub fn connect(port: u16, auth_header: &str) -> Result<WebSocket, String> {
        // 0 = no receive timeout: the tracker thread blocks until the client sends something.
        let err = |what: &str| format!("{what} failed (error {})", unsafe { GetLastError() });
        let r = open(port, "GET", "/", 0).ok_or_else(|| err("open"))?;
        unsafe {
            if WinHttpSetOption(r.req.0, WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET, null(), 0) == 0 {
                return Err(err("upgrade option"));
            }
        }
        let code = send(&r, auth_header, &[]).ok_or_else(|| err("send"))?;
        if code != 101 {
            let mut buf = vec![0u16; 8192];
            let mut size = (buf.len() * 2) as u32;
            unsafe {
                WinHttpQueryHeaders(r.req.0, WINHTTP_QUERY_RAW_HEADERS_CRLF, null(), buf.as_mut_ptr() as *mut c_void, &mut size, null_mut());
            }
            let hdrs = String::from_utf16_lossy(&buf[..size as usize / 2]);
            return Err(format!("unexpected status {code}: {hdrs}"));
        }
        let ws = H(unsafe { WinHttpWebSocketCompleteUpgrade(r.req.0, 0) });
        if ws.0.is_null() {
            return Err(err("complete upgrade"));
        }
        Ok(WebSocket { ws, _req: r })
    }

    pub fn send_text(&self, text: &str) -> bool {
        unsafe {
            WinHttpWebSocketSend(
                self.ws.0,
                WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE,
                text.as_ptr() as *const c_void,
                text.len() as u32,
            ) == 0
        }
    }

    /// Blocks until a full text message arrives. None = connection closed/broken.
    pub fn recv_text(&self) -> Option<String> {
        let mut msg = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read: u32 = 0;
            let mut kind: WINHTTP_WEB_SOCKET_BUFFER_TYPE = 0;
            let err = unsafe {
                WinHttpWebSocketReceive(self.ws.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read, &mut kind)
            };
            if err != 0 {
                return None;
            }
            msg.extend_from_slice(&buf[..read as usize]);
            match kind {
                WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE | WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE => {
                    return Some(String::from_utf8_lossy(&msg).into_owned());
                }
                WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE => return None,
                _ => {} // fragment: keep reading
            }
        }
    }
}
