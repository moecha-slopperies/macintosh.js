use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};
use tauri::AppHandle;
use url::Url;

const MAX_HEADER_BYTES: usize = 16 * 1024;

pub struct LoopbackServer {
    pub base_url: Url,
}

pub fn start(app: &AppHandle) -> Result<LoopbackServer, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("could not start the local asset server: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("could not read the local asset server address: {error}"))?;
    let base_url = Url::parse(&format!("http://127.0.0.1:{}/", address.port()))
        .map_err(|error| format!("could not build the local asset server URL: {error}"))?;

    let app = app.clone();
    thread::Builder::new()
        .name("macintosh.js-assets".to_string())
        .spawn(move || {
            for connection in listener.incoming() {
                let Ok(stream) = connection else {
                    continue;
                };
                let app = app.clone();
                let _ = thread::Builder::new()
                    .name("macintosh.js-asset".to_string())
                    .spawn(move || {
                        if let Err(error) = serve(&app, stream) {
                            eprintln!("local asset request failed: {error}");
                        }
                    });
            }
        })
        .map_err(|error| format!("could not start the local asset server thread: {error}"))?;

    Ok(LoopbackServer { base_url })
}

fn serve(app: &AppHandle, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let request = match read_request(&mut stream)? {
        Some(request) => request,
        None => return write_error(&mut stream, 400, "Bad Request"),
    };
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();

    if !version.starts_with("HTTP/1.") {
        return write_error(&mut stream, 400, "Bad Request");
    }
    if !matches!(method, "GET" | "HEAD") {
        return write_error(&mut stream, 405, "Method Not Allowed");
    }
    let Some(path) = asset_path(target) else {
        return write_error(&mut stream, 400, "Bad Request");
    };
    let Some(asset) = app.asset_resolver().get(path) else {
        return write_error(&mut stream, 404, "Not Found");
    };

    let mut headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n",
        asset.mime_type,
        asset.bytes.len()
    );
    if let Some(csp) = asset.csp_header.as_deref() {
        headers.push_str("Content-Security-Policy: ");
        headers.push_str(csp);
        headers.push_str("\r\n");
    }
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes())?;
    if method != "HEAD" {
        stream.write_all(&asset.bytes)?;
    }
    Ok(())
}

fn read_request(stream: &mut TcpStream) -> io::Result<Option<String>> {
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if request.len() > MAX_HEADER_BYTES {
            return Ok(None);
        }
    }
    let Ok(request) = String::from_utf8(request) else {
        return Ok(None);
    };
    Ok(request.lines().next().map(str::to_string))
}

fn asset_path(target: &str) -> Option<String> {
    if !target.starts_with('/') {
        return None;
    }
    let encoded = target.split(['?', '#']).next()?;
    let decoded = percent_decode(encoded)?;
    if decoded.contains(['\0', '\\']) {
        return None;
    }

    let mut segments = Vec::new();
    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            segment => segments.push(segment),
        }
    }
    if segments.is_empty() {
        Some("index.html".to_string())
    } else {
        Some(segments.join("/"))
    }
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn write_error(stream: &mut TcpStream, status: u16, reason: &str) -> io::Result<()> {
    let body = format!("{status} {reason}\n");
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes())?;
    stream.write_all(body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::asset_path;

    #[test]
    fn serves_root_as_index() {
        assert_eq!(asset_path("/").as_deref(), Some("index.html"));
        assert_eq!(asset_path("/?cache=1").as_deref(), Some("index.html"));
    }

    #[test]
    fn strips_queries_and_normalizes_segments() {
        assert_eq!(
            asset_path("/assets/app.js?v=1").as_deref(),
            Some("assets/app.js")
        );
        assert_eq!(
            asset_path("/assets/./app.js").as_deref(),
            Some("assets/app.js")
        );
        assert_eq!(
            asset_path("/assets/%61pp.js").as_deref(),
            Some("assets/app.js")
        );
    }

    #[test]
    fn rejects_path_traversal() {
        assert_eq!(asset_path("/../secret"), None);
        assert_eq!(asset_path("/assets/%2e%2e/secret"), None);
        assert_eq!(asset_path("/assets/%5c..%5csecret"), None);
        assert_eq!(asset_path("https://example.com/index.html"), None);
    }
}
