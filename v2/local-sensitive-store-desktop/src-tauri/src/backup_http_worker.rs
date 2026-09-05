use serde_json::json;
use std::sync::{
    mpsc::{self, SyncSender, TrySendError},
    Arc, Mutex,
};
use std::thread;
use tiny_http::{Header, Method, Request};

const WORKER_COUNT: usize = 2;
const QUEUE_CAPACITY: usize = 4;

pub(crate) struct BackupHttpWorker {
    sender: SyncSender<Request>,
}

impl BackupHttpWorker {
    pub(crate) fn start(handler: impl Fn(Request) + Send + Sync + 'static) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel::<Request>(QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        let handler = Arc::new(handler);
        for index in 0..WORKER_COUNT {
            let receiver = Arc::clone(&receiver);
            let handler = Arc::clone(&handler);
            thread::Builder::new()
                .name(format!("local-backup-http-{index}"))
                .spawn(move || loop {
                    // Drop the receiver lock before doing any file, DB or network work.
                    let request = receiver
                        .lock()
                        .ok()
                        .and_then(|receiver| receiver.recv().ok());
                    let Some(request) = request else {
                        break;
                    };
                    handler(request);
                })
                .map_err(|error| format!("backup_http_worker_start_failed:{error}"))?;
        }
        Ok(Self { sender })
    }

    pub(crate) fn dispatch_or_return(&self, request: Request) -> Option<Request> {
        if !requires_worker(request.method(), request.url()) {
            return Some(request);
        }
        match self.sender.try_send(request) {
            Ok(()) => {}
            Err(TrySendError::Full(request) | TrySendError::Disconnected(request)) => {
                let origin = crate::allowed_origin(&request);
                let mut response = crate::json_response(
                    503,
                    json!({ "ok": false, "error": "backup_busy" }),
                    &origin,
                );
                if let Ok(header) = Header::from_bytes(b"Retry-After", b"1") {
                    response.add_header(header);
                }
                let _ = request.respond(response);
            }
        }
        None
    }
}

fn requires_worker(method: &Method, url: &str) -> bool {
    if method == &Method::Options {
        return false;
    }
    let path = url.split('?').next().unwrap_or(url);
    path.starts_with("/v1/backups/") || path.starts_with("/v1/device-sync/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tiny_http::Server;

    const WAIT: Duration = Duration::from_secs(3);

    fn open_request(address: SocketAddr, method: &str, route: &str) -> TcpStream {
        let mut stream = TcpStream::connect_timeout(&address, WAIT).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.set_write_timeout(Some(WAIT)).unwrap();
        write!(stream, "{method} {route} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://127.0.0.1:8794\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        stream
    }

    fn read_response(mut stream: TcpStream) -> String {
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn backup_route_dispatch_keeps_preflight_and_ordinary_reads_on_receiver() {
        for route in [
            "/v1/backups/status",
            "/v1/backups/run?tenantId=fixture",
            "/v1/device-sync/status",
            "/v1/device-sync/run",
        ] {
            assert!(requires_worker(&Method::Get, route));
            assert!(!requires_worker(&Method::Options, route));
        }
        for route in [
            "/v1/health",
            "/v1/work-notes",
            "/v1/device-authorization/browser-link",
            "/v1/backups-other/run",
        ] {
            assert!(!requires_worker(&Method::Get, route));
        }
    }

    #[test]
    fn delayed_backups_keep_health_responsive_and_queue_capacity_bounded() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (processed_tx, processed_rx) = mpsc::channel();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let threads = Arc::new(Mutex::new(HashSet::new()));
        let handler_active = Arc::clone(&active);
        let handler_peak = Arc::clone(&peak);
        let handler_threads = Arc::clone(&threads);
        let worker = BackupHttpWorker::start(move |request| {
            let in_flight = handler_active.fetch_add(1, Ordering::SeqCst) + 1;
            handler_peak.fetch_max(in_flight, Ordering::SeqCst);
            handler_threads
                .lock()
                .unwrap()
                .insert(thread::current().id());
            started_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            let origin = crate::allowed_origin(&request);
            let _ = request.respond(crate::json_response(
                200,
                json!({ "ok": true, "syntheticBackup": true }),
                &origin,
            ));
            handler_active.fetch_sub(1, Ordering::SeqCst);
        })
        .unwrap();
        let receiver = thread::spawn(move || {
            for request in server.incoming_requests() {
                if request.url() == "/__fixture_shutdown" {
                    let _ = request.respond(crate::json_response(200, json!({ "ok": true }), "*"));
                    break;
                }
                if let Some(request) = worker.dispatch_or_return(request) {
                    let origin = crate::allowed_origin(&request);
                    let _ = request.respond(crate::json_response(
                        200,
                        json!({ "ok": true, "syntheticFastRead": true }),
                        &origin,
                    ));
                }
                processed_tx.send(()).unwrap();
            }
        });

        let mut accepted = Vec::new();
        for _ in 0..WORKER_COUNT {
            accepted.push(open_request(address, "POST", "/v1/backups/run"));
            processed_rx.recv_timeout(WAIT).unwrap();
            started_rx.recv_timeout(WAIT).unwrap();
        }
        for _ in 0..QUEUE_CAPACITY {
            accepted.push(open_request(
                address,
                "GET",
                "/v1/device-sync/status?tenantId=fixture",
            ));
            processed_rx.recv_timeout(WAIT).unwrap();
        }
        let overloaded = open_request(address, "POST", "/v1/device-sync/run");
        processed_rx.recv_timeout(WAIT).unwrap();
        let response = read_response(overloaded);
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
        assert!(response.contains("\"error\":\"backup_busy\""));
        assert!(response.to_lowercase().contains("retry-after: 1"));
        assert!(response.contains("http://127.0.0.1:8794"));

        // None of the synthetic disk/network jobs have been released yet.
        for (method, route) in [
            ("GET", "/v1/health"),
            ("GET", "/v1/work-notes"),
            ("OPTIONS", "/v1/backups/run"),
        ] {
            let fast = open_request(address, method, route);
            processed_rx.recv_timeout(WAIT).unwrap();
            assert!(read_response(fast).starts_with("HTTP/1.1 200"));
            assert_eq!(active.load(Ordering::SeqCst), WORKER_COUNT);
        }
        for _ in 0..accepted.len() {
            release_tx.send(()).unwrap();
        }
        for stream in accepted {
            assert!(read_response(stream).starts_with("HTTP/1.1 200"));
        }
        assert_eq!(peak.load(Ordering::SeqCst), WORKER_COUNT);
        assert_eq!(threads.lock().unwrap().len(), WORKER_COUNT);
        assert!(
            read_response(open_request(address, "GET", "/__fixture_shutdown"))
                .starts_with("HTTP/1.1 200")
        );
        receiver.join().unwrap();
    }
}
