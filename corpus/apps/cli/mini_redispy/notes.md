# mini_redispy

A port of the pure-Python path of [redis-py](https://github.com/redis/redis-py)
5.2.1 (MIT; Redis Inc. and contributors), the client most Python services use
to talk to Redis: Django and Flask caches, Celery and RQ queues, rate
limiters, session stores. It covers the RESP2 and RESP3 parsers over the
socket buffer, the command packer and the encoder, the connection's handshake,
the connection pool, retries, the command methods with their reply callbacks,
pipelines, MULTI/EXEC transactions with WATCH, `BITFIELD`, and `parse_url()`.
The vendored copy is the library's own source, unchanged, trimmed to the
modules `import redis` loads.

```
call zadd s:board { s:ada i:1 } nx=T: gt=T:
! DataError: Only one of 'nx', 'lt', or 'gr' may be defined.
...
pipe watch s:balance
send b'*2\r\n$5\r\nWATCH\r\n$7\r\nbalance\r\n'
= True
pipe multi
= None
call set s:balance i:80
= queued
pipe execute
send b'*1\r\n$5\r\nMULTI\r\n*3\r\n$3\r\nSET\r\n$7\r\nbalance\r\n$2\r\n80\r\n*1\r\n$4\r\nEXEC\r\n'
! WatchError: Watched variable changed.
...
url redis://app:p%2540ss%253Aword@10.0.0.5:7000
= {'username': 'app', 'password': 'p@ss:word', 'host': '10.0.0.5', 'port': 7000}
```

## What it does

The other end of the socket is a scripted server. `mk.py` writes
`data/jobs.txt`, 40 jobs. Each gives a client its options, the replies the
server sends (one unit per command it receives, or "close" the connection, or
"hang" and never answer), and the calls to make. `oracle.py` plays the jobs
through the vendored redis-py with a fake socket in place of TCP;
`mini_redispy.kara` plays them through the port. Both print every connect,
send and close, and what each call returned or raised.

- **Commands** (482 calls of 154 methods): strings and keys, `SORT`, hashes,
  lists, sets, sorted sets, streams, geo, scripts, pub/sub and the server,
  with each method's argument checks (every `DataError` it raises before
  sending) and the callback that shapes its reply: `INFO` and `CLIENT LIST`
  parsing, `ZRANGE` with scores cast by `float` or `int`, `XINFO STREAM
  FULL`, `XPENDING`, `GEORADIUS` with distances and coordinates, `ACL
  GETUSER`, `SLOWLOG GET`, `CLUSTER NODES`, `COMMAND`, `MEMORY STATS`,
  `SENTINEL MASTER` and the rest.
- **Encoding**: str, bytes, int and float arguments, a str command name
  split on whitespace, `None`, `bool` and `list` refused, and values over
  the 6000-byte cutoff sent as their own writes.
- **Replies**: every RESP2 and RESP3 type, error replies mapped to their
  exception classes, a push reply skipped, undecodable bytes with
  `decode_responses`, and protocol errors.
- **The handshake**: `AUTH` with a password or a user (and the fallback for
  servers older than 6), `HELLO 3` with and without `AUTH`, `CLIENT
  SETNAME`, `CLIENT SETINFO` refused by old servers, `SELECT`, and each way
  one fails.
- **Connections**: the server closing the socket, a timeout, a retry on
  timeout, and the pool's check for stale data on a reused connection.
- **Pipelines**: queued commands, `raise_on_error=False`, transactions with
  errors at queue time and at `EXEC`, `WATCH` and `UNWATCH`, a watched key
  that changed, and a connection lost while watching.
- **`parse_url()`** (24 URLs): users and passwords, unix sockets, `rediss`,
  query arguments with their parsers, bad ports and bad values.

The jobs keep two places where redis-py 5.2.1 itself goes wrong, since the
port matches it: under RESP3, `SENTINEL MASTER` raises `KeyError: 'name'`
because the callback looks up str keys in a map whose keys are bytes, and a
RESP3 null bulk string `$-1` reads as `read(-1)`, which waits for one more
byte and times out.

## How the port works

- **Values** are one `Val` enum standing in for Python's: None, bool, int
  (with a big-integer form for digits past i64), float, str, bytes, list,
  tuple, set, dict in insertion order, exceptions and classes. `show()` is
  `repr()`, including str and bytes quoting and float repr, and `py_eq` is
  Python's `==` with `1 == 1.0 == True`.
- **The socket buffer** follows `_parsers/socket.py`: `read(length)` asks for
  two bytes more and drops them, `readline` loops until CRLF, `purge` empties
  only a fully read buffer, and `can_read` does a non-blocking read whose
  timeout means "nothing". The RESP2 and RESP3 parsers are `resp2.py` and
  `resp3.py`, with `parse_error`'s table of error prefixes.
- **The connection and the pool** follow `connection.py`: `on_connect`'s
  order, a `RedisError` during connect disconnecting, `read_response`
  disconnecting on any error, the pool popping the newest connection, and the
  stale-data check that reconnects once and then raises "Connection not
  ready".
- **Retries** are `Retry.call_with_retry` with `NoBackoff`: zero retries, or
  one with `retry_on_timeout`, over `ConnectionError` and `TimeoutError`.
- **Commands** are built by the methods of `commands/core.py`, bound by
  position or keyword the way Python binds them, and parsed by the callback
  tables of `_parsers/helpers.py` for RESP2 and RESP3. The options a method
  passes (`withscores`, `score_cast_func`, `groups`, `full`, `justid`,
  `NEVER_DECODE`, `EMPTY_RESPONSE`) reach the callback the same way.
- **Pipelines** follow `Pipeline` in `client.py`: commands run at once while
  watching, `_execute_transaction` annotates each queued error and inserts it
  into the `EXEC` reply, `_disconnect_raise_reset` and
  `_disconnect_reset_raise` differ in whether they reset before raising, and
  `reset()` sends `UNWATCH` and swallows only a `ConnectionError`.
- **`parse_url()`** runs over a port of `urlsplit`, `parse_qs` and `unquote`
  from `urllib.parse`, with the `port` property's checks.

## Compiler gaps

None. `karac check` caught escaped quotes inside an f-string interpolation,
with a message that names the fix; `karac fix` added `#[derive(Clone)]` to the
two enums and `ref` to two patterns; and `karac check` noted that a struct
named `Url` shadows the prelude type, so it is `SplitUrl` here.

## Scope

Left out: TLS, Sentinel and Cluster clients, pub/sub listening, client-side
caching, `Lock`, Lua `Script` objects, the hiredis parser, and the asyncio
client. `LASTSAVE` (a datetime) and arguments given as `timedelta` or
`datetime` are not modelled.

## Running

```
python3 -s -P -B mk.py            # writes data/jobs.txt
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py         # embeds data/jobs.txt in mini_redispy.kara
karac run mini_redispy.kara
```
