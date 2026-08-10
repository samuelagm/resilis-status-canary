use std::env;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

const DEFAULT_PORT: u16 = 8080;
const WORKER_COUNT: usize = 2;
const QUEUE_CAPACITY: usize = 8;
const WORKER_STACK_SIZE: usize = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const ROOT_BODY: &[u8] = b"Resilis deployment canary\n";
const HEALTH_BODY: &[u8] = b"ok\n";
const NOT_FOUND_BODY: &[u8] = b"not found\n";
const BAD_REQUEST_BODY: &[u8] = b"bad request\n";
const CANARY_BODY: &[u8] = include_bytes!("canary.txt");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Response {
    status: u16,
    reason: &'static str,
    body: &'static [u8],
}

impl Response {
    const fn new(status: u16, reason: &'static str, body: &'static [u8]) -> Self {
        Self {
            status,
            reason,
            body,
        }
    }
}

const ROOT_RESPONSE: Response = Response::new(200, "OK", ROOT_BODY);
const HEALTH_RESPONSE: Response = Response::new(200, "OK", HEALTH_BODY);
const CANARY_RESPONSE: Response = Response::new(200, "OK", CANARY_BODY);
const NOT_FOUND_RESPONSE: Response = Response::new(404, "Not Found", NOT_FOUND_BODY);
const BAD_REQUEST_RESPONSE: Response = Response::new(400, "Bad Request", BAD_REQUEST_BODY);

fn main() {
    let port = match port_from_environment() {
        Ok(port) => port,
        Err(message) => {
            eprintln!("{message}");
            process::exit(2);
        }
    };

    if let Err(error) = serve(port) {
        eprintln!("server stopped: {error}");
        process::exit(1);
    }
}

fn port_from_environment() -> Result<u16, &'static str> {
    match env::var("PORT") {
        Ok(value) => port_from_environment_value(Some(&value)),
        Err(env::VarError::NotPresent) => port_from_environment_value(None),
        Err(env::VarError::NotUnicode(_)) => Err("PORT must contain valid UTF-8"),
    }
}

fn port_from_environment_value(value: Option<&str>) -> Result<u16, &'static str> {
    value.map_or(Ok(DEFAULT_PORT), parse_port_value)
}

fn parse_port_value(value: &str) -> Result<u16, &'static str> {
    if value.is_empty() {
        return Ok(DEFAULT_PORT);
    }
    value
        .parse::<u16>()
        .map_err(|_| "PORT must be an integer from 0 to 65535")
}

fn serve(port: u16) -> io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    let address = listener.local_addr()?;
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
    let _workers = spawn_connection_workers(receiver)?;
    eprintln!("resilis-status-canary listening on {address}");

    for connection in listener.incoming() {
        match connection {
            Ok(stream) => sender.send(stream).map_err(|error| {
                io::Error::new(
                    ErrorKind::BrokenPipe,
                    format!("connection worker queue closed: {error}"),
                )
            })?,
            Err(error) => eprintln!("connection accept error: {error}"),
        }
    }

    Ok(())
}

fn spawn_connection_workers(
    receiver: mpsc::Receiver<TcpStream>,
) -> io::Result<Vec<thread::JoinHandle<()>>> {
    let receiver = Arc::new(Mutex::new(receiver));
    let mut workers = Vec::with_capacity(WORKER_COUNT);

    for index in 0..WORKER_COUNT {
        let receiver = Arc::clone(&receiver);
        let worker = thread::Builder::new()
            .name(format!("canary-worker-{index}"))
            .stack_size(WORKER_STACK_SIZE)
            .spawn(move || loop {
                let stream = {
                    let receiver = match receiver.lock() {
                        Ok(receiver) => receiver,
                        Err(_) => return,
                    };
                    match receiver.recv() {
                        Ok(stream) => stream,
                        Err(_) => return,
                    }
                };

                if let Err(error) = serve_connection(stream) {
                    if !matches!(
                        error.kind(),
                        ErrorKind::TimedOut
                            | ErrorKind::WouldBlock
                            | ErrorKind::UnexpectedEof
                            | ErrorKind::ConnectionReset
                    ) {
                        eprintln!("connection error: {error}");
                    }
                }
            })?;
        workers.push(worker);
    }

    Ok(workers)
}

fn serve_connection(mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(CONNECTION_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECTION_TIMEOUT))?;

    let response = match read_request(&mut stream) {
        Ok(Some(request)) => response_for_request(&request),
        Ok(None) => return Ok(()),
        Err(error) if error.kind() == ErrorKind::InvalidData => BAD_REQUEST_RESPONSE,
        Err(error) => return Err(error),
    };

    stream.write_all(&serialize_response(response))
}

fn read_request(stream: &mut TcpStream) -> io::Result<Option<Vec<u8>>> {
    let mut request = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];

    loop {
        let bytes_read = stream.read(&mut buffer)?;
        if bytes_read == 0 {
            return if request.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "request ended before its headers were complete",
                ))
            };
        }

        if request.len() + bytes_read > MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "request headers are too large",
            ));
        }

        request.extend_from_slice(&buffer[..bytes_read]);
        if header_end(&request).is_some() {
            return Ok(Some(request));
        }
    }
}

fn response_for_request(request: &[u8]) -> Response {
    let parsed = match parse_request(request) {
        Ok(parsed) => parsed,
        Err(()) => return BAD_REQUEST_RESPONSE,
    };

    if parsed.method != "GET" {
        return NOT_FOUND_RESPONSE;
    }

    response_for_path(parsed.target)
}

fn response_for_path(path: &str) -> Response {
    match path {
        "/" => ROOT_RESPONSE,
        "/healthz" => HEALTH_RESPONSE,
        "/canary" => CANARY_RESPONSE,
        _ => NOT_FOUND_RESPONSE,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedRequest<'a> {
    method: &'a str,
    target: &'a str,
}

fn parse_request(request: &[u8]) -> Result<ParsedRequest<'_>, ()> {
    header_end(request).ok_or(())?;
    let line_end = request
        .windows(2)
        .position(|window| window == b"\r\n")
        .ok_or(())?;
    let line = std::str::from_utf8(&request[..line_end]).map_err(|_| ())?;
    let mut fields = line.split_ascii_whitespace();
    let method = fields.next().ok_or(())?;
    let target = fields.next().ok_or(())?;
    let version = fields.next().ok_or(())?;

    if fields.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(());
    }

    Ok(ParsedRequest { method, target })
}

fn header_end(request: &[u8]) -> Option<usize> {
    request.windows(4).position(|window| window == b"\r\n\r\n")
}

fn serialize_response(response: Response) -> Vec<u8> {
    let mut serialized = Vec::with_capacity(192 + response.body.len());
    write!(
        &mut serialized,
        "HTTP/1.1 {} {}\r\ncache-control: no-store\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response.status,
        response.reason,
        response.body.len()
    )
    .expect("writing a response to memory cannot fail");
    serialized.extend_from_slice(response.body);
    serialized
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;

    #[test]
    fn route_contract_returns_expected_status_and_body() {
        let cases = [
            ("/", ROOT_RESPONSE),
            ("/healthz", HEALTH_RESPONSE),
            ("/canary", CANARY_RESPONSE),
            ("/unknown", NOT_FOUND_RESPONSE),
            ("/healthz?probe=1", NOT_FOUND_RESPONSE),
        ];

        for (path, expected) in cases {
            let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
            assert_eq!(response_for_request(request.as_bytes()), expected, "{path}");
        }
    }

    #[test]
    fn response_serialization_sets_required_headers() {
        for response in [
            ROOT_RESPONSE,
            HEALTH_RESPONSE,
            CANARY_RESPONSE,
            NOT_FOUND_RESPONSE,
        ] {
            let serialized = serialize_response(response);
            let separator = serialized
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("serialized responses have headers");
            let headers = &serialized[..separator];

            assert!(headers
                .windows(b"cache-control: no-store".len())
                .any(|window| window == b"cache-control: no-store"));
            assert!(headers
                .windows(b"content-type: text/plain; charset=utf-8".len())
                .any(|window| window == b"content-type: text/plain; charset=utf-8"));
            assert!(serialized.ends_with(response.body));
        }
    }

    #[test]
    fn malformed_and_non_get_requests_do_not_expose_routes() {
        assert_eq!(
            response_for_request(b"GET / HTTP/1.1\r\n\r\n"),
            ROOT_RESPONSE
        );
        assert_eq!(
            response_for_request(b"POST /canary HTTP/1.1\r\n\r\n"),
            NOT_FOUND_RESPONSE
        );
        assert_eq!(
            response_for_request(b"GET / HTTP/2\r\n\r\n"),
            BAD_REQUEST_RESPONSE
        );
        assert_eq!(
            response_for_request(b"not an HTTP request"),
            BAD_REQUEST_RESPONSE
        );
    }

    #[test]
    fn port_defaults_when_unset_or_empty() {
        assert_eq!(port_from_environment_value(None), Ok(DEFAULT_PORT));
        assert_eq!(port_from_environment_value(Some("")), Ok(DEFAULT_PORT));
    }

    #[test]
    fn port_accepts_valid_values() {
        for value in ["0", "1", "8080", "65535"] {
            let expected = value.parse::<u16>().expect("test port is valid");
            assert_eq!(port_from_environment_value(Some(value)), Ok(expected));
        }
    }

    #[test]
    fn port_rejects_invalid_values() {
        for value in [" ", "-1", "65536", "8080extra", "not-a-port"] {
            assert!(
                port_from_environment_value(Some(value)).is_err(),
                "{value:?}"
            );
        }
    }

    #[test]
    fn worker_pool_uses_a_fixed_number_of_workers() {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let workers = spawn_connection_workers(receiver).expect("spawn connection workers");
        assert_eq!(workers.len(), WORKER_COUNT);

        drop(sender);
        for worker in workers {
            worker.join().expect("join connection worker");
        }
    }

    #[test]
    fn network_handler_writes_the_http_contract() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
        let address = listener.local_addr().expect("read test listener address");
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept test connection");
            serve_connection(stream).expect("serve test connection");
        });

        let mut client = TcpStream::connect(address).expect("connect test client");
        client
            .write_all(b"GET /canary HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write test request");
        client
            .shutdown(Shutdown::Write)
            .expect("close test request");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .expect("read test response");
        worker.join().expect("join test server");

        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with(CANARY_BODY));
        assert!(response
            .windows(b"cache-control: no-store".len())
            .any(|window| window == b"cache-control: no-store"));
    }
}
