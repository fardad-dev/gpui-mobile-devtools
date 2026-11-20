# Event stream, protocol 1

The app serves one port (default 9700):

| Request | Response |
|---|---|
| `GET /` | the viewer page |
| `GET /hello` | the current `hello` event, as JSON |
| `GET /ws` (WebSocket upgrade) | the event stream |

With LAN access on, every request needs `?token=<token>` (logged by the app).
WebSocket upgrades whose `Origin` is not loopback or the server's own host are
refused.

## Stream

Each WebSocket text message is one JSON object with a `type`. On connect the
app sends a `hello`, then the buffered history in order, then new events as
they happen. The app never reads messages from the viewer except close and
ping.

Every event except `hello` has a `seq`, increasing by 1 within a session. A
viewer that reconnects gets the history again and skips events with a `seq`
it has seen. A new `session` in `hello` means the app restarted; `seq` and
request `id`s start over.

Times: `ts` is milliseconds since the Unix epoch, `elapsedMs` milliseconds
since the request started.

### `hello`

```json
{"type":"hello","protocol":1,"session":"9f2c41d0a3b7e815","app":"my-app","platform":"android","arch":"aarch64","pid":12345,"devtools":"0.1.0","ts":1795212942367.3}
```

### `log`

```json
{"seq":7,"type":"log","ts":1795212942400.1,"level":"INFO","target":"my_app::home","message":"loaded 12 widgets","file":"src/home.rs","line":81,"thread":"main"}
```

`level` is `ERROR`, `WARN`, `INFO`, `DEBUG` or `TRACE`. `file`, `line` and
`thread` may be `null`.

### `http.request`

```json
{"seq":8,"type":"http.request","id":3,"ts":1795212942410.0,"method":"POST","url":"https://api.example.com/cart","headers":[["content-type","application/json"]],"body":{"text":"{\"id\":1}","size":8,"truncated":false}}
```

`body` is `null` when the request has none or it is streamed.

### `http.response`

The response head arrived.

```json
{"seq":9,"type":"http.response","id":3,"ts":1795212942490.5,"elapsedMs":80.5,"status":200,"headers":[["content-type","application/json"]]}
```

### `http.finished`

The app read the whole body (or stopped reading it).

```json
{"seq":10,"type":"http.finished","id":3,"ts":1795212942495.2,"elapsedMs":85.2,"size":128,"body":{"text":"{\"ok\":true}","size":128,"truncated":false}}
```

`size` is the full body length. `body` is `null` when nothing was kept (empty
body, or a content type that is only counted), `{"binary":true,"size":n}` when
the body isn't UTF-8 text, and otherwise text cut to `max_body_bytes`
(`truncated` tells).

### `http.failed`

```json
{"seq":11,"type":"http.failed","id":4,"ts":1795212943000.0,"elapsedMs":90.1,"error":"error sending request: dns error"}
```

`error` is `"cancelled"` when the app dropped the request before it finished.
