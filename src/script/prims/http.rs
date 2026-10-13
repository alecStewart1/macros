//! HTTP primitives: streaming and one-shot client requests, the loopback
//! server, URL decoding and open-url.

use std::sync::{Arc, Mutex};

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    let async_ids = &ctx.ids;
    reg.section(Section::Http);
    {
        // `(make-http-stream url headers body on-line on-done)` → a stream id
        // (used by http-stream-cancel). `headers` is a flat list of alternating
        // key/value strings: (list "x-api-key" key "anthropic-version" "..."). A
        // POST with the given JSON `body`; `on-line` is called per response line
        // (SSE `data:`/`event:` lines arrive verbatim), `on-done` with the id when
        // the stream closes. Callbacks may be "name" strings or 'name symbols.
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "make-http-stream",
            move |url: String,
                  StringList(flat): StringList,
                  body: String,
                  on_line: Callback,
                  on_done: Callback|
                  -> usize {
                let id = ids.next();
                let headers = flat
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|kv| (kv[0].clone(), kv[1].clone()))
                    .collect();
                let Callback(on_line) = on_line;
                let Callback(on_done) = on_done;
                q.push(EditorCommand::HttpStream(Box::new(HttpStreamArgs {
                    id,
                    url,
                    headers,
                    body,
                    on_line,
                    on_done,
                })));
                id as usize
            },
        );
    }
    register_unary(reg, queue, "http-stream-cancel", |id| {
        EditorCommand::HttpStreamCancel { id }
    });
    {
        // `(http-request method url headers body on-response)` → #t once queued.
        // A one-shot request of any verb ("GET"/"POST"/"PATCH"/"DELETE"); the
        // streaming `make-http-stream` is POST-only and hides the status code.
        // `headers` is a flat alternating key/value string list; `body` "" means
        // no body. `on-response` (a "name" string or 'name symbol) is called once
        // with "<status>\n<body>" — status on the first line (0 = transport
        // failure), body after the first newline. Parse it in Scheme with
        // `string->jsexpr` after splitting off the status line.
        //
        // `(http-request-full …)` takes the same arguments but delivers
        // "<status>\n<Header: value>\n…\n\n<hex-body>": the status line, every
        // response header, a blank line, then the body HEX-encoded — so you can
        // read `Link`/`ETag`/rate-limit headers and download binary content
        // (images, attachments) that the text payload would corrupt.
        for (name, full) in [("http-request", false), ("http-request-full", true)] {
            let q = queue.clone();
            reg.register_fn(
                name,
                move |method: String,
                      url: String,
                      StringList(flat): StringList,
                      body: String,
                      on_response: Callback|
                      -> bool {
                    let headers = flat
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|kv| (kv[0].clone(), kv[1].clone()))
                        .collect();
                    let Callback(on_response) = on_response;
                    q.push(EditorCommand::HttpRequest(Box::new(HttpRequestArgs {
                        method,
                        url,
                        headers,
                        body,
                        on_response,
                        full,
                    })));
                    true
                },
            );
        }
    }
    {
        // `(http-serve host port content-type body on-request)` → the bound port
        // (the handle for `http-server-stop`), or 0 if the bind failed. Pass port
        // 0 for an OS-assigned ephemeral port (an OAuth loopback wants
        // host="127.0.0.1", port=0). The socket is bound here so the port is known
        // synchronously; the accept loop runs on a worker thread. Each connection
        // gets one fixed response (status 200, the given `content-type` + `body`);
        // `on-request` is called with the request target ("/path?query") so Scheme
        // can pull the OAuth code/state out of the query string. Serves until
        // `http-server-stop` is called.
        let q = queue.clone();
        reg.register_fn(
            "http-serve",
            move |host: String,
                  Port(port): Port,
                  content_type: String,
                  body: String,
                  on_request: Callback|
                  -> usize {
                let listener = match std::net::TcpListener::bind((host.as_str(), port)) {
                    Ok(l) => l,
                    Err(_) => return 0,
                };
                let bound = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                if bound == 0 {
                    return 0;
                }
                let Callback(on_request) = on_request;
                q.push(EditorCommand::HttpServe(Box::new(HttpServeArgs {
                    listener: Arc::new(Mutex::new(Some(listener))),
                    port: bound,
                    content_type,
                    body,
                    on_request,
                })));
                bound as usize
            },
        );
    }
    register_unary(reg, queue, "http-server-stop", |Port(port): Port| {
        EditorCommand::HttpServerStop { port }
    });
    // `(url-decode s)` → `s` with %XX escapes decoded (RFC 3986; `+` stays a
    // plus — this is for URL components, not form bodies). Bytes are
    // reassembled as UTF-8 (lossy), so a percent-encoded multibyte character
    // round-trips. A malformed escape is kept literally.
    reg.register_fn("url-decode", |s: String| -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
            {
                out.push((h * 16 + l) as u8);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    });
    reg.register_fn("open-url", |url: String| -> bool {
        crate::update::open_url(&url).is_ok()
    });
}
