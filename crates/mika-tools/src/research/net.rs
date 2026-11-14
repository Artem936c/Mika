//! Network of the deep search: only pages of the open internet.
//!
//! Addresses come from search results, not from the user, and nobody is asked
//! about each page. So every address is checked before the request and at
//! every redirect, and the client connects only to public addresses.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use mika_core::policy::blind::is_blind_url;

pub const MAX_REDIRECTS: usize = 5;
pub const ALLOWED_PORTS: [u16; 4] = [80, 443, 8080, 8443];
pub const MAX_URL: usize = 2000;
pub const PAGE_BYTES: usize = 2 * 1024 * 1024;
pub const SEARCH_BYTES: usize = 512 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// The same as in `win/http.rs`.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Mika/0.1";

/// Top-level names that never belong to the open internet.
const LOCAL_SUFFIXES: [&str; 13] = [
    ".localhost",
    ".local",
    ".localdomain",
    ".internal",
    ".intranet",
    ".lan",
    ".home",
    ".corp",
    ".home.arpa",
    ".test",
    ".example",
    ".invalid",
    ".onion",
];

/// Why an address is not fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetRefusal {
    Malformed,
    TooLong,
    Scheme,
    Credentials,
    Port,
    LocalName,
    PrivateAddress,
    Blind,
    TooManyRedirects,
}

impl NetRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            NetRefusal::Malformed => "адрес не разобрать",
            NetRefusal::TooLong => "адрес слишком длинный",
            NetRefusal::Scheme => "не http и не https",
            NetRefusal::Credentials => "в адресе есть имя и пароль",
            NetRefusal::Port => "нестандартный порт",
            NetRefusal::LocalName => "имя из локальной сети",
            NetRefusal::PrivateAddress => "адрес не из открытого интернета",
            NetRefusal::Blind => "страница входа, банка или оплаты",
            NetRefusal::TooManyRedirects => "слишком много переадресаций",
        }
    }
}

impl std::fmt::Display for NetRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for NetRefusal {}

/// The address as the HTTP client will read it, when it may be fetched.
pub fn check_url(url: &str) -> Result<reqwest::Url, NetRefusal> {
    if url.len() > MAX_URL {
        return Err(NetRefusal::TooLong);
    }
    // The parser of the client itself: `http://2130706433/` and `http://0x7f.1/`
    // become `127.0.0.1` here, exactly as they would at the request.
    let parsed = reqwest::Url::parse(url).map_err(|_| NetRefusal::Malformed)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(NetRefusal::Scheme);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(NetRefusal::Credentials);
    }
    let host = parsed
        .host_str()
        .ok_or(NetRefusal::Malformed)?
        .to_lowercase();
    let host = host
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.is_empty() {
        return Err(NetRefusal::Malformed);
    }
    match host.parse::<IpAddr>() {
        Ok(ip) if !is_public_ip(ip) => return Err(NetRefusal::PrivateAddress),
        Ok(_) => {}
        Err(_) if is_local_name(host) => return Err(NetRefusal::LocalName),
        Err(_) => {}
    }
    let port = parsed.port_or_known_default().ok_or(NetRefusal::Port)?;
    if !ALLOWED_PORTS.contains(&port) {
        return Err(NetRefusal::Port);
    }
    // Without the query on purpose: a search request carries the user's words
    // there, and a question about banks must not refuse the search itself.
    let bare = format!("{}://{}{}", parsed.scheme(), host, parsed.path());
    if is_blind_url(&bare) {
        return Err(NetRefusal::Blind);
    }
    Ok(parsed)
}

fn is_local_name(host: &str) -> bool {
    host == "localhost" || !host.contains('.') || LOCAL_SUFFIXES.iter().any(|s| host.ends_with(s))
}

/// Whether an address belongs to the open internet.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let private = a == 0
        || a == 10
        || (a == 100 && (64..=127).contains(&b))
        || a == 127
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224;
    !private
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let seg = ip.segments();
    // Global unicast is 2000::/3. Everything else is local, reserved, carries
    // an IPv4 address inside (`::a.b.c.d`, NAT64) or is multicast.
    if seg[0] & 0xe000 != 0x2000 {
        return false;
    }
    let documentation = seg[0] == 0x2001 && seg[1] == 0x0db8;
    let teredo = seg[0] == 0x2001 && seg[1] == 0;
    let six_to_four = seg[0] == 0x2002;
    !(documentation || teredo || six_to_four)
}

/// Whether the next hop of a redirect may be followed; `hops` hops were made
/// before it.
pub fn redirect_verdict(hops: usize, next: &str) -> Result<(), NetRefusal> {
    if hops >= MAX_REDIRECTS {
        return Err(NetRefusal::TooManyRedirects);
    }
    check_url(next).map(|_| ())
}

/// A page as it came from the network.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    /// The address after redirects.
    pub url: String,
    pub html: String,
    /// The body was longer than the limit and was cut.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NetError {
    Refused(NetRefusal),
    Status(u16),
    NotText(String),
    TimedOut,
    Failed(String),
}

impl NetError {
    /// Why the page was not read, for the report.
    pub fn reason(&self) -> String {
        match self {
            NetError::Refused(r) => r.to_string(),
            NetError::Status(code) => format!("сайт ответил {code}"),
            NetError::NotText(mime) => format!("не текст ({mime})"),
            NetError::TimedOut => {
                format!("сайт не ответил за {} секунд", REQUEST_TIMEOUT.as_secs())
            }
            NetError::Failed(e) => format!("нет связи: {e}"),
        }
    }
}

#[async_trait]
pub trait Net: Send + Sync {
    /// Reads one public web page. The body is cut at `max_bytes`.
    async fn get(&self, url: &str, max_bytes: usize) -> Result<Fetched, NetError>;
}

/// DNS for the guarded client: only public addresses come back, and the
/// client connects only to what comes back. So a DNS answer cannot change
/// between the check and the connection.
struct PublicResolver;

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let found = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let kept: Vec<SocketAddr> = found.filter(|a| is_public_ip(a.ip())).collect();
            if kept.is_empty() {
                let refusal: Box<dyn std::error::Error + Send + Sync> =
                    Box::new(NetRefusal::PrivateAddress);
                return Err(refusal);
            }
            let addrs: reqwest::dns::Addrs = Box::new(kept.into_iter());
            Ok(addrs)
        })
    }
}

/// HTTP client that reads only public pages: no proxy, no cookies, its own
/// DNS, every redirect checked, the body capped.
pub struct GuardedHttp {
    client: reqwest::Client,
}

impl GuardedHttp {
    pub fn new() -> Self {
        Self::with_resolver(Arc::new(PublicResolver))
    }

    fn with_resolver<R: reqwest::dns::Resolve + 'static>(resolver: Arc<R>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            // Behind a proxy the proxy resolves the name, so the address
            // could not be checked.
            .no_proxy()
            .dns_resolver(resolver)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                // `previous` starts with the address of the request itself.
                let hops = attempt.previous().len().saturating_sub(1);
                match redirect_verdict(hops, attempt.url().as_str()) {
                    Ok(()) => attempt.follow(),
                    Err(refusal) => attempt.error(refusal),
                }
            }))
            .build()
            .expect("reqwest client");
        Self { client }
    }
}

impl Default for GuardedHttp {
    fn default() -> Self {
        Self::new()
    }
}

/// The refusal of the resolver or of the redirect check inside a client
/// error, else what went wrong with the connection.
fn send_error(e: reqwest::Error) -> NetError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(err) = source {
        if let Some(refusal) = err.downcast_ref::<NetRefusal>() {
            return NetError::Refused(*refusal);
        }
        source = err.source();
    }
    if e.is_timeout() {
        NetError::TimedOut
    } else {
        NetError::Failed(e.without_url().to_string())
    }
}

/// The type a site named, for the reason text: only the characters a type
/// name is made of, 60 at most. The site writes this header, and the reason
/// goes into the report and to the chat model.
fn mime_name(mime: &str) -> String {
    let name: String = mime
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '+' | '.' | '-'))
        .take(60)
        .collect();
    if name.is_empty() {
        "?".into()
    } else {
        name
    }
}

#[async_trait]
impl Net for GuardedHttp {
    async fn get(&self, url: &str, max_bytes: usize) -> Result<Fetched, NetError> {
        let parsed = check_url(url).map_err(NetError::Refused)?;
        let mut resp = self.client.get(parsed).send().await.map_err(send_error)?;
        check_url(resp.url().as_str()).map_err(NetError::Refused)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(NetError::Status(status.as_u16()));
        }
        let content_type = match resp.headers().get(reqwest::header::CONTENT_TYPE) {
            None => String::new(),
            // A header that is not plain ASCII names no type that is read.
            Some(value) => value
                .to_str()
                .map_err(|_| NetError::NotText("?".into()))?
                .to_string(),
        };
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !matches!(
            mime.as_str(),
            "" | "text/html" | "application/xhtml+xml" | "text/plain"
        ) {
            return Err(NetError::NotText(mime_name(&mime)));
        }
        let final_url = resp.url().to_string();
        let mut bytes: Vec<u8> = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = resp.chunk().await.map_err(send_error)? {
            let room = max_bytes.saturating_sub(bytes.len());
            if chunk.len() > room {
                bytes.extend_from_slice(&chunk[..room]);
                truncated = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Fetched {
            url: final_url,
            html: decode_body(&bytes, &content_type),
            truncated,
        })
    }
}

/// The one guarded client of the process: `register_all` runs many times.
pub fn guarded() -> Arc<dyn Net> {
    static NET: OnceLock<Arc<GuardedHttp>> = OnceLock::new();
    NET.get_or_init(|| Arc::new(GuardedHttp::new())).clone()
}

/// Windows-1251 for the bytes 0x80..=0xBF; 0x98 has no character.
const CP1251_HIGH: [char; 64] = [
    '\u{0402}', '\u{0403}', '\u{201A}', '\u{0453}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{20AC}', '\u{2030}', '\u{0409}', '\u{2039}', '\u{040A}', '\u{040C}', '\u{040B}', '\u{040F}',
    '\u{0452}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{FFFD}', '\u{2122}', '\u{0459}', '\u{203A}', '\u{045A}', '\u{045C}', '\u{045B}', '\u{045F}',
    '\u{00A0}', '\u{040E}', '\u{045E}', '\u{0408}', '\u{00A4}', '\u{0490}', '\u{00A6}', '\u{00A7}',
    '\u{0401}', '\u{00A9}', '\u{0404}', '\u{00AB}', '\u{00AC}', '\u{00AD}', '\u{00AE}', '\u{0407}',
    '\u{00B0}', '\u{00B1}', '\u{0406}', '\u{0456}', '\u{0491}', '\u{00B5}', '\u{00B6}', '\u{00B7}',
    '\u{0451}', '\u{2116}', '\u{0454}', '\u{00BB}', '\u{0458}', '\u{0405}', '\u{0455}', '\u{0457}',
];

fn cp1251_char(byte: u8) -> char {
    match byte {
        0x00..=0x7F => byte as char,
        0x80..=0xBF => CP1251_HIGH[(byte - 0x80) as usize],
        // «А» is 0xC0, «я» is 0xFF.
        _ => char::from_u32(0x0410 + (byte - 0xC0) as u32).unwrap_or('\u{FFFD}'),
    }
}

fn decode_cp1251(bytes: &[u8]) -> String {
    bytes.iter().map(|b| cp1251_char(*b)).collect()
}

/// The value after the first `charset=` of `text`, lower case.
fn charset_in(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("charset=")? + "charset=".len();
    let name: String = lower[at..]
        .trim_start_matches(['"', '\'', ' '])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Text of a page body: UTF-8, or Windows-1251 when the page says so or is
/// not valid UTF-8.
pub fn decode_body(bytes: &[u8], content_type: &str) -> String {
    let declared = charset_in(content_type).or_else(|| {
        let head = &bytes[..bytes.len().min(2048)];
        charset_in(&String::from_utf8_lossy(head))
    });
    match declared.as_deref() {
        Some("windows-1251" | "cp1251" | "win-1251" | "x-cp1251") => decode_cp1251(bytes),
        Some(_) => String::from_utf8_lossy(bytes).into_owned(),
        None => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
            // A body cut at the size limit may end inside a character.
            Err(e) if e.error_len().is_none() => {
                String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
            }
            Err(_) => decode_cp1251(bytes),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn check_url_refuses_what_is_not_a_public_web_page() {
        use NetRefusal::*;
        let head = "https://example.com/";
        let long = format!("{head}{}", "a".repeat(MAX_URL + 1 - head.len()));
        assert_eq!(long.len(), MAX_URL + 1);
        assert!(check_url(&long[..MAX_URL]).is_ok());
        let cases: Vec<(&str, NetRefusal)> = vec![
            ("file:///C:/Windows/win.ini", Scheme),
            ("ftp://x.org/a", Scheme),
            ("data:text/html,x", Scheme),
            ("javascript:alert(1)", Scheme),
            ("https://user:pw@example.com/", Credentials),
            ("https://user@example.com/", Credentials),
            ("http://example.com:22/", Port),
            ("https://example.com:3389/", Port),
            ("http://localhost/", LocalName),
            ("http://LOCALHOST./", LocalName),
            ("http://router/", LocalName),
            ("http://nas.local/", LocalName),
            ("http://x.internal/", LocalName),
            ("http://printer.lan/", LocalName),
            ("http://something.onion/", LocalName),
            ("http://127.0.0.1/", PrivateAddress),
            ("http://2130706433/", PrivateAddress),
            ("http://0x7f.1/", PrivateAddress),
            ("http://017700000001/", PrivateAddress),
            ("http://127.1/", PrivateAddress),
            ("http://[::1]/", PrivateAddress),
            ("http://[::ffff:10.0.0.1]/", PrivateAddress),
            ("http://[::ffff:7f00:1]/", PrivateAddress),
            ("http://10.1.2.3/", PrivateAddress),
            ("http://172.16.0.1/", PrivateAddress),
            ("http://192.168.1.1/", PrivateAddress),
            ("http://169.254.169.254/latest/meta-data", PrivateAddress),
            ("http://100.64.0.1/", PrivateAddress),
            ("http://0.0.0.0/", PrivateAddress),
            ("http://[fe80::1]/", PrivateAddress),
            ("http://[fd00::1]/", PrivateAddress),
            ("http://[64:ff9b::7f00:1]/", PrivateAddress),
            ("https://accounts.google.com/signin", Blind),
            ("https://shop.example.org/checkout/step2", Blind),
            ("https://online.sberbank.ru/", Blind),
            ("not an address", Malformed),
            ("http://", Malformed),
            ("//example.com/a", Malformed),
            ("", Malformed),
            (long.as_str(), TooLong),
        ];
        for (url, why) in cases {
            assert_eq!(check_url(url), Err(why), "{url}");
        }
    }

    #[test]
    fn check_url_accepts_public_pages_and_the_search_request() {
        for url in [
            "https://ru.wikipedia.org/wiki/Rust",
            "http://example.com:8080/a?b=1",
            "https://example.com:8443/",
            "https://8.8.8.8/",
            "https://[2606:4700:4700::1111]/",
            // Blind words in the query do not count: they are the user's words.
            "https://html.duckduckgo.com/html/?q=best+bank+login+payment",
            "https://example.com/author/ivan",
        ] {
            assert!(check_url(url).is_ok(), "{url}");
        }
        // The address comes back as the client will read it.
        assert_eq!(
            check_url("HTTPS://Example.COM/a b").unwrap().as_str(),
            "https://example.com/a%20b"
        );
    }

    #[test]
    fn public_addresses_are_told_from_the_rest() {
        let private = [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "10.255.255.255",
            "100.64.0.1",
            "100.127.255.255",
            "127.0.0.1",
            "127.255.255.255",
            "169.254.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.0.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::7f00:1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.1",
            "64:ff9b::a00:1",
            "100::1",
            "2001:db8::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2002:c0a8:101::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "4000::1",
        ];
        for ip in private {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip}");
        }
        let public = [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.255.255",
            "172.32.0.1",
            "192.0.1.1",
            "192.167.255.255",
            "198.17.255.255",
            "198.20.0.1",
            "223.255.255.255",
            "::ffff:8.8.8.8",
            "2606:4700:4700::1111",
            "2a02:6b8::feed:0ff",
            "2001:4860:4860::8888",
        ];
        for ip in public {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn redirects_are_counted_and_checked() {
        let next = "https://example.com/next";
        for hops in 0..MAX_REDIRECTS {
            assert_eq!(redirect_verdict(hops, next), Ok(()), "{hops}");
        }
        // Five hops were made: the sixth is one too many.
        assert_eq!(
            redirect_verdict(MAX_REDIRECTS, next),
            Err(NetRefusal::TooManyRedirects)
        );
        assert_eq!(
            redirect_verdict(0, "http://192.168.0.1/"),
            Err(NetRefusal::PrivateAddress)
        );
        assert_eq!(redirect_verdict(0, "file:///x"), Err(NetRefusal::Scheme));
        assert_eq!(
            redirect_verdict(1, "https://example.com/login"),
            Err(NetRefusal::Blind)
        );
    }

    #[test]
    fn refusals_and_errors_have_plain_reasons() {
        assert_eq!(
            NetRefusal::PrivateAddress.to_string(),
            "адрес не из открытого интернета"
        );
        assert_eq!(
            NetError::Refused(NetRefusal::Blind).reason(),
            "страница входа, банка или оплаты"
        );
        assert_eq!(NetError::Status(404).reason(), "сайт ответил 404");
        assert_eq!(
            NetError::NotText("application/pdf".into()).reason(),
            "не текст (application/pdf)"
        );
        assert_eq!(NetError::TimedOut.reason(), "сайт не ответил за 20 секунд");
        assert_eq!(
            NetError::Failed("обрыв".into()).reason(),
            "нет связи: обрыв"
        );
    }

    #[tokio::test]
    async fn resolver_gives_no_local_address() {
        use reqwest::dns::Resolve;
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        let err = match PublicResolver.resolve(name).await {
            Ok(addrs) => panic!("local addresses came back: {:?}", addrs.collect::<Vec<_>>()),
            Err(e) => e,
        };
        assert_eq!(
            err.downcast_ref::<NetRefusal>(),
            Some(&NetRefusal::PrivateAddress)
        );
    }

    /// A server on 127.0.0.1 that answers every connection with the reply of
    /// the first route its path starts with. Counts the connections.
    async fn serve(routes: Vec<(&'static str, Vec<u8>)>) -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                seen.fetch_add(1, Ordering::SeqCst);
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(n) if n > 0 => buf.extend_from_slice(&tmp[..n]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let reply = routes
                    .iter()
                    .find(|(prefix, _)| path.starts_with(prefix))
                    .map(|(_, reply)| reply.clone())
                    .unwrap_or_else(|| {
                        b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_vec()
                    });
                let _ = sock.write_all(&reply).await;
                let _ = sock.shutdown().await;
            }
        });
        (port, hits)
    }

    fn reply(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn redirect(to: &str) -> Vec<u8> {
        reply("302 Found", &format!("location: {to}\r\n"), b"")
    }

    /// Sends every name to the test server: stands in for public DNS.
    struct ToLoopback(u16);

    impl reqwest::dns::Resolve for ToLoopback {
        fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            let addr = SocketAddr::from(([127, 0, 0, 1], self.0));
            Box::pin(async move {
                let addrs: reqwest::dns::Addrs = Box::new(std::iter::once(addr));
                Ok(addrs)
            })
        }
    }

    /// The guarded client with every public name answered by the test server.
    fn client_to(port: u16) -> GuardedHttp {
        GuardedHttp::with_resolver(Arc::new(ToLoopback(port)))
    }

    #[tokio::test]
    async fn local_server_is_refused_without_a_connection() {
        let (port, hits) = serve(vec![(
            "/",
            reply("200 OK", "content-type: text/html\r\n", b"<p>secret</p>"),
        )])
        .await;
        let http = GuardedHttp::new();
        for url in [
            format!("http://127.0.0.1:{port}/"),
            format!("http://localhost:{port}/"),
            format!("http://[::1]:{port}/"),
        ] {
            let got = http.get(&url, PAGE_BYTES).await;
            assert!(
                matches!(
                    got,
                    Err(NetError::Refused(
                        NetRefusal::PrivateAddress | NetRefusal::LocalName
                    ))
                ),
                "{url}: {got:?}"
            );
        }
        assert_eq!(
            http.get(&format!("http://127.0.0.1:{port}/"), PAGE_BYTES)
                .await,
            Err(NetError::Refused(NetRefusal::PrivateAddress))
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_a_local_address_is_refused_at_connect() {
        // `check_url` is skipped on purpose: this is the path of a public-looking
        // name whose DNS answer is local. The real resolver must stop it, and
        // the refusal must come back through the client's error.
        let (port, hits) = serve(vec![(
            "/",
            reply("200 OK", "content-type: text/html\r\n", b"<p>secret</p>"),
        )])
        .await;
        let http = GuardedHttp::new();
        let err = http
            .client
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .map(|r| r.status())
            .map_err(send_error);
        assert_eq!(err, Err(NetError::Refused(NetRefusal::PrivateAddress)));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn page_is_read_with_its_final_address() {
        let (port, hits) = serve(vec![
            ("/start", redirect("http://pages.example.org/final")),
            (
                "/final",
                reply(
                    "200 OK",
                    "content-type: text/html; charset=utf-8\r\n",
                    "<html><body><p>Привет</p></body></html>".as_bytes(),
                ),
            ),
        ])
        .await;
        let got = client_to(port)
            .get("http://pages.example.org/start", PAGE_BYTES)
            .await
            .unwrap();
        assert_eq!(got.url, "http://pages.example.org/final");
        assert!(got.html.contains("Привет") && !got.truncated);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn redirect_to_a_local_address_is_refused_and_not_followed() {
        let (port, hits) = serve(vec![
            ("/to-router", redirect("http://192.168.0.1/admin")),
            (
                "/to-loopback",
                redirect(&format!("http://127.0.0.1:{}/", 9)),
            ),
            ("/to-name", redirect("http://nas.local/")),
            ("/to-file", redirect("file:///C:/Windows/win.ini")),
            ("/to-entry", redirect("https://pages.example.org/login")),
            ("/to-port", redirect("http://pages.example.org:22/")),
        ])
        .await;
        let http = client_to(port);
        for (path, why) in [
            ("/to-router", NetRefusal::PrivateAddress),
            ("/to-loopback", NetRefusal::PrivateAddress),
            ("/to-name", NetRefusal::LocalName),
            ("/to-entry", NetRefusal::Blind),
            ("/to-port", NetRefusal::Port),
        ] {
            assert_eq!(
                http.get(&format!("http://pages.example.org{path}"), PAGE_BYTES)
                    .await,
                Err(NetError::Refused(why)),
                "{path}"
            );
        }
        // The client itself may refuse a scheme it cannot speak before the
        // policy is asked; either way nothing is read.
        assert!(http
            .get("http://pages.example.org/to-file", PAGE_BYTES)
            .await
            .is_err());
        // One connection for each first request, none for a second hop.
        assert_eq!(hits.load(Ordering::SeqCst), 6);
    }

    #[tokio::test]
    async fn sixth_redirect_is_one_too_many() {
        let (port, hits) = serve(vec![
            ("/r1", redirect("/r2")),
            ("/r2", redirect("/r3")),
            ("/r3", redirect("/r4")),
            ("/r4", redirect("/r5")),
            ("/r5", redirect("/r6")),
            ("/r6", redirect("/r7")),
            (
                "/r7",
                reply("200 OK", "content-type: text/html\r\n", b"<p>end</p>"),
            ),
        ])
        .await;
        let http = client_to(port);
        // Five hops from /r2 reach the page.
        let got = http
            .get("http://pages.example.org/r2", PAGE_BYTES)
            .await
            .unwrap();
        assert_eq!(got.url, "http://pages.example.org/r7");
        let before = hits.load(Ordering::SeqCst);
        // Six hops from /r1 do not.
        assert_eq!(
            http.get("http://pages.example.org/r1", PAGE_BYTES).await,
            Err(NetError::Refused(NetRefusal::TooManyRedirects))
        );
        assert_eq!(hits.load(Ordering::SeqCst) - before, 6);
    }

    #[tokio::test]
    async fn status_type_and_size_are_checked() {
        let big = "я".repeat(4000);
        let (port, _) = serve(vec![
            ("/gone", reply("404 Not Found", "", b"")),
            (
                "/file.pdf",
                reply("200 OK", "content-type: Application/PDF\r\n", b"%PDF-1.7"),
            ),
            (
                "/json",
                reply("200 OK", "content-type: application/json\r\n", b"{}"),
            ),
            (
                "/plain",
                reply("200 OK", "content-type: text/plain\r\n", b"just text"),
            ),
            ("/untyped", reply("200 OK", "", b"<p>no type</p>")),
            (
                "/big",
                reply("200 OK", "content-type: text/html\r\n", big.as_bytes()),
            ),
        ])
        .await;
        let http = client_to(port);
        let get = |path: &'static str, max: usize| {
            let http = &http;
            async move {
                http.get(&format!("http://pages.example.org{path}"), max)
                    .await
            }
        };
        assert_eq!(get("/gone", 1000).await, Err(NetError::Status(404)));
        assert_eq!(
            get("/file.pdf", 1000).await,
            Err(NetError::NotText("application/pdf".into()))
        );
        assert_eq!(
            get("/json", 1000).await,
            Err(NetError::NotText("application/json".into()))
        );
        assert_eq!(get("/plain", 1000).await.unwrap().html, "just text");
        assert_eq!(get("/untyped", 1000).await.unwrap().html, "<p>no type</p>");
        // 8000 bytes of two-byte letters cut at an odd byte: still UTF-8.
        let cut = get("/big", 1001).await.unwrap();
        assert!(cut.truncated);
        assert_eq!(cut.html, "я".repeat(500));
        let whole = get("/big", 8000).await.unwrap();
        assert!(!whole.truncated);
        assert_eq!(whole.html.chars().count(), 4000);
    }

    fn to_cp1251(text: &str) -> Vec<u8> {
        text.chars()
            .map(|c| {
                (0u8..=255)
                    .find(|b| cp1251_char(*b) == c)
                    .unwrap_or_else(|| panic!("no Windows-1251 byte for {c:?}"))
            })
            .collect()
    }

    #[test]
    fn body_is_decoded_as_utf8_or_windows_1251() {
        let text = "<p>Привет, мир — «ёЁ» №5 ї</p>";
        assert_eq!(
            decode_body(text.as_bytes(), "text/html; charset=UTF-8"),
            text
        );
        assert_eq!(decode_body(text.as_bytes(), "text/html"), text);
        let bytes = to_cp1251(text);
        assert_eq!(decode_body(&bytes, "text/html; charset=windows-1251"), text);
        assert_eq!(decode_body(&bytes, "text/html; charset=\"CP1251\""), text);
        // No charset anywhere and not valid UTF-8: Windows-1251.
        assert_eq!(decode_body(&bytes, "text/html"), text);
        // The page itself names the charset.
        let page = format!("<html><head><meta charset=\"windows-1251\"></head>{text}</html>");
        assert_eq!(decode_body(&to_cp1251(&page), ""), page);
        let old = format!(
            "<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1251\">{text}"
        );
        assert_eq!(decode_body(&to_cp1251(&old), "text/html"), old);
        // The header wins over the page.
        assert_eq!(
            decode_body(page.as_bytes(), "text/html; charset=utf-8"),
            page
        );
        // Another charset is read as UTF-8 with what cannot be read replaced.
        assert_eq!(
            decode_body(b"caf\xe9", "text/html; charset=iso-8859-1"),
            "caf\u{FFFD}"
        );
        // The byte without a character.
        assert_eq!(
            decode_body(b"\x98", "text/html; charset=cp1251"),
            "\u{FFFD}"
        );
        // A UTF-8 body cut inside a character stays UTF-8.
        let cut = &"привет".as_bytes()[..5];
        assert_eq!(decode_body(cut, "text/html"), "пр");
    }

    #[test]
    fn windows_1251_table_is_whole() {
        assert_eq!(cp1251_char(0xC0), 'А');
        assert_eq!(cp1251_char(0xDF), 'Я');
        assert_eq!(cp1251_char(0xE0), 'а');
        assert_eq!(cp1251_char(0xFF), 'я');
        assert_eq!(cp1251_char(0xA8), 'Ё');
        assert_eq!(cp1251_char(0xB8), 'ё');
        assert_eq!(cp1251_char(0xB9), '№');
        assert_eq!(cp1251_char(0xAB), '«');
        assert_eq!(cp1251_char(0xBB), '»');
        assert_eq!(cp1251_char(0x97), '—');
        assert_eq!(cp1251_char(0x41), 'A');
        let high: String = (0x80u8..=0xBF).map(cp1251_char).collect();
        assert_eq!(
            high,
            "ЂЃ‚ѓ„…†‡€‰Љ‹ЊЌЋЏђ‘’“”•–—\u{FFFD}™љ›њќћџ\u{A0}ЎўЈ¤Ґ¦§Ё©Є«¬\u{AD}®Ї°±Ііґµ¶·ё№є»јЅѕї"
        );
    }

    /// Reads a real page and a real result page of the search engine.
    ///
    /// `cargo test -p mika-tools live_guarded_fetch -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs the internet: reads example.com and a DuckDuckGo result page"]
    async fn live_guarded_fetch() {
        let http = GuardedHttp::new();
        let page = http.get("https://example.com/", PAGE_BYTES).await.unwrap();
        assert!(page.html.contains("Example Domain"), "{}", page.html);
        let results = http
            .get(&crate::web::search_url("rust язык"), SEARCH_BYTES)
            .await
            .unwrap();
        let found = crate::web::parse_duckduckgo(&results.html);
        assert!(
            !found.is_empty(),
            "no results in {} bytes",
            results.html.len()
        );
    }
}
