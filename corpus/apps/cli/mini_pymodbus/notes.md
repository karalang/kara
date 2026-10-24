# mini_pymodbus

[pymodbus](https://github.com/pymodbus-dev/pymodbus) 3.6.9, the Modbus
library for Python, ported to Kāra. Modbus is the protocol PLCs, drives,
meters and HMIs have spoken since 1979, over TCP or a serial line. The
port puts a client and a server on one link and plays requests, raw bytes
and device replies through them.

```
== A poll cycle: an HMI reads and writes a line of eight drives over RTU
server rtu multi
...
> ReadInputRegistersRequest: ReadInputRegistersRequest (0,4)
  tx 010400000004f1c9
  server got ReadInputRegistersRequest: ReadInputRegistersRequest (0,4) | address=0 count=4
  rx 01040802ba01d9019702729348
  client got ReadInputRegistersResponse: ReadInputRegistersResponse (4) | registers=[698, 473, 407, 626]
...
> GetClearModbusPlusRequest
  tx 001400000006010800150003
  server got ReadExceptionStatusRequest: ReadExceptionStatusRequest(7)
  server got GetClearModbusPlusRequest | sub=0x0015 message=3
...
> raw 11 bytes
  ! server closed the connection: struct.error: unpack requires a buffer of 2 bytes
```

## What it does

`data/jobs.txt` holds 18 jobs, 792 steps. Each job sets up a server on one
framer (Modbus/TCP, RTU or ASCII) with one slave or several, fills its
datastore, and then plays steps:

- `req`: the client builds a request, the framer wraps it (`tx`), the
  server's framer cuts requests out of the bytes, possibly split into
  pieces, and the server runs each one against its datastore. The reply
  (`rx`) goes back through the client's framer.
- `raw` sends bytes the client did not build: split MBAP segments, noise
  on a serial line, bad CRCs and LRCs, wrong lengths, other slaves'
  frames.
- `devraw` hands the client a reply from a device: exception replies,
  short frames, odd lengths, unknown function codes.
- `ident`, `counters`, `event`, `diagbits`, `slaveid` and `control` set
  and show the control block every diagnostic reads.
- `pl` runs the payload builder and decoder in each byte and word order,
  `crc` and `lrc` the two checksums, and `peek` and `reset` look at and
  restore a datastore.

The jobs cover every read and write function code; the +1 address offset
against `zero_mode`; range checks; sparse blocks with holes and frozen
layouts; a gateway with several slaves, a missing one and broadcast;
RTU's CRC framing; ASCII's LRC framing; MBAP framing; diagnostics
(function 8, every sub-function); the comm event log and exception
status; device identity (Read Device Information and Report Slave ID);
file records and the FIFO queue; decoding a device's replies; function
codes the server does not implement; requests the client cannot encode;
the payload builder and decoder; and a poll cycle of an HMI over eight
drives.

`oracle.py` drives the vendored library (`vendor/`, BSD-3-Clause, see
`vendor/LICENSE`) through the same steps and wrote `expected.out`. The
library is used as is; what the oracle adds is the bus, and a port of the
async server's `_async_execute`, which is not vendored because it needs a
transport.

## How the port works

The PDUs are one struct with a class tag (`Cls`), the way pymodbus's
classes share fields: `encode`, `decode`, `rtu_size` (the RTU frame
length a class reads off a buffer), `pdu_str` and `fields` each match on
the class. Diagnostics are one class each way, with the sub-function
class a decoder picks kept beside it, as pymodbus swaps `__class__`.
Python exceptions are an `Exc` with the type's name and message, and each
place pymodbus catches one catches the same ones here: the server decoder
lets every error through to the framer, the client decoder turns every
error into "Unable to decode", and the bus prints what the server would
have done.

The port keeps pymodbus 3.6.9's behaviour where it is surprising, since
that is what a device on the other end of the wire sees:

- The control block is a singleton. Its counters, event log, diagnostic
  register, listen-only flag and identity outlive any server, and nothing
  on the request path increments a counter.
- The files and FIFO requests define a plain `execute` that the async
  server awaits, so every one of them is a TypeError and a SlaveFailure
  reply. A diagnostic sub-function with no class has no `execute` at
  all: AttributeError, SlaveFailure.
- Over TCP a request with no data (function 7, 11, 12 or 17) is 8 bytes,
  and the MBAP reader waits for 9, so it is answered when the next packet
  arrives.
- The RTU framer sizes a diagnostic as 8 bytes whatever its data, so a
  long Return Query Data mis-frames; after one bad CRC it starts every
  later search in that read one byte in. An odd-length diagnostic reply gets an ASCII
  `0` appended before it is read as words. The FIFO reply's RTU size
  shifts its high byte by 16, and its decoder reads `count - 4` values.
- A Report Slave ID reply's identifier includes its trailing status byte,
  and a Read Device Information reply counts the characters of each
  object, not its bytes.

The datastore is an arena of slave contexts that the server context maps
slave ids into, so `slave`, `block`, `drop` and a slave id the server
refuses alias the way Python's objects do.

## Compiler gaps

None. `karac check` found two of my mistakes, and its messages said what
to write instead: a literal brace in an f-string is `{{`, not `\{`, and
`for f in frames` borrows each element, so moving one into another vector
takes `frames.into_iter()`. `karac fix` offered `.clone()` for the second;
I took `.into_iter()`, which the same message names first. With those
two edits the MIR interpreter, the LLVM backend, the ASan build and the
native binary each matched the oracle byte for byte on their first run,
with nothing on stderr.

## Scope

The port covers what the two ends of a link do with bytes. It leaves out
the transports (sockets, serial ports, TLS), the client's transaction
manager with its retries and timeouts, the server's request handler loop,
the simulator datastore and the remote slave context. The binary
(`ModbusBinaryFramer`) and TLS framers are left out too.

## Running

```bash
python3 -s -P -B mk.py                 # writes data/jobs.txt
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py              # the jobs_text() function at the end of the .kara
karac run mini_pymodbus.kara
karac build --mir mini_pymodbus.kara && ./mini_pymodbus
```

About 3.3 s on the MIR interpreter (a debug build of `karac`) and 0.03 s
built. The program writes `data/jobs.txt` and reads it back.
