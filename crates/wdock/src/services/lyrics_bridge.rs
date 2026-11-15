//! Loopback endpoint for the browser companion and the Spotify lyrics bridge.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Once;
use std::time::Duration;
use windows::core::HSTRING;
use windows::Data::Json::JsonObject;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

const PORT: u16 = 43179;

pub fn start() {
    static START: Once = Once::new();
    START.call_once(|| {
        let _ = std::thread::Builder::new().name("wdock-lyrics-bridge".into()).spawn(|| {
            let listener = match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, PORT)) {
                Ok(listener) => listener,
                Err(err) => { log::warn!("lyrics companion unavailable: {err}"); return; }
            };
            let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().is_ok() };
            for stream in listener.incoming().flatten() { reply(stream); }
            if initialized { unsafe { CoUninitialize() }; }
        });
    });
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars().take(1024) {
        match ch {
            '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn spotify_origin_allowed(header: &str) -> bool {
    header.lines().any(|line| line.trim().eq_ignore_ascii_case("origin: https://xpui.app.spotify.com"))
}

fn reply(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let mut data = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let (header_end, content_length) = loop {
        let Ok(n) = stream.read(&mut chunk) else { return; };
        if n == 0 || data.len() + n > 262144 { return; }
        data.extend_from_slice(&chunk[..n]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let Ok(header) = std::str::from_utf8(&data[..pos]) else { return; };
            let length = header.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
            }).unwrap_or(0);
            if length > 240000 { return; }
            break (pos + 4, length);
        }
    };
    while data.len() < header_end + content_length {
        let Ok(n) = stream.read(&mut chunk) else { return; };
        if n == 0 || data.len() + n > 262144 { return; }
        data.extend_from_slice(&chunk[..n]);
    }
    let Ok(header) = std::str::from_utf8(&data[..header_end]) else { return; };
    let mut lines = header.lines();
    let path = lines.next().unwrap_or_default();
    let mut host = false;
    for line in lines {
        host |= line.trim().eq_ignore_ascii_case("host: 127.0.0.1:43179");
    }
    if !host { return; }
    if path == "GET /spotify-sync HTTP/1.1" {
        if !spotify_origin_allowed(header) { return; }
        let ready = super::media::current().is_some_and(|media| super::lyrics::timed(&media).is_some());
        let body = format!("{{\"ready\":{ready}}}");
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: https://xpui.app.spotify.com\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(response.as_bytes());
        return;
    }
    if path == "POST /spotify-timed HTTP/1.1" {
        if !spotify_origin_allowed(header) || content_length == 0 { return; }
        if let Ok(body) = std::str::from_utf8(&data[header_end..header_end + content_length]) { let _ = accept_timed(body); }
        let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
        return;
    }
    if path == "POST /spotify-lyrics HTTP/1.1" {
        if !spotify_origin_allowed(header) || content_length == 0 { return; }
        let Ok(body) = std::str::from_utf8(&data[header_end..header_end + content_length]) else { return; };
        let _ = accept_spotify_lyrics(body);
        let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
        return;
    }
    if path != "GET /now HTTP/1.1" { return; }
    let body = match super::media::current() {
        Some(media) => {
            let lyrics = super::lyrics::timed(&media).or_else(|| super::lyrics::live(&media)).map(|live| format!("{{\"lines\":[{}],\"active\":{}}}", live.lines.iter().map(|line| json_string(line)).collect::<Vec<_>>().join(","), live.active)).unwrap_or_else(|| "null".into());
            format!("{{\"title\":{},\"artist\":{},\"album\":{},\"positionMs\":{},\"durationMs\":{},\"playing\":{},\"lyrics\":{}}}",
                json_string(&media.title), json_string(&media.artist), json_string(&media.album), media.position_now(), media.length_ms, media.playing, lyrics)
        },
        None => "null".into(),
    };
    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = stream.write_all(response.as_bytes());
}

fn accept_timed(body: &str) -> Option<()> {
    use super::lyrics::{TimedLine, accept_timed};
    let json = JsonObject::Parse(&HSTRING::from(body)).ok()?;
    let title = json.GetNamedString(&HSTRING::from("title")).ok()?.to_string();
    let artist = json.GetNamedString(&HSTRING::from("artist")).ok()?.to_string();
    let array = json.GetNamedArray(&HSTRING::from("lines")).ok()?;
    if array.Size().ok()? > 1000 { return None; }
    let mut lines = Vec::new();
    for i in 0..array.Size().ok()? {
        let obj = array.GetObjectAt(i).ok()?;
        let num = |name: &str| obj.GetNamedNumber(&HSTRING::from(name)).ok().filter(|n| n.is_finite() && *n >= 0.0 && *n <= 86400000.0).map(|n| n as u64);
        let start = num("start")?; let end = num("end")?;
        if end < start { return None; }
        let text = obj.GetNamedString(&HSTRING::from("text")).ok()?.to_string();
        if text.is_empty() || text.len() > 1024 { return None; }
        let mut words = Vec::new();
        if let Ok(parts) = obj.GetNamedArray(&HSTRING::from("words")) {
            if parts.Size().ok()? > 256 { return None; }
            for j in 0..parts.Size().ok()? {
                let part = parts.GetObjectAt(j).ok()?;
                let t = |key: &str| part.GetNamedNumber(&HSTRING::from(key)).ok().filter(|n| n.is_finite() && *n >= 0.0 && *n <= 86400000.0).map(|n| n as u64);
                let a = t("start")?; let b = t("end")?; if b < a { return None; }
                let word = part.GetNamedString(&HSTRING::from("text")).ok()?.to_string();
                if word.len() > 1024 { return None; }
                let joined = part.GetNamedBoolean(&HSTRING::from("join")).unwrap_or(false);
                words.push((a, b, word, joined));
            }
        }
        lines.push(TimedLine { start, text, words });
    }
    if lines.windows(2).any(|pair| pair[0].start > pair[1].start) { return None; }
    let mut colors = Vec::new();
    if let Ok(array) = json.GetNamedArray(&HSTRING::from("colors")) {
        let count = array.Size().ok()?;
        if count != 0 && count != 12 && count != 96 { return None; }
        for i in 0..count {
            let color = array.GetStringAt(i).ok()?.to_string();
            if color.len() != 7 || !color.starts_with('#') || !color.is_ascii() { return None; }
            colors.push([u8::from_str_radix(&color[1..3], 16).ok()?, u8::from_str_radix(&color[3..5], 16).ok()?, u8::from_str_radix(&color[5..7], 16).ok()?]);
        }
    }
    accept_timed(title, artist, lines, colors).then_some(())
}

fn accept_spotify_lyrics(body: &str) -> bool {
    let Some((title, artist, lyrics)) = parse_spotify_lyrics(body) else { return false; };
    super::lyrics::accept_live(title, artist, lyrics)
}

fn parse_spotify_lyrics(body: &str) -> Option<(String, String, super::lyrics::LiveLyrics)> {
    let json = JsonObject::Parse(&HSTRING::from(body)).ok()?;
    let read = |name: &str| json.GetNamedString(&HSTRING::from(name)).ok().map(|s| s.to_string());
    let (title, artist) = (read("title")?, read("artist")?);
    let source = read("source").unwrap_or_else(|| "Lyrics Plus".into());
    if !matches!(source.as_str(), "Spicy Lyrics" | "Lyrics Plus") { return None; }
    let array = json.GetNamedArray(&HSTRING::from("lines")).ok()?;
    let count = array.Size().ok()?;
    if count == 0 || count > 12 { return None; }
    let index = json.GetNamedNumber(&HSTRING::from("active")).ok()?;
    if !index.is_finite() || index.fract() != 0.0 || index < 0.0 || index >= f64::from(count) { return None; }
    let mut lines = Vec::new();
    for index in 0..count {
        let value = array.GetStringAt(index).ok()?;
        lines.push(value.to_string());
    }
    let mut colors = Vec::new();
    if let Ok(array) = json.GetNamedArray(&HSTRING::from("colors")) {
        let size = array.Size().ok()?;
        if size != 0 && size != 12 && size != 96 { return None; }
        for i in 0..size {
            let value = array.GetStringAt(i).ok()?.to_string();
            if value.len() != 7 || !value.starts_with('#') || !value.is_ascii() { return None; }
            colors.push([u8::from_str_radix(&value[1..3], 16).ok()?, u8::from_str_radix(&value[3..5], 16).ok()?, u8::from_str_radix(&value[5..7], 16).ok()?]);
        }
    }
    let progress = json.GetNamedNumber(&HSTRING::from("progress")).ok().map(|value| value as f32);
    if progress.is_some_and(|p| !p.is_finite() || !(0.0..=256.0).contains(&p)) { return None; }
    Some((title, artist, super::lyrics::LiveLyrics { lines, active: index as usize, source, colors, progress }))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    #[test]
    fn json_escapes_text_without_corrupting_unicode() {
        assert_eq!(super::json_string("a\"\\\nViệt"), "\"a\\\"\\\\\\u000aViệt\"");
    }

    #[test]
    fn spotify_lyrics_only_accept_the_expected_browser_origin() {
        assert!(super::spotify_origin_allowed("POST /spotify-lyrics HTTP/1.1\r\nOrigin: https://xpui.app.spotify.com\r\n"));
        assert!(!super::spotify_origin_allowed("Origin: https://evil.example\r\n"));
        assert!(!super::spotify_origin_allowed("Origin: https://xpui.app.spotify.com.evil.example\r\n"));
        assert!(!super::spotify_origin_allowed("Host: 127.0.0.1:43179\r\n"));
    }

    #[test]
    fn spotify_payload_validates_line_indices() {
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
        let good = r#"{"title":"Song","artist":"Artist","active":1,"lines":["Before","Current"]}"#;
        let parsed = super::parse_spotify_lyrics(good).unwrap();
        assert_eq!(parsed.2.active, 1);
        assert_eq!(parsed.2.source, "Lyrics Plus");
        assert_eq!(parsed.2.lines, ["Before", "Current"]);
        let spicy = r##"{"title":"Song","artist":"Artist","source":"Spicy Lyrics","active":0,"lines":["Line"],"colors":["#010203","#010203","#010203","#010203","#010203","#010203","#010203","#010203","#010203","#010203","#010203","#010203"]}"##;
        let parsed = super::parse_spotify_lyrics(spicy).unwrap();
        assert_eq!(parsed.2.source, "Spicy Lyrics");
        assert_eq!(parsed.2.colors, vec![[1, 2, 3]; 12]);
        let dynamic = format!(r##"{{"title":"Song","artist":"Artist","source":"Spicy Lyrics","active":0,"progress":4.5,"lines":["Line"],"colors":[{}]}}"##,
            vec![r##""#010203""##; 96].join(","));
        let parsed = super::parse_spotify_lyrics(&dynamic).unwrap();
        assert_eq!(parsed.2.colors.len(), 96);
        assert_eq!(parsed.2.progress, Some(4.5));
        let invalid = r#"{"title":"Song","artist":"Artist","active":2,"lines":["Before","Current"]}"#;
        assert!(super::parse_spotify_lyrics(invalid).is_none());
    }

    #[test]
    fn local_endpoint_returns_a_json_snapshot() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            super::reply(stream);
        });
        let mut client = std::net::TcpStream::connect(address).unwrap();
        client.write_all(b"GET /now HTTP/1.1\r\nHost: 127.0.0.1:43179\r\n\r\n").unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        worker.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: application/json"));
        assert!(response.ends_with("\r\n\r\nnull"));
    }

    #[test]
    fn sync_status_accepts_only_spotify_origin() {
        fn request(origin: &str) -> String {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let worker = std::thread::spawn(move || { let (stream, _) = listener.accept().unwrap(); super::reply(stream); });
            let mut client = std::net::TcpStream::connect(address).unwrap();
            write!(client, "GET /spotify-sync HTTP/1.1\r\nHost: 127.0.0.1:43179\r\nOrigin: {origin}\r\n\r\n").unwrap();
            let mut response = String::new(); client.read_to_string(&mut response).unwrap(); worker.join().unwrap(); response
        }
        let response = request("https://xpui.app.spotify.com");
        assert!(response.contains("Access-Control-Allow-Origin: https://xpui.app.spotify.com"));
        assert!(response.contains("\"ready\":"));
        assert!(request("https://evil.example").is_empty());
    }
}
